//! Workstream A2: generated `manifest.json` and base lang (4.1, 5.9).

use crate::config::ProjectConfig;
use crate::lock::Lockfile;
use crate::model::{ContentItem, Edition, OutputId};
use std::path::Path;

pub struct ManifestInput<'a> {
    pub cfg: &'a ProjectConfig,
    pub lock: &'a Lockfile,
    pub output: &'a OutputId,
    pub edition: Edition,
    /// Dev build: dev UUIDs derived from this worktree root; versions unchanged.
    pub dev_worktree: Option<&'a Path>,
    /// BP-relative path of the script bundle entry (e.g. `scripts/main.js`), if any: adds the
    /// script module and `beacon.scriptModules` dependencies.
    pub script_entry: Option<&'a str>,
    pub has_bp: bool,
    pub has_rp: bool,
}

/// `manifest.json` for each pack of the output, base `texts/en_US.lang` (`pack.name`,
/// `pack.description`, `Merge::LangLines` so module lang files merge into it) and
/// `pack_icon.png` (passthrough) when `packMeta.icon` is set. `texts/languages.json` is NOT
/// generated here: the orchestrator derives it after merge from the `texts/*.lang` dests.
pub fn generate(_m: &ManifestInput) -> Vec<ContentItem> {
    todo!("A2")
}
