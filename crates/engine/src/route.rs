//! Workstream A1: config loading and routing (5.3 concepts 1 and 2).

use crate::config::ProjectConfig;
use crate::diag::Diagnostic;
use crate::model::{ContentItem, Generator, OutputId};
use crate::path::{RelPath, Vfs};
use std::collections::BTreeMap;
use std::path::Path;

/// Reads and validates `config.json` (4.1). Errors use `ConfigInvalid` / `ConfigField`.
pub fn load(_root: &Path) -> Result<ProjectConfig, Vec<Diagnostic>> {
    todo!("A1")
}

#[derive(Debug, Default)]
pub struct Routed {
    /// Convention + `module.json` items, sorted by (dest, origin). Not conflict-resolved yet.
    pub items: Vec<ContentItem>,
    pub generators: Vec<Generator>,
    /// Per BP output: game-script entries = every module's top-level `scripts/*.ts`
    /// (not `.d.ts`), sorted. Phase-2 hooks replace this rule.
    pub script_entries: BTreeMap<OutputId, Vec<RelPath>>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Routes every indexed file under `beacon.sources`. Modules = first-level folders of each
/// source dir. Output: `module.json` `output`, else the first output whose `include` glob
/// matches the module folder, else the implicit `main` output when `outputs` is empty.
pub fn route(_cfg: &ProjectConfig, _vfs: &dyn Vfs) -> Routed {
    todo!("A1")
}
