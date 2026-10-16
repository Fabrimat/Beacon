//! `beacon-fetch`: pinned downloads (9.2) for tools Beacon runs (esbuild first). A download is
//! accepted only if its size and blake3 hash match the pin compiled into Beacon. Skeleton: the
//! shapes are fixed, [`fetch`] is not built yet. The engine maps [`FetchError`] to
//! `DownloadFailed` / `DownloadHashMismatch` / `DownloadNeedsConfirm` diagnostics.

use std::path::{Path, PathBuf};

/// A file pinned at build time of Beacon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pinned {
    pub url: &'static str,
    /// Hex blake3 of the whole file.
    pub blake3: &'static str,
    /// Exact size in bytes (checked while streaming, before hashing finishes).
    pub size: u64,
}

/// May the download happen?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// Already confirmed (`--yes`).
    Yes,
    /// Ask on the terminal (stdin and stderr are a TTY).
    Tty,
    /// Never download: a cache miss is [`FetchError::NeedsConfirm`].
    No,
}

#[derive(Debug)]
pub enum FetchError {
    /// Not cached and not confirmed.
    NeedsConfirm,
    /// Network or HTTP failure (message names the URL).
    Http(String),
    /// Size or hash differs from the pin; the file was discarded.
    HashMismatch {
        expected: String,
        actual: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NeedsConfirm => write!(f, "download not confirmed"),
            FetchError::Http(e) => write!(f, "download failed: {e}"),
            FetchError::HashMismatch { expected, actual } => {
                write!(f, "hash mismatch: expected {expected}, got {actual}")
            }
            FetchError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FetchError {}

impl From<std::io::Error> for FetchError {
    fn from(e: std::io::Error) -> Self {
        FetchError::Io(e)
    }
}

/// Returns `dir/<blake3>` holding the pinned file, downloading it on a cache miss:
///
/// 1. Cache by hash: `dir/<blake3>` that exists (and re-hashes to the pin) is returned as is,
///    without network or confirmation.
/// 2. Otherwise ask per `confirm` ([`FetchError::NeedsConfirm`] when refused or `No`).
/// 3. Stream `url` (HTTPS only, rustls) into a temp file in `dir`, hashing while writing and
///    failing as soon as more than `size` bytes arrive.
/// 4. Size and hash must equal the pin, else the temp file is deleted and the result is
///    [`FetchError::HashMismatch`].
/// 5. Atomic rename of the temp file to `dir/<blake3>` (a concurrent fetch of the same pin
///    renaming first is fine: same bytes).
pub fn fetch(p: &Pinned, dir: &Path, confirm: Confirm) -> Result<PathBuf, FetchError> {
    let _ = (p, dir, confirm);
    unimplemented!("beacon-fetch: pinned downloads are not built yet")
}
