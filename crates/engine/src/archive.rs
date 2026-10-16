//! Workstream A2: deterministic archives (5.2): sorted entries, fixed mtime and permissions,
//! fixed deflate level; already-compressed formats (png, ogg, jpg) stored.

use crate::model::Body;
use crate::path::{RelPath, Vfs};
use std::path::Path;

/// Writes a zip at `out` with `entries` (zip path -> body). `.mcpack` = one pack at the zip
/// root; `.mcaddon` = each pack in its own top-level folder (caller builds the paths).
pub fn write_zip(
    _out: &Path,
    _entries: &[(RelPath, &Body)],
    _vfs: &dyn Vfs,
) -> std::io::Result<()> {
    todo!("A2")
}
