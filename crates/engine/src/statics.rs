//! Static outputs (4.3): prebuilt packs (`outputs[].static`) included as-is, UUIDs and
//! versions kept. Runs right after routing; its items join the routed ones. Not built yet.

use crate::config::ProjectConfig;
use crate::diag::Diagnostic;
use crate::model::ContentItem;
use crate::path::Vfs;

/// Passthrough items (own `manifest.json` included) for every static output's packs, plus
/// `StaticPackInvalid` diagnostics.
pub fn items(cfg: &ProjectConfig, vfs: &dyn Vfs) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    let _ = (cfg, vfs);
    (Vec::new(), Vec::new())
}
