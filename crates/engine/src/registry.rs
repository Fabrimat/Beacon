//! Derived registries (5.9): files the game wants as one list per pack (e.g.
//! `item_texture.json`, `terrain_texture.json`, tick hooks), derived from a plan's items and
//! the modules' `module.json`. Runs per plan, before merge. Not built yet.

use crate::build::Plan;
use crate::config::ProjectConfig;
use crate::diag::Diagnostic;
use crate::model::ContentItem;
use crate::path::Vfs;
use crate::route::Routed;

/// Extra items for `plan` (merged with the rest by `merge::resolve`), plus diagnostics.
pub(crate) fn derive(
    cfg: &ProjectConfig,
    routed: &Routed,
    plan: &Plan,
    items: &[ContentItem],
    vfs: &dyn Vfs,
) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    let _ = (cfg, routed, plan, items, vfs);
    (Vec::new(), Vec::new())
}
