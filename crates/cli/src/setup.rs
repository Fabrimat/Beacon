//! `beacon setup` / `beacon doctor` (9): Minecraft detection plus the three pinned, consented
//! downloads (esbuild, API index, BDS), reported as `engine::setup::SetupReport` (`schema: 1`).

use super::Opts;
use beacon_api::PINS;
use beacon_bundler::Confirm;
use beacon_engine::api_index::ApiIndex;
use beacon_engine::diag::{Code, Diagnostic, exit};
use beacon_engine::setup::{self, SetupReport, Step, StepStatus, Tool};
use beacon_fetch::{FetchError, Pinned};
use clap::ValueEnum;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ToolArg {
    Esbuild,
    ApiIndex,
    Bds,
}

impl ToolArg {
    fn tool(self) -> Tool {
        match self {
            ToolArg::Esbuild => Tool::Esbuild,
            ToolArg::ApiIndex => Tool::ApiIndex,
            ToolArg::Bds => Tool::Bds,
        }
    }
}

#[derive(clap::Args)]
pub struct SetupArgs {
    /// Tools to install (default: none, only report). Downloads need `--yes`,
    /// `BEACON_DOWNLOAD=yes` or a TTY prompt.
    #[arg(value_enum)]
    tools: Vec<ToolArg>,
    /// Report what would be installed; download nothing.
    #[arg(long)]
    dry_run: bool,
    /// Accept the Minecraft EULA for BDS (or `BEACON_BDS_EULA=accept`); logged.
    #[arg(long)]
    accept_eula: bool,
}

fn step(tool: Tool, version: &str, bytes: u64, status: StepStatus) -> Step {
    Step {
        tool,
        version: version.to_owned(),
        bytes,
        status,
        default_selected: tool != Tool::Bds,
        needs_eula: tool == Tool::Bds,
        diagnostic: None,
    }
}

fn failed(mut s: Step, d: Diagnostic) -> Step {
    s.status = StepStatus::Failed;
    s.diagnostic = Some(d);
    s
}

fn esbuild_step(tools: &Path) -> Step {
    let platform = beacon_bundler::platform();
    esbuild_step_with(tools, &platform, beacon_bundler::esbuild_pin(&platform))
}

/// [`esbuild_step`] with the pin injected (tests cover platforms without one).
fn esbuild_step_with(tools: &Path, platform: &str, pin: Option<Pinned>) -> Step {
    let platform = platform.to_owned();
    let v = beacon_bundler::ESBUILD_VERSION;
    let Some(pin) = pin else {
        return step(Tool::Esbuild, v, 0, StepStatus::Unsupported);
    };
    let s = step(Tool::Esbuild, v, pin.size, StepStatus::Installed);
    if beacon_bundler::verified_install(tools, &platform).is_some() {
        s
    } else if beacon_bundler::installed_path(tools, &platform).exists() {
        let msg = "the installed esbuild no longer matches its recorded hash; reinstall it";
        failed(s, Diagnostic::new(Code::DownloadHashMismatch, msg))
    } else {
        Step {
            status: StepStatus::Missing,
            ..s
        }
    }
}

fn api_step() -> Step {
    // the newest pinned game version (a project's own version is `beacon api install`'s job)
    let vp = PINS.last().expect("api pins");
    let bytes = vp.packages.iter().map(|(_, _, p)| p.size).sum();
    let status = if ApiIndex::load(vp.game_version).is_ok() {
        StepStatus::Installed
    } else {
        StepStatus::Missing
    };
    step(Tool::ApiIndex, vp.game_version, bytes, status)
}

fn bds_step(tools: &Path) -> Step {
    bds_step_with(tools, beacon_bds::pin())
}

/// [`bds_step`] with the pin injected (tests cover platforms without one).
fn bds_step_with(tools: &Path, pin: Option<Pinned>) -> Step {
    let v = beacon_bds::VERSION;
    let Some(pin) = pin else {
        return step(Tool::Bds, v, 0, StepStatus::Unsupported);
    };
    let s = step(Tool::Bds, v, pin.size, StepStatus::Installed);
    let zip = tools.join(pin.blake3);
    if !zip.is_file() {
        return Step {
            status: StepStatus::Missing,
            ..s
        };
    }
    let mut h = blake3::Hasher::new();
    let ok = std::fs::File::open(&zip)
        .ok()
        .and_then(|f| h.update_reader(f).ok())
        .is_some()
        && h.finalize().to_hex().as_str() == pin.blake3;
    if ok {
        s
    } else {
        let msg = "the cached BDS download no longer matches its pin; delete it and reinstall";
        failed(s, Diagnostic::new(Code::DownloadHashMismatch, msg))
    }
}

fn status(tools: &Path) -> Vec<Step> {
    vec![esbuild_step(tools), api_step(), bds_step(tools)]
}

/// `Declined` when the download was not confirmed, else `Failed` with the diagnostic.
fn settle(s: &mut Step, r: Result<(), Diagnostic>) {
    match r {
        Ok(()) => s.status = StepStatus::Installed,
        Err(d) => {
            s.status = if d.code == Code::DownloadNeedsConfirm {
                StepStatus::Declined
            } else {
                StepStatus::Failed
            };
            s.diagnostic = Some(d);
        }
    }
}

fn install(o: &Opts, tools: &Path, eula: Option<&'static str>, s: &mut Step) {
    if s.status == StepStatus::Unsupported {
        let msg = format!("no pinned {:?} download for this platform", s.tool);
        let code = if s.tool == Tool::Esbuild {
            Code::BundlerMissing
        } else {
            Code::BdsFailed
        };
        *s = failed(s.clone(), Diagnostic::new(code, msg));
        return;
    }
    // present, or a pin problem that a plain download must not paper over (doctor says why)
    if matches!(s.status, StepStatus::Installed | StepStatus::Failed) {
        return;
    }
    let confirm = if o.download {
        Confirm::Yes
    } else {
        Confirm::Tty
    };
    match s.tool {
        Tool::Esbuild => {
            let r = beacon_bundler::install_pinned(tools, &beacon_bundler::platform(), confirm);
            settle(s, r.map(drop));
        }
        Tool::ApiIndex => {
            let g = PINS.last().expect("api pins").game_version;
            settle(s, beacon_api::ensure(g, confirm).map(drop));
        }
        Tool::Bds => {
            let Some(method) = eula else {
                s.status = StepStatus::Declined;
                s.diagnostic = Some(Diagnostic::new(
                    Code::BdsEulaRequired,
                    format!(
                        "BDS use requires accepting the Minecraft EULA ({}): \
                         pass --accept-eula or set BEACON_BDS_EULA=accept",
                        beacon_bds::EULA_URL
                    ),
                ));
                return;
            };
            let r = beacon_bds::fetch(tools, confirm).map_err(|e| match e {
                FetchError::NeedsConfirm => Diagnostic::new(
                    Code::DownloadNeedsConfirm,
                    format!(
                        "BDS {} must be downloaded ({} bytes, blake3-pinned); \
                         pass --yes or set BEACON_DOWNLOAD=yes to allow it",
                        s.version, s.bytes
                    ),
                ),
                FetchError::HashMismatch { .. } => {
                    Diagnostic::new(Code::DownloadHashMismatch, e.to_string())
                }
                e => Diagnostic::new(Code::DownloadFailed, e.to_string()),
            });
            // the acceptance is logged only once the confirmed download went through
            let r = r.and_then(|_| {
                beacon_bds::log_eula(tools, method).map_err(|e| {
                    let msg = format!("cannot write the EULA log: {e}");
                    Diagnostic::new(Code::BdsFailed, msg)
                })
            });
            settle(s, r);
        }
    }
}

fn mib(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

/// Prints the report (JSON with a report-level `diagnostics` list, or text); returns the exit code.
fn print(o: &Opts, r: &SetupReport) -> i32 {
    let mut diags: Vec<Diagnostic> = r
        .steps
        .iter()
        .filter_map(|s| s.diagnostic.clone())
        .collect();
    if r.installs.is_empty() {
        diags.push(Diagnostic::new(
            Code::MinecraftNotFound,
            "No Minecraft install found. Start Minecraft once, sign in and reach the main menu, then retry.",
        ));
    }
    if o.json {
        let mut v = serde_json::to_value(r).expect("report serializes");
        v["diagnostics"] = serde_json::to_value(&diags).expect("diagnostics serialize");
        println!("{v}");
    } else {
        for i in &r.installs {
            let (p, l) = (i.product, i.layout);
            println!("minecraft {p:?} ({l:?}): {}", i.com_mojang.display());
        }
        for s in &r.steps {
            let (t, v, st) = (s.tool, &s.version, s.status);
            println!("{t:?} {v} ({}): {st:?}", mib(s.bytes));
        }
        crate::print_diags(&diags);
    }
    if diags.iter().any(|d| d.code == Code::BdsEulaRequired) {
        exit::USAGE
    } else if r.steps.iter().any(|s| s.status == StepStatus::Failed) {
        exit::DIAGNOSTICS
    } else {
        exit::OK
    }
}

fn tools_dir() -> Result<PathBuf, i32> {
    beacon_fetch::default_dir().ok_or_else(|| {
        eprintln!("error: no tools dir (set BEACON_HOME)");
        exit::USAGE
    })
}

pub fn run(o: &Opts, a: &SetupArgs) -> i32 {
    let tools = match tools_dir() {
        Ok(t) => t,
        Err(c) => return c,
    };
    let mut steps = status(&tools);
    if !a.dry_run {
        let env = std::env::var("BEACON_BDS_EULA").ok();
        let eula = beacon_bds::eula_method(a.accept_eula, env.as_deref());
        for s in &mut steps {
            if a.tools.iter().any(|t| t.tool() == s.tool) {
                install(o, &tools, eula, s);
            }
        }
    }
    print(
        o,
        &SetupReport {
            schema: setup::SCHEMA,
            installs: setup::detect(),
            steps,
        },
    )
}

/// `beacon doctor`: the same report, nothing installed; a failed pin check exits 1.
pub fn doctor(o: &Opts) -> i32 {
    let tools = match tools_dir() {
        Ok(t) => t,
        Err(c) => return c,
    };
    print(
        o,
        &SetupReport {
            schema: setup::SCHEMA,
            installs: setup::detect(),
            steps: status(&tools),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpinned_platform_is_unsupported_not_failed() {
        let t = tempfile::tempdir().unwrap();
        let b = bds_step_with(t.path(), None);
        let e = esbuild_step_with(t.path(), "plan9-x64", None);
        for s in [&b, &e] {
            assert_eq!(s.status, StepStatus::Unsupported);
            assert!(s.diagnostic.is_none());
        }
        // report-only: exit 0
        let o = Opts {
            project: ".".into(),
            json: true,
            yes: false,
            download: false,
        };
        let r = SetupReport {
            schema: setup::SCHEMA,
            installs: vec![],
            steps: vec![e.clone(), b.clone()],
        };
        assert_eq!(print(&o, &r), exit::OK);
        // explicitly requested: failed
        let mut b = b;
        install(&o, t.path(), None, &mut b);
        assert_eq!(b.status, StepStatus::Failed);
        assert!(b.diagnostic.is_some());
    }
}
