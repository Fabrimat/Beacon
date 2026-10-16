//! Workstream A1: the one merge primitive (5.3). Groups items by `Dest` and applies `Merge`.

use crate::diag::Diagnostic;
use crate::model::ContentItem;
use crate::path::Vfs;

/// Returns one item per `Dest` (sorted by dest) plus diagnostics: `RouteCollision`,
/// `CaseFoldCollision` (dest paths equal after case folding within one pack),
/// `LangDuplicateKey`, `MergeConflict`. Passthrough bodies taking part in a JSON or lang merge
/// are read through `vfs` (JSON with `//` and `/* */` comments must parse).
pub fn resolve(_items: Vec<ContentItem>, _vfs: &dyn Vfs) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    todo!("A1")
}
