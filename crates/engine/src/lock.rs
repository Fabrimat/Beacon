//! Workstream A2: `beacon.lock.json` (4.2).

use crate::config::ProjectConfig;
use crate::diag::Diagnostic;
use crate::model::{Edition, OutputId, Pack};
use std::path::Path;

/// A2 defines the fields: pack + module UUIDs keyed by (output x edition) (one shared UUID
/// when `sharedEditionUuids`), script module UUID per output, `canonicalVersion`, bundler
/// name + version.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lockfile {}

impl Lockfile {
    /// Loads the lockfile, adds missing entries for `outputs` (new random UUIDs), and saves
    /// it (canonical JSON) if it changed. With `frozen`, a needed change is
    /// `Code::LockfileFrozen` instead.
    pub fn ensure(
        _root: &Path,
        _cfg: &ProjectConfig,
        _outputs: &[OutputId],
        _frozen: bool,
    ) -> Result<Lockfile, Diagnostic> {
        todo!("A2")
    }

    /// Release UUID of a pack header.
    pub fn pack_uuid(&self, _output: &str, _pack: Pack, _edition: Edition) -> String {
        todo!("A2")
    }
}

/// Dev UUID (4.2): deterministic from the release UUID and the worktree root path, never
/// committed (blake3-derived, formatted as a v4-shaped UUID).
pub fn dev_uuid(_release: &str, _worktree_root: &Path) -> String {
    todo!("A2")
}
