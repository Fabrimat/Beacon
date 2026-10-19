//! First-run wizard commands (9): the bundled `beacon setup --json` sidecar, plus the one-time
//! copy of the esbuild the installer ships (decision 6: the 5-minute path has no download).

use super::{AppCore, AppError, AppResult, Progress};
use beacon_engine::diag::Code;
use beacon_engine::setup::{SetupReport, Tool};
use std::path::{Path, PathBuf};
use std::process::Command;

/// blake3 of the **extracted** `esbuild.exe` of the pinned `@esbuild/win32-x64` tarball. The
/// bundler pin (`crates/bundler/src/pins.rs`) covers the *tarball*, not the binary, so it
/// cannot vouch for a loose file; this is the hash of the member of that pinned tarball
/// (what `download::install` writes to the `.blake3` sidecar). Regenerate on an esbuild bump:
/// `beacon setup esbuild --yes` with a fresh `BEACON_HOME`, then read the sidecar.
const SHIPPED_WIN32_X64: &str = "7a8e9ab3a516c3a0d6645deb4db8703f4e58eb3653a3d74a879588fd2aed0381";

/// Largest binary the first-launch copy accepts (esbuild is ~10 MB).
const MAX_SHIPPED: u64 = 64 << 20;

/// `esbuild[.exe]` next to the `beacon` sidecar (Tauri strips the triple suffix).
fn shipped_path(beacon_exe: &Path) -> PathBuf {
    beacon_exe.with_file_name(if cfg!(windows) {
        "esbuild.exe"
    } else {
        "esbuild"
    })
}

fn shipped_hash(platform: &str) -> Option<&'static str> {
    (platform == "win32-x64").then_some(SHIPPED_WIN32_X64)
}

/// Copies `shipped` into `tools` as the bundler's installed esbuild (+ `.blake3` sidecar) only if
/// its blake3 is `want`. The bytes hashed are the bytes written (one read), via temp + rename.
/// `Ok(Some(path))`: installed (or already installed and verified: nothing touched);
/// `Ok(None)`: nothing shipped; `Err`: hash mismatch (nothing written) or an I/O failure.
fn install_shipped(
    shipped: &Path,
    tools: &Path,
    platform: &str,
    want: &str,
) -> Result<Option<PathBuf>, String> {
    let io = |e: std::io::Error| e.to_string();
    if let Some(p) = beacon_bundler::verified_install(tools, platform) {
        return Ok(Some(p));
    }
    let len = match std::fs::metadata(shipped) {
        Ok(m) if m.is_file() => m.len(),
        _ => return Ok(None),
    };
    if len > MAX_SHIPPED {
        return Err(format!("shipped esbuild is {len} bytes, refusing it"));
    }
    let bytes = std::fs::read(shipped).map_err(io)?;
    let got = blake3::hash(&bytes).to_hex().to_string();
    if got != want {
        return Err(format!("shipped esbuild hash {got} does not match its pin"));
    }
    std::fs::create_dir_all(tools).map_err(io)?;
    let dest = beacon_bundler::installed_path(tools, platform);
    let side = PathBuf::from(format!("{}.blake3", dest.display()));
    let tmp = tools.join(".esbuild-first-launch");
    let tmp_side = tools.join(".esbuild-first-launch.blake3");
    std::fs::write(&tmp, &bytes).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    std::fs::write(&tmp_side, &got).map_err(io)?;
    // binary first: without its sidecar it is "not verified" and the next launch redoes this
    std::fs::rename(&tmp, &dest).map_err(io)?;
    std::fs::rename(&tmp_side, &side).map_err(io)?;
    Ok(Some(dest))
}

impl AppCore {
    /// [`install_shipped`] with the expected hash injected (tests; the app uses `first_launch`).
    pub fn install_shipped_esbuild_with(
        shipped: &Path,
        tools: &Path,
        platform: &str,
        want: &str,
    ) -> Result<Option<PathBuf>, String> {
        install_shipped(shipped, tools, platform, want)
    }

    /// First launch: put the installer's esbuild into the tools dir (see [`SHIPPED_WIN32_X64`]).
    /// Never fails the app: a problem is logged and Play falls back to the wizard's download.
    pub fn first_launch(self) -> AppCore {
        let platform = beacon_bundler::platform();
        if let (Some(tools), Some(want)) = (beacon_fetch::default_dir(), shipped_hash(&platform))
            && let Err(e) =
                install_shipped(&shipped_path(&self.beacon_exe), &tools, &platform, want)
        {
            eprintln!("beacon: shipped esbuild not installed: {e}");
        }
        self
    }

    fn run_setup(&self, args: &[&str]) -> AppResult<SetupReport> {
        let mut cmd = Command::new(&self.beacon_exe);
        cmd.arg("setup").args(args).arg("--json");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let fail = |m: String| AppError::new(Code::DownloadFailed, &m);
        let out = cmd
            .output()
            .map_err(|e| fail(format!("cannot run beacon setup: {e}")))?;
        // exit 1 (a failed step) still prints a full report; only unparsable output is an error
        serde_json::from_slice(&out.stdout).map_err(|e| {
            fail(format!(
                "beacon setup gave no report ({e}): {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        })
    }

    /// `beacon setup --json --dry-run`: installs + steps, nothing downloaded.
    pub fn setup_status(&self) -> AppResult<SetupReport> {
        self.run_setup(&["--dry-run"])
    }

    /// Downloads exactly `accepted` (the user's selection); BDS also needs `accept_eula`,
    /// otherwise it is not requested at all.
    pub fn setup_run(&self, accepted: &[Tool], accept_eula: bool) -> AppResult<SetupReport> {
        self.setup_run_with(accepted, accept_eula, &mut |_| {})
    }

    /// [`Self::setup_run`], reporting `beacon://progress` payloads to `progress`.
    // ponytail: coarse (one event per tool, then done); the sidecar prints only at the end.
    pub fn setup_run_with(
        &self,
        accepted: &[Tool],
        accept_eula: bool,
        progress: &mut dyn FnMut(Progress),
    ) -> AppResult<SetupReport> {
        let mut args = vec![];
        for (t, name) in [
            (Tool::Esbuild, "esbuild"),
            (Tool::ApiIndex, "api-index"),
            (Tool::Bds, "bds"),
        ] {
            if accepted.contains(&t) && (t != Tool::Bds || accept_eula) {
                args.push(name);
            }
        }
        if args.is_empty() {
            return self.setup_status();
        }
        let mut ev = |step: &str, percent| {
            progress(Progress {
                task: "setup".into(),
                step: step.into(),
                percent: Some(percent),
            })
        };
        for a in &args {
            ev(a, 0);
        }
        args.push("--yes");
        if accept_eula && accepted.contains(&Tool::Bds) {
            args.push("--accept-eula");
        }
        let r = self.run_setup(&args);
        ev("done", 100);
        r
    }
}

#[cfg(test)]
mod tests {
    /// Bumping esbuild changes the extracted binary: regenerate `SHIPPED_WIN32_X64` first.
    #[test]
    fn shipped_hash_was_generated_for_this_esbuild_version() {
        assert_eq!(beacon_bundler::ESBUILD_VERSION, "0.28.2");
    }
}
