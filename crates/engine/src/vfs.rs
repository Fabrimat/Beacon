//! Workstream A1: the project index and the [`Vfs`] implementation (5.2).
//! Keep the public signatures; internals are yours.

use crate::diag::Diagnostic;
use crate::path::{Hash, RelPath, Vfs};
use std::path::Path;

pub struct Index {
    _todo: (),
}

impl Index {
    /// Walks `root` (sorted by NFC byte order, never following reparse points) and indexes
    /// path + size + mtime + file id + blake3 for every file. Excludes `.git`, `.beacon`,
    /// `node_modules`, the `packs.*` folders and `extra_excludes`. Reparse points are skipped
    /// with `Code::ReparsePointSkipped`. `prev` enables the (mtime, size, file-id) fast path.
    pub fn build(
        _root: &Path,
        _extra_excludes: &[RelPath],
        _prev: Option<&Index>,
    ) -> (Index, Vec<Diagnostic>) {
        todo!("A1")
    }

    /// All indexed files, sorted.
    pub fn files(&self) -> Vec<&RelPath> {
        todo!("A1")
    }
}

impl Vfs for Index {
    fn exists(&self, _p: &RelPath) -> bool {
        todo!("A1")
    }
    fn read(&self, _p: &RelPath) -> std::io::Result<Vec<u8>> {
        todo!("A1")
    }
    fn hash(&self, _p: &RelPath) -> std::io::Result<Hash> {
        todo!("A1")
    }
    fn list(&self, _dir: Option<&RelPath>) -> Vec<String> {
        todo!("A1")
    }
    fn stream(&self, _p: &RelPath, _out: &mut dyn std::io::Write) -> std::io::Result<u64> {
        todo!("A1")
    }
}
