//! Contract (phase 4): git-sourced plugins and `beacon plugin update` (5.10).
//!
//! The only runnable plugin kind today is `legacyFilter` (14.3). Its `cwd` may now come from
//! git instead of the project: `{"legacyFilter": {..., "source": {"git": "<https url>",
//! "ref": "<tag or full sha>"}}}` (`id` = the entry's `id`, required with `source`).
//!
//! - **Pins live in the lockfile** (`Lockfile::plugins`): full commit SHA + blake3 content
//!   hash of the fetched tree (sorted paths + per-file hashes, `.git` excluded). Builds use
//!   the pin only; they never resolve a ref, so a moving branch cannot change a build.
//!   A source without a pin is `PluginUnpinned`; a fetched tree not matching its hash is
//!   `PluginHashMismatch` and does not run.
//! - **`beacon plugin update [<id>...] [--to <ref>]`** resolves `ref` (or `--to`) to a full SHA
//!   with the system `git` (`ls-remote`, then `fetch --depth 1 <sha>` into
//!   `<data dir>/beacon/plugins/<sha>/`), shows the diff (old/new SHA, files added / removed
//!   / changed, `run` command changes) and writes the lock only after confirmation (`--yes`
//!   off a TTY). Branch names are accepted for `--to`/`ref` only here, and are always
//!   recorded as the SHA they resolved to. Only in trusted sessions (5.5).
//! - **Trust re-prompt**: the trust key (5.5) becomes `config.json` plugins **plus** the
//!   lock's plugin pins ([`trust_key`]). An update therefore invalidates the stored decision
//!   and the next run asks again. With no pins the key is exactly today's (the plugin array),
//!   so existing trust decisions stay valid.

use crate::diag::Diagnostic;
use crate::lock::Lockfile;
use serde_json::Value;
use std::path::Path;

/// One pinned plugin in `beacon.lock.json` `plugins.<id>`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PluginLock {
    /// Git URL as written in `config.json` (https only).
    pub git: String,
    /// Full 40-hex (SHA-1) or 64-hex (SHA-256) commit id.
    pub commit: String,
    /// blake3 of the fetched tree, hex.
    pub hash: String,
}

/// What an update would change for one plugin (shown before writing; `--json` output).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdate {
    pub id: String,
    pub from: Option<PluginLock>,
    pub to: PluginLock,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

/// The value hashed into the trust key: the `config.json` plugin array, plus
/// `{"pins": lock.plugins}` appended only when there are pins.
pub fn trust_key(config_plugins: &[Value], lock: Option<&Lockfile>) -> Vec<Value> {
    let _ = (config_plugins, lock);
    todo!("phase 4 wave 2: plugin update")
}

/// Fetches and diffs; writes nothing. `ids` empty = every git-sourced plugin.
pub fn plan_update(
    root: &Path,
    ids: &[String],
    to: Option<&str>,
) -> Result<Vec<PluginUpdate>, Diagnostic> {
    let _ = (root, ids, to);
    todo!("phase 4 wave 2: plugin update")
}

/// Writes the new pins to the lockfile (atomically).
pub fn apply_update(root: &Path, updates: &[PluginUpdate]) -> Result<(), Diagnostic> {
    let _ = (root, updates);
    todo!("phase 4 wave 2: plugin update")
}
