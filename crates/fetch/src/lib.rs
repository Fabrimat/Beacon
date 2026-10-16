//! `beacon-fetch`: pinned downloads (9.2) for tools Beacon runs (esbuild first). A download is
//! accepted only if its size and blake3 hash match the pin compiled into Beacon. The engine
//! maps [`FetchError`] to `DownloadFailed` / `DownloadHashMismatch` / `DownloadNeedsConfirm`
//! diagnostics.

use std::fs::File;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    /// Ask on the terminal (stdin and stderr are a TTY); without a TTY this is `No`.
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

/// Default cache dir: `<user data dir>/beacon/tools` (`BEACON_HOME` replaces `<user data dir>/beacon`).
pub fn default_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("BEACON_HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::data_local_dir()?.join("beacon"),
    };
    Some(base.join("tools"))
}

/// Hex blake3 of the file, or `None` if it is unreadable.
fn hash_file(path: &Path) -> Option<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(File::open(path).ok()?).ok()?;
    Some(h.finalize().to_hex().to_string())
}

/// Returns `dir/<blake3>` holding the pinned file, downloading it on a cache miss:
///
/// 1. Cache by hash: `dir/<blake3>` that exists and re-hashes to the pin is returned as is,
///    without network or confirmation. A cached file that no longer matches is deleted (miss).
/// 2. Otherwise ask per `confirm` ([`FetchError::NeedsConfirm`] when refused or `No`).
/// 3. Stream `url` (HTTPS only, rustls) into a temp file in `dir`, hashing while writing and
///    failing as soon as more than `size` bytes arrive.
/// 4. Size and hash must equal the pin, else the temp file is deleted and the result is
///    [`FetchError::HashMismatch`].
/// 5. Atomic rename of the temp file to `dir/<blake3>` (a concurrent fetch of the same pin
///    renaming first is fine: same bytes).
pub fn fetch(p: &Pinned, dir: &Path, confirm: Confirm) -> Result<PathBuf, FetchError> {
    fetch_inner(p, dir, confirm, false)
}

fn fetch_inner(
    p: &Pinned,
    dir: &Path,
    confirm: Confirm,
    allow_http: bool,
) -> Result<PathBuf, FetchError> {
    let dest = dir.join(p.blake3);
    if dest.is_file() {
        if hash_file(&dest).as_deref() == Some(p.blake3) {
            return Ok(dest);
        }
        std::fs::remove_file(&dest)?; // tampered or truncated: never trust it
    }
    let ok = match confirm {
        Confirm::Yes => true,
        Confirm::No => false,
        Confirm::Tty => {
            if io::stdin().is_terminal() && io::stderr().is_terminal() {
                eprint!(
                    "Download {} ({} bytes) to {}? [y/N] ",
                    p.url,
                    p.size,
                    dir.display()
                );
                let mut a = String::new();
                let _ = io::stdin().read_line(&mut a);
                a.trim().eq_ignore_ascii_case("y")
            } else {
                false
            }
        }
    };
    if !ok {
        return Err(FetchError::NeedsConfirm);
    }
    std::fs::create_dir_all(dir)?;
    let http = |e: &dyn std::fmt::Display| FetchError::Http(format!("{}: {e}", p.url));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(!allow_http)
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!(
            "beacon/",
            env!("CARGO_PKG_VERSION"),
            " (pinned download)"
        ))
        .build()
        .into();
    let resp = agent.get(p.url).call().map_err(|e| http(&e))?;
    let mut body = resp.into_body().into_reader().take(p.size + 1);
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?; // dropped on any error = removed
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let r = body.read(&mut buf).map_err(|e| http(&e))?;
        if r == 0 {
            break;
        }
        n += r as u64;
        if n > p.size {
            break; // abort: more bytes than pinned
        }
        hasher.update(&buf[..r]);
        tmp.write_all(&buf[..r])?;
    }
    let actual = hasher.finalize().to_hex().to_string();
    if n != p.size || actual != p.blake3 {
        let got = if n > p.size {
            format!("more than {} bytes", p.size)
        } else {
            actual
        };
        return Err(FetchError::HashMismatch {
            expected: p.blake3.to_string(),
            actual: got,
        });
    }
    tmp.persist(&dest).map_err(|e| e.error)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves `body` to every connection (the thread ends with the test process).
    fn serve(body: Vec<u8>) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/f", l.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut s in l.incoming().flatten() {
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&body);
            }
        });
        url
    }

    fn pin(url: &str, body: &[u8]) -> Pinned {
        let url: &'static str = Box::leak(url.to_string().into_boxed_str());
        let h: &'static str = Box::leak(blake3::hash(body).to_hex().to_string().into_boxed_str());
        Pinned {
            url,
            blake3: h,
            size: body.len() as u64,
        }
    }

    fn files(d: &Path) -> usize {
        std::fs::read_dir(d).unwrap().count()
    }

    #[test]
    fn downloads_then_caches_without_network() {
        let d = tempfile::tempdir().unwrap();
        let body = b"hello pinned".to_vec();
        let p = pin(&serve(body.clone()), &body);
        let f = fetch_inner(&p, d.path(), Confirm::Yes, true).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), body);
        assert_eq!(files(d.path()), 1);
        let dead = Pinned {
            url: "http://127.0.0.1:1/x",
            ..p
        };
        assert_eq!(fetch_inner(&dead, d.path(), Confirm::No, true).unwrap(), f);
    }

    #[test]
    fn needs_confirm_and_http_rejected() {
        let d = tempfile::tempdir().unwrap();
        let body = b"x".to_vec();
        let p = pin(&serve(body.clone()), &body);
        assert!(matches!(
            fetch_inner(&p, d.path(), Confirm::No, true),
            Err(FetchError::NeedsConfirm)
        ));
        // the public entry point is HTTPS-only
        assert!(matches!(
            fetch(&p, d.path(), Confirm::Yes),
            Err(FetchError::Http(_))
        ));
        assert_eq!(files(d.path()), 0);
    }

    #[test]
    fn mismatch_and_overflow_leave_nothing() {
        let d = tempfile::tempdir().unwrap();
        let good = b"0123456789".to_vec();
        for bad in [&b"abcdefghij"[..], b"0123456789 and more", b"012"] {
            let p = pin(&serve(bad.to_vec()), &good);
            let e = fetch_inner(&p, d.path(), Confirm::Yes, true).unwrap_err();
            assert!(matches!(e, FetchError::HashMismatch { .. }), "{e}");
        }
        assert_eq!(files(d.path()), 0);
    }

    #[test]
    fn tampered_cache_is_rejected_then_refetched() {
        let d = tempfile::tempdir().unwrap();
        let body = b"genuine bytes".to_vec();
        let p = pin(&serve(body.clone()), &body);
        let f = fetch_inner(&p, d.path(), Confirm::Yes, true).unwrap();
        std::fs::write(&f, b"evil evil evl").unwrap();
        assert!(matches!(
            fetch_inner(&p, d.path(), Confirm::No, true),
            Err(FetchError::NeedsConfirm)
        ));
        assert!(!f.exists());
        std::fs::write(&f, b"evil").unwrap();
        let f = fetch_inner(&p, d.path(), Confirm::Yes, true).unwrap();
        assert_eq!(std::fs::read(f).unwrap(), body);
    }
}
