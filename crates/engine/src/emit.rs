//! Workstream A2: writes one pack directory through the emit ledger (5.9).

use crate::diag::Diagnostic;
use crate::model::Body;
use crate::path::{RelPath, Vfs};
use std::path::Path;

pub const LEDGER_FILE: &str = ".beacon-emit.json";

#[derive(Debug, Default)]
pub struct EmitReport {
    pub written: usize,
    pub unchanged: usize,
    pub deleted: usize,
}

/// Makes `dir` contain `files` (pack-relative), touching only files the ledger owns:
/// - unchanged content (by hash) is not rewritten;
/// - absent dest: write directly; replacing: temp file in the same dir + atomic rename;
///   sharing violations retry with backoff, then `Code::EmitFailed`;
/// - stale files are deleted only if listed in the ledger; a missing/corrupt ledger in a
///   non-empty dir deletes nothing (`Code::LedgerMissing`); files not in the ledger are
///   never deleted;
/// - long paths via `\\?\` on Windows; the ledger is written last.
pub fn emit(
    _dir: &Path,
    _files: &[(RelPath, &Body)],
    _vfs: &dyn Vfs,
) -> (EmitReport, Vec<Diagnostic>) {
    todo!("A2")
}
