//! Workstream A1: the project index and the [`Vfs`] implementation (5.2).
//! This is the sandbox boundary (5.4): only indexed paths exist, nothing is followed through
//! symlinks, junctions or mount points.

use crate::diag::{Code, Diagnostic};
use crate::path::{Hash, RelPath, Vfs};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

const ALWAYS_EXCLUDED: [&str; 3] = [".git", ".beacon", "node_modules"];

/// (size, mtime in ns, file id): the fast-path identity of a file.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime: u128,
    id: (u64, u64),
}

struct Entry {
    /// Real OS path (macOS may hand out NFD names, so never rebuild it from the key).
    os: PathBuf,
    stat: Stat,
    hash: Hash,
}

pub struct Index {
    files: BTreeMap<RelPath, Entry>,
    /// Directory (`""` = root) -> sorted child names.
    children: BTreeMap<String, BTreeSet<String>>,
}

#[cfg(windows)]
fn is_reparse(md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    md.file_attributes() & ::windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
        != 0
}
#[cfg(not(windows))]
fn is_reparse(md: &std::fs::Metadata) -> bool {
    md.file_type().is_symlink()
}

/// Opens without following a final-component reparse point.
#[cfg(windows)]
fn open_nofollow(p: &Path) -> io::Result<File> {
    use ::windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    use std::os::windows::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(p)
}
#[cfg(unix)]
fn open_nofollow(p: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(p)
    {
        Err(e) if e.kind() == ErrorKind::FilesystemLoop => Err(changed()),
        r => r,
    }
}

/// An indexed path whose file is gone, swapped, a link, or modified: never `NotFound`, which
/// is reserved for unindexed paths. The orchestrator re-indexes on this error.
fn changed() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "changed since indexing")
}

/// Stats an open handle. A reparse point or non-file is reported as [`changed`].
#[cfg(windows)]
fn stat(f: &File) -> io::Result<Stat> {
    use ::windows_sys::Win32::Foundation::HANDLE;
    use ::windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
    };
    use std::os::windows::io::AsRawHandle;
    // SAFETY: plain out-struct filled by the OS for a live handle.
    let i = unsafe {
        let mut i: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(f.as_raw_handle() as HANDLE, &mut i) == 0 {
            return Err(io::Error::last_os_error());
        }
        i
    };
    if i.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(changed());
    }
    let w = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
    Ok(Stat {
        size: w(i.nFileSizeHigh, i.nFileSizeLow),
        mtime: u128::from(w(
            i.ftLastWriteTime.dwHighDateTime,
            i.ftLastWriteTime.dwLowDateTime,
        )),
        id: (
            u64::from(i.dwVolumeSerialNumber),
            w(i.nFileIndexHigh, i.nFileIndexLow),
        ),
    })
}
#[cfg(unix)]
fn stat(f: &File) -> io::Result<Stat> {
    use std::os::unix::fs::MetadataExt;
    let m = f.metadata()?;
    if !m.is_file() {
        return Err(changed());
    }
    Ok(Stat {
        size: m.len(),
        mtime: (m.mtime() as u128) * 1_000_000_000 + m.mtime_nsec() as u128,
        id: (m.dev(), m.ino()),
    })
}

fn excluded(name: &str, rel: &str, extra: &[RelPath]) -> bool {
    let ci = cfg!(any(windows, target_os = "macos"));
    let eq = |a: &str, b: &str| {
        if ci {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };
    ALWAYS_EXCLUDED.iter().any(|x| x.eq_ignore_ascii_case(name))
        || extra.iter().any(|e| {
            let e = e.as_str();
            rel.len() >= e.len()
                && rel.is_char_boundary(e.len())
                && eq(&rel[..e.len()], e)
                && (rel.len() == e.len() || rel[e.len()..].starts_with('/'))
        })
}

struct Walk<'a> {
    extra: &'a [RelPath],
    prev: Option<&'a Index>,
    index: Index,
    diags: Vec<Diagnostic>,
}

impl Walk<'_> {
    fn dir(&mut self, abs: &Path, rel: &str) {
        let rd = match std::fs::read_dir(abs) {
            Ok(rd) => rd,
            Err(e) => {
                self.diags.push(Diagnostic::new(
                    Code::Internal,
                    format!("cannot read dir {}: {e}", abs.display()),
                ));
                return;
            }
        };
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        for e in rd.flatten() {
            match e.file_name().to_str() {
                Some(n) => entries.push((n.nfc().collect(), e.path())),
                None => self.diags.push(Diagnostic::new(
                    Code::NonPortablePath,
                    format!("file name is not valid Unicode: {}", e.path().display()),
                )),
            }
        }
        entries.sort();
        for (name, path) in entries {
            let child = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            if excluded(&name, &child, self.extra) {
                continue;
            }
            let key = match RelPath::parse(&child) {
                Ok(k) => k,
                Err(e) => {
                    self.diags
                        .push(Diagnostic::new(Code::NonPortablePath, e.to_string()));
                    continue;
                }
            };
            let Ok(md) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if is_reparse(&md) {
                self.diags.push(
                    Diagnostic::new(
                        Code::ReparsePointSkipped,
                        format!("skipped link or junction {key}"),
                    )
                    .at(key, None, None),
                );
            } else if md.is_dir() {
                self.dir(&path, &child);
            } else if md.is_file() {
                self.file(key, path);
            }
        }
    }

    fn file(&mut self, key: RelPath, os: PathBuf) {
        let opened = open_nofollow(&os).and_then(|f| Ok((stat(&f)?, f)));
        let (s, mut f) = match opened {
            Ok(v) => v,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) => return, // vanished or became a link
            Err(e) => {
                self.diags.push(
                    Diagnostic::new(Code::Internal, format!("cannot index {key}: {e}"))
                        .at(key, None, None),
                );
                return;
            }
        };
        let hash = match self
            .prev
            .and_then(|p| p.files.get(&key))
            .filter(|p| p.stat == s)
        {
            Some(p) => p.hash,
            None => {
                let mut h = blake3::Hasher::new();
                if let Err(e) = h.update_reader(&mut f) {
                    self.diags.push(
                        Diagnostic::new(Code::Internal, format!("cannot hash {key}: {e}"))
                            .at(key, None, None),
                    );
                    return;
                }
                *h.finalize().as_bytes()
            }
        };
        let mut dir = key.as_str();
        while let Some((parent, name)) = dir.rsplit_once('/') {
            self.index
                .children
                .entry(parent.to_owned())
                .or_default()
                .insert(name.to_owned());
            dir = parent;
        }
        self.index
            .children
            .entry(String::new())
            .or_default()
            .insert(dir.to_owned());
        self.index.files.insert(key, Entry { os, stat: s, hash });
    }
}

impl Index {
    /// Walks `root` (sorted by NFC byte order, never following reparse points) and indexes
    /// path + size + mtime + file id + blake3 for every file. Excludes `.git`, `.beacon`,
    /// `node_modules` and `extra_excludes`; the caller must pass the `packs.*` folders there
    /// (they are not known here). Reparse points are skipped
    /// with `Code::ReparsePointSkipped`. `prev` enables the (mtime, size, file-id) fast path.
    pub fn build(
        root: &Path,
        extra_excludes: &[RelPath],
        prev: Option<&Index>,
    ) -> (Index, Vec<Diagnostic>) {
        let mut w = Walk {
            extra: extra_excludes,
            prev,
            index: Index {
                files: BTreeMap::new(),
                children: BTreeMap::new(),
            },
            diags: Vec::new(),
        };
        w.dir(root, "");
        (w.index, w.diags)
    }

    /// All indexed files, sorted.
    pub fn files(&self) -> Vec<&RelPath> {
        self.files.keys().collect()
    }

    fn open(&self, p: &RelPath) -> io::Result<File> {
        let e = self.files.get(p).ok_or(ErrorKind::NotFound)?;
        let f = open_nofollow(&e.os).map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                changed()
            } else {
                err
            }
        })?;
        // (size, mtime, file id) must still match: a swapped dir or junction fails on file id
        if stat(&f)? != e.stat {
            return Err(changed());
        }
        Ok(f)
    }
}

impl Vfs for Index {
    fn exists(&self, p: &RelPath) -> bool {
        self.files.contains_key(p) || self.children.contains_key(p.as_str())
    }
    fn read(&self, p: &RelPath) -> io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.open(p)?.read_to_end(&mut buf)?;
        if blake3::hash(&buf).as_bytes() != &self.files[p].hash {
            return Err(changed());
        }
        Ok(buf)
    }
    fn hash(&self, p: &RelPath) -> io::Result<Hash> {
        self.files
            .get(p)
            .map(|e| e.hash)
            .ok_or_else(|| ErrorKind::NotFound.into())
    }
    fn list(&self, dir: Option<&RelPath>) -> Vec<String> {
        self.children
            .get(dir.map_or("", RelPath::as_str))
            .map(|c| c.iter().cloned().collect())
            .unwrap_or_default()
    }
    /// On `Err` (including "changed since indexing", detected only at the end) the caller
    /// must discard whatever was already written to `out`.
    fn stream(&self, p: &RelPath, out: &mut dyn io::Write) -> io::Result<u64> {
        struct Tee<'a>(&'a mut dyn io::Write, blake3::Hasher);
        impl io::Write for Tee<'_> {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                let n = self.0.write(b)?;
                self.1.update(&b[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        let mut tee = Tee(out, blake3::Hasher::new());
        let n = io::copy(&mut self.open(p)?, &mut tee)?;
        if tee.1.finalize().as_bytes() != &self.files[p].hash {
            return Err(changed());
        }
        Ok(n)
    }
}
