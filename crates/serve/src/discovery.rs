//! Daemon discovery (3): a JSON file per project in the per-user runtime directory, never in
//! the project. The exclusive lock file next to it is held for the daemon's whole lifetime:
//! a discovery file whose lock can be taken is stale, whatever its pid says.
//!
//! - Windows: `%LOCALAPPDATA%\beacon\run\`; pipe `\\.\pipe\beacon-<key>-<random>` created with
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS` and an owner-only DACL;
//!   the random part (only in the owner-only discovery file) stops name squatting.
//! - macOS: `~/Library/Caches/beacon/run/`; Linux: `$XDG_RUNTIME_DIR/beacon/` (else the cache
//!   dir); socket `<run dir>/<key>.sock`, directory mode 0700. Socket paths over the
//!   `sun_path` limit (104 bytes on macOS) fall back to a short path under `/tmp/beacon-<uid>/`.
//! - `BEACON_HOME` replaces the base dir (`<BEACON_HOME>/run`), for tests.

use beacon_engine::build::canon;
use std::io;
use std::path::{Path, PathBuf};

/// Contents of `<run dir>/<key>.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    /// Pipe name (Windows) or socket path.
    pub pipe: String,
    pub pid: u32,
    pub engine: String,
    pub protocol: u32,
    /// Canonical project root the daemon serves (checked against the key's input).
    pub project: String,
    /// Phase 5 (app webview): loopback port + bearer token, owner-only like the file itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Per-user runtime directory for discovery, lock and socket files.
pub fn run_dir() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("BEACON_HOME") {
        return Some(PathBuf::from(h).join("run"));
    }
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("beacon").join("run"))
    } else if cfg!(target_os = "macos") {
        dirs::cache_dir().map(|d| d.join("beacon").join("run"))
    } else {
        dirs::runtime_dir()
            .map(|d| d.join("beacon"))
            .or_else(|| dirs::cache_dir().map(|d| d.join("beacon").join("run")))
    }
}

/// The one canonical spelling of a project root, used by the shim and the daemon alike (and
/// by `Session`): absolute, `..` resolved, symlinks / 8.3 names / letter case as on disk
/// (`build::canon`, which also tolerates a missing tail).
pub fn canonical_root(project: &Path) -> PathBuf {
    canon(project)
}

/// Stable per-worktree key: 16 hex chars of blake3 over [`canonical_root`] (lowercased on
/// Windows, like the trust key), so any spelling of the same root gives the same key.
pub fn project_key(project: &Path) -> String {
    let root = canonical_root(project);
    let s = root.to_string_lossy();
    let s = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    blake3::hash(s.as_bytes()).to_hex()[..16].to_string()
}

impl Discovery {
    pub fn read(path: &Path) -> Option<Discovery> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Atomic replace (temp file + rename): readers never see half a file.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// Serves `root` (a [`canonical_root`]); guards against a key collision.
    pub fn serves(&self, root: &Path) -> bool {
        let r = root.to_string_lossy();
        if cfg!(windows) {
            self.project.to_lowercase() == r.to_lowercase()
        } else {
            self.project == r
        }
    }
}

/// `<run dir>/<key>.json`.
pub fn discovery_path(run: &Path, key: &str) -> PathBuf {
    run.join(format!("{key}.json"))
}

/// `<run dir>/<key>.lock`: held exclusively (`File::try_lock`) by the live daemon.
pub fn lock_path(run: &Path, key: &str) -> PathBuf {
    run.join(format!("{key}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_short() {
        let a = project_key(Path::new("/p/one"));
        assert_eq!(a.len(), 16);
        assert_eq!(a, project_key(Path::new("/p/one")));
        assert_ne!(a, project_key(Path::new("/p/two")));
        if cfg!(windows) {
            assert_eq!(
                project_key(Path::new("D:\\P")),
                project_key(Path::new("d:\\p\\x\\.."))
            );
        }
        let run = Path::new("r");
        assert_eq!(discovery_path(run, &a), run.join(format!("{a}.json")));
        assert_eq!(lock_path(run, &a), run.join(format!("{a}.lock")));
    }
}
