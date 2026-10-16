//! Download the pinned esbuild tarball (via `beacon-fetch`), extract exactly one member, check
//! that the binary says the pinned version, and install it as `<tools>/esbuild-<v>-<platform>`.
//! Executes the binary (`--version`): fine, it is hash-verified and the session is trusted.

use crate::pins::{self, VERSION};
use beacon_engine::diag::{Code, Diagnostic};
use beacon_fetch::{Confirm, FetchError};
use flate2::read::GzDecoder;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

const MAX_BINARY: u64 = 200 << 20; // esbuild is ~10 MB; guards against a gzip bomb

/// `<tools>/esbuild-<version>-<platform>[.exe]`
pub fn installed_path(tools: &Path, platform: &str) -> PathBuf {
    tools.join(format!(
        "esbuild-{VERSION}-{platform}{}",
        if platform.starts_with("win32") {
            ".exe"
        } else {
            ""
        }
    ))
}

/// The installed binary, if it still matches the blake3 recorded beside it at install time
/// (`<binary>.blake3`). Same-user limit: anyone who can write the tools dir can rewrite both
/// files; this only catches corruption and casual tampering, not a same-user attacker.
pub fn verified(tools: &Path, platform: &str) -> Option<PathBuf> {
    let exe = installed_path(tools, platform);
    let want = std::fs::read_to_string(sidecar(&exe)).ok()?;
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(&exe).ok()?).ok()?;
    (h.finalize().to_hex().as_str() == want.trim()).then_some(exe)
}

fn sidecar(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".blake3");
    PathBuf::from(s)
}

/// Fetches (cached, hash-verified) and installs the pinned esbuild; returns its path.
pub fn install(tools: &Path, platform: &str, confirm: Confirm) -> Result<PathBuf, Diagnostic> {
    let pin = pins::pin(platform).ok_or_else(|| {
        Diagnostic::new(
            Code::BundlerMissing,
            format!("no pinned esbuild for {platform}; set BEACON_ESBUILD"),
        )
    })?;
    let fail = |m: String| Diagnostic::new(Code::DownloadFailed, m);
    let tgz = beacon_fetch::fetch(&pin, tools, confirm).map_err(|e| match e {
        FetchError::NeedsConfirm => Diagnostic::new(
            Code::DownloadNeedsConfirm,
            format!(
                "esbuild {VERSION} must be downloaded from {} ({} bytes, blake3-pinned); \
                 pass --yes or set BEACON_DOWNLOAD=yes to allow it",
                pin.url, pin.size
            ),
        ),
        FetchError::HashMismatch { .. } => {
            Diagnostic::new(Code::DownloadHashMismatch, e.to_string())
        }
        e => fail(e.to_string()),
    })?;
    let tmp = extract(&tgz, platform, tools).map_err(|e| fail(format!("{}: {e}", pin.url)))?;
    let out = Command::new(&tmp)
        .arg("--version")
        .output()
        .map_err(|e| fail(format!("cannot run the downloaded esbuild: {e}")))?;
    let got = String::from_utf8_lossy(&out.stdout);
    if got.trim() != VERSION {
        return Err(fail(format!(
            "downloaded esbuild reports {:?}, expected {VERSION}",
            got.trim()
        )));
    }
    let dest = installed_path(tools, platform);
    let mut h = blake3::Hasher::new();
    h.update_reader(std::fs::File::open(&tmp).map_err(|e| fail(e.to_string()))?)
        .map_err(|e| fail(e.to_string()))?;
    tmp.persist(&dest).map_err(|e| fail(e.to_string()))?;
    std::fs::write(sidecar(&dest), h.finalize().to_hex().as_str())
        .map_err(|e| fail(e.to_string()))?;
    Ok(dest)
}

/// Writes the one expected member of the tarball to a temp file in `dir` (mode 0o755). Every
/// member is inspected: any link, absolute path or `..` rejects the whole archive.
pub(crate) fn extract(
    tgz: &Path,
    platform: &str,
    dir: &Path,
) -> Result<tempfile::TempPath, String> {
    let win = platform.starts_with("win32");
    let want = if win {
        "package/esbuild.exe"
    } else {
        "package/bin/esbuild"
    };
    let file = std::fs::File::open(tgz).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(GzDecoder::new(file));
    let mut found = None;
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let ty = entry.header().entry_type();
        let unsafe_path = name.starts_with(['/', '\\'])
            || name.as_bytes().get(1) == Some(&b':')
            || name.split(['/', '\\']).any(|c| c == "..");
        if ty.is_symlink() || ty.is_hard_link() || unsafe_path {
            return Err(format!("unsafe archive member {name:?}"));
        }
        if name != want || !ty.is_file() || found.is_some() {
            continue;
        }
        let mut f = tempfile::Builder::new()
            .prefix(".esbuild-")
            .suffix(if win { ".exe" } else { "" })
            .tempfile_in(dir)
            .map_err(|e| e.to_string())?;
        let n =
            io::copy(&mut (&mut entry).take(MAX_BINARY + 1), &mut f).map_err(|e| e.to_string())?;
        if n > MAX_BINARY {
            return Err("esbuild member is implausibly large".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
        found = Some(f.into_temp_path()); // closes the handle: a file open for write can't be run
    }
    found.ok_or_else(|| format!("{want} not found in the archive"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    type Member<'a> = (&'a str, tar::EntryType, &'a [u8]);

    /// A tgz of members; names are written raw (the tar builder would refuse `..`).
    fn tgz(members: &[Member]) -> tempfile::NamedTempFile {
        let mut b = tar::Builder::new(Vec::new());
        for (name, ty, body) in members {
            let mut h = tar::Header::new_gnu();
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            h.set_entry_type(*ty);
            h.set_size(body.len() as u64);
            if ty.is_symlink() || ty.is_hard_link() {
                h.set_link_name("/etc/passwd").unwrap();
            }
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, *body).unwrap();
        }
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&b.into_inner().unwrap()).unwrap();
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(&z.finish().unwrap()).unwrap();
        f
    }

    const FILE: tar::EntryType = tar::EntryType::Regular;

    #[test]
    fn extracts_only_the_expected_member() {
        let d = tempfile::tempdir().unwrap();
        let t = tgz(&[
            ("package/README.md", FILE, b"hi"),
            ("package/bin/esbuild", FILE, b"BIN"),
            ("package/bin/esbuild2", FILE, b"no"),
        ]);
        let p = extract(t.path(), "linux-x64", d.path()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"BIN");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
        drop(p);
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
        let t = tgz(&[("package/esbuild.exe", FILE, b"EXE")]);
        assert!(extract(t.path(), "win32-x64", d.path()).is_ok());
        // wrong member for the platform
        assert!(extract(t.path(), "linux-x64", d.path()).is_err());
    }

    #[test]
    fn rejects_malicious_archives_and_writes_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let d = parent.path().join("tools");
        std::fs::create_dir(&d).unwrap();
        let bin: Member = ("package/bin/esbuild", FILE, b"BIN");
        let link = tar::EntryType::Symlink;
        let cases: Vec<(&str, Vec<Member>)> = vec![
            ("dotdot", vec![bin, ("../evil", FILE, b"x")]),
            ("dotdot-deep", vec![bin, ("package/../../evil", FILE, b"x")]),
            ("abs", vec![bin, ("/tmp/evil", FILE, b"x")]),
            ("backslash-abs", vec![bin, ("\\evil", FILE, b"x")]),
            ("drive", vec![bin, ("C:evil", FILE, b"x")]),
            ("symlink", vec![bin, ("package/link", link, b"")]),
            (
                "hardlink",
                vec![bin, ("package/hl", tar::EntryType::Link, b"")],
            ),
            (
                "expected-is-symlink",
                vec![("package/bin/esbuild", link, b"")],
            ),
            ("expected-missing", vec![("package/README.md", FILE, b"x")]),
        ];
        for (what, members) in cases {
            let t = tgz(&members);
            assert!(extract(t.path(), "linux-x64", &d).is_err(), "{what}");
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0, "{what}");
            assert!(!parent.path().join("evil").exists(), "{what}");
        }
    }

    /// Network: the real pinned tarball of the host platform downloads, verifies, extracts and
    /// runs. `cargo test -p beacon-bundler -- --ignored real_pinned_download`
    #[test]
    #[ignore]
    fn real_pinned_download() {
        let d = tempfile::tempdir().unwrap();
        let platform = pins::platform();
        let exe = install(d.path(), &platform, Confirm::Yes).unwrap();
        assert_eq!(exe, installed_path(d.path(), &platform));
        assert_eq!(verified(d.path(), &platform), Some(exe.clone()));
        std::fs::write(&exe, b"tampered").unwrap();
        assert!(verified(d.path(), &platform).is_none());
        // re-extracted from the cached tarball, no confirmation needed
        let exe = install(d.path(), &platform, Confirm::No).unwrap();
        assert!(verified(d.path(), &platform).is_some());
        let v = Command::new(&exe).arg("--version").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&v.stdout).trim(), VERSION);
        // the tarball is cached by hash: a second install needs no confirmation or network
        assert!(install(d.path(), &platform, Confirm::No).is_ok());
    }
}
