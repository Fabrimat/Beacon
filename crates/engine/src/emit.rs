//! Workstream A2: writes one pack directory through the emit ledger (5.9).

use crate::canonical;
use crate::diag::{Code, Diagnostic};
use crate::model::Body;
use crate::path::{RelPath, Vfs};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

pub const LEDGER_FILE: &str = ".beacon-emit.json";

#[derive(Debug, Default)]
pub struct EmitReport {
    pub written: usize,
    pub unchanged: usize,
    pub deleted: usize,
}

/// Pack-relative path -> blake3 hex of what the build last wrote there.
pub type Ledger = BTreeMap<String, String>;

/// Windows ERROR_SHARING_VIOLATION (32) / ERROR_LOCK_VIOLATION (33).
fn is_sharing(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))
}

/// Runs `op`, retrying sharing violations with backoff (~0.3 s total).
fn retry(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut delay = 20;
    for _ in 0..4 {
        match op() {
            Err(e) if is_sharing(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                delay *= 2;
            }
            r => return r,
        }
    }
    op()
}

/// Creates `p` exclusively (`O_EXCL` / `CREATE_NEW`: never through an existing entry, so a
/// planted or dangling link is not followed); an existing entry (a stale temp file or a link)
/// is removed first, the link itself, never its target.
pub(crate) fn create_fresh(p: &Path) -> io::Result<fs::File> {
    let open = || fs::OpenOptions::new().write(true).create_new(true).open(p);
    match open() {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(p)?;
            open()
        }
        r => r,
    }
}

/// The first entry below `base` on the way down to `p` (inclusive) that is a symlink or
/// junction; `None` when there is none or the walk reaches a path that does not exist yet
/// (created later as a plain directory or file).
pub(crate) fn link_below(base: &Path, p: &Path) -> Option<PathBuf> {
    let mut cur = base.to_path_buf();
    for c in p.strip_prefix(base).ok()?.components() {
        cur.push(c);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Some(cur),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

/// Absent dest: write directly. Replacing: temp file in the same dir + atomic rename.
/// Callers have checked that no link lies on the way (see [`link_below`]).
fn write_file(
    dest: &Path,
    fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    retry(|| {
        // `symlink_metadata`, not `exists`: a dangling link at `dest` exists too
        let target = if fs::symlink_metadata(dest).is_ok() {
            let mut t = dest.as_os_str().to_owned();
            t.push(".beacon-tmp");
            PathBuf::from(t)
        } else {
            dest.to_owned()
        };
        let res = (|| {
            let mut w = BufWriter::new(create_fresh(&target)?);
            fill(&mut w)?;
            w.flush()?;
            drop(w);
            if target != dest {
                fs::rename(&target, dest)?;
            }
            Ok(())
        })();
        if res.is_err() {
            let _ = fs::remove_file(&target);
        }
        res
    })
}

fn read_ledger(dir: &Path) -> Option<Ledger> {
    serde_json::from_slice(&fs::read(dir.join(LEDGER_FILE)).ok()?).ok()
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
    dir: &Path,
    files: &[(RelPath, &Body)],
    vfs: &(dyn Vfs + Sync),
) -> (EmitReport, Vec<Diagnostic>) {
    let (rep, diags, _) = emit_known(dir, files, vfs, None);
    (rep, diags)
}

/// [`emit`] for a long-running session: `known` is the ledger this process wrote last time.
/// With it, destinations whose hash is unchanged are skipped without touching the disk (no
/// stat, no ledger read, no non-empty probe), and the ledger file is only rewritten when it
/// changed. Returns the new ledger.
// ponytail: a hand-deleted or hand-edited output is not noticed on such rebuilds; it is
// restored by the next full build (a new session, or after the caller drops `known` on a
// watcher overflow). Deletes stay limited to ledger-listed, validated paths.
pub fn emit_known(
    dir: &Path,
    files: &[(RelPath, &Body)],
    vfs: &(dyn Vfs + Sync),
    known: Option<&Ledger>,
) -> (EmitReport, Vec<Diagnostic>, Ledger) {
    let mut rep = EmitReport::default();
    let mut diags = Vec::new();
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        Diagnostic::new(Code::EmitFailed, format!("{what}: {e}"))
    };
    // canonicalize yields a `\\?\` verbatim path on Windows (long paths); segments are pushed
    // one by one so separators are backslashes there.
    let root = match fs::create_dir_all(dir).and_then(|_| fs::canonicalize(dir)) {
        Ok(r) => r,
        Err(e) => {
            diags.push(fail(&dir.display().to_string(), &e));
            return (rep, diags, Ledger::new());
        }
    };
    let mut read = if known.is_none() {
        read_ledger(&root)
    } else {
        None
    };
    if let Some(l) = &mut read {
        // a tampered ledger must never steer deletes outside `root`
        l.retain(|k, _| {
            let ok = RelPath::parse(k).is_ok_and(|p| p.as_str() == k);
            if !ok {
                diags.push(Diagnostic::new(
                    Code::LedgerMissing,
                    format!("{LEDGER_FILE}: ignoring invalid entry {k:?}; left untouched"),
                ));
            }
            ok
        });
    }
    let old: Option<&Ledger> = known.or(read.as_ref());
    if old.is_none() && fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some()) {
        diags.push(Diagnostic::new(
            Code::LedgerMissing,
            format!(
                "{} is missing or corrupt in {}; no stale files were deleted",
                LEDGER_FILE,
                dir.display()
            ),
        ));
    }
    let at = |rel: &str| rel.split('/').fold(root.clone(), |p, s| p.join(s));

    let mut ledger = Ledger::new();
    let mut todo = Vec::new();
    for (rel, body) in files {
        let bytes = match body {
            Body::Json(v) => Some(canonical::to_vec(v)),
            Body::Text(t) => Some(t.clone().into_bytes()),
            Body::Passthrough(_) => None,
        };
        let hash = match (&bytes, body) {
            (Some(b), _) => blake3::hash(b).to_hex().to_string(),
            (None, Body::Passthrough(src)) => match vfs.hash(src) {
                Ok(h) => blake3::Hash::from_bytes(h).to_hex().to_string(),
                Err(e) => {
                    diags.push(fail(&format!("{rel}: cannot hash {src}"), &e));
                    continue;
                }
            },
            _ => unreachable!(),
        };
        // ponytail: passthrough size is unknown without reading, so only existence is checked
        let unchanged = old.and_then(|l| l.get(rel.as_str())) == Some(&hash)
            && (known.is_some()
                || fs::metadata(at(rel.as_str()))
                    .is_ok_and(|m| bytes.as_ref().is_none_or(|b| m.len() == b.len() as u64)));
        if unchanged {
            rep.unchanged += 1;
            ledger.insert(rel.to_string(), hash);
            continue;
        }
        todo.push((rel, *body, bytes, hash));
    }

    // Links inside the pack dir would redirect writes (and deletes) anywhere: a write through
    // a linked folder is refused; a link at the destination itself is replaced only if the
    // ledger owns that path. Checked before any directory is created.
    let mut linked: std::collections::HashMap<PathBuf, Option<PathBuf>> = Default::default();
    let mut link_in = |dir: &Path| {
        linked
            .entry(dir.to_owned())
            .or_insert_with(|| link_below(&root, dir))
            .clone()
    };
    todo.retain(|(rel, ..)| {
        let dest = at(rel.as_str());
        let parent = dest.parent().unwrap_or(&root);
        let refuse = |link: &Path| {
            Diagnostic::new(
                Code::EmitFailed,
                format!(
                    "refusing to write {rel}: {} is a link (not owned by the build)",
                    link.display()
                ),
            )
        };
        if let Some(l) = link_in(parent) {
            diags.push(refuse(&l));
            return false;
        }
        if fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
            let owned = old.is_some_and(|o| o.contains_key(rel.as_str()));
            if !owned
                || fs::remove_file(&dest)
                    .or_else(|_| fs::remove_dir(&dest))
                    .is_err()
            {
                diags.push(refuse(&dest));
                return false;
            }
        }
        true
    });

    // directories first (serial, deduplicated), then the files in parallel: on Windows with
    // Defender a create costs ~1 ms, so a cold emit of thousands of files is latency-bound
    let mut dirs = std::collections::BTreeSet::new();
    for (rel, ..) in &todo {
        if let Some(p) = at(rel.as_str()).parent() {
            dirs.insert(p.to_owned());
        }
    }
    for d in dirs {
        let _ = fs::create_dir_all(d); // a failure surfaces as that file's write error
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(8);
    let write_one = |(rel, body, bytes, _): &(&RelPath, &Body, Option<Vec<u8>>, String)| {
        write_file(&at(rel.as_str()), &mut |w| match (bytes, body) {
            (Some(b), _) => w.write_all(b),
            (None, Body::Passthrough(src)) => vfs.stream(src, w).map(|_| ()),
            _ => unreachable!(),
        })
    };
    let results: Vec<io::Result<()>> = if threads < 2 || todo.len() < 16 {
        todo.iter().map(write_one).collect()
    } else {
        let chunk = todo.len().div_ceil(threads);
        std::thread::scope(|sc| {
            let hs: Vec<_> = todo
                .chunks(chunk)
                .map(|c| sc.spawn(|| c.iter().map(write_one).collect::<Vec<_>>()))
                .collect();
            hs.into_iter()
                .flat_map(|h| h.join().expect("emit worker"))
                .collect()
        })
    };
    // joined in input order: ledger and diagnostics stay deterministic
    for ((rel, _, _, hash), res) in todo.into_iter().zip(results) {
        match res {
            Ok(()) => {
                rep.written += 1;
                ledger.insert(rel.to_string(), hash);
            }
            Err(e) => diags.push(fail(&format!("cannot write {rel}"), &e)),
        }
    }

    if let Some(old) = old {
        let wanted: std::collections::HashSet<&str> =
            files.iter().map(|(r, _)| r.as_str()).collect();
        for (rel, hash) in old {
            if wanted.contains(rel.as_str()) {
                // a failed write keeps its old ownership
                ledger.entry(rel.clone()).or_insert_with(|| hash.clone());
                continue;
            }
            let path = at(rel);
            if let Some(l) = path.parent().and_then(&mut link_in) {
                diags.push(Diagnostic::new(
                    Code::EmitFailed,
                    format!("not deleting {rel}: {} is a link", l.display()),
                ));
                ledger.insert(rel.clone(), hash.clone());
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => rep.deleted += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => {
                    diags.push(fail(&format!("cannot delete {rel}"), &e));
                    ledger.insert(rel.clone(), hash.clone());
                    continue;
                }
            }
            let mut d = path.parent();
            while let Some(p) = d.filter(|p| *p != root && p.starts_with(&root)) {
                if fs::remove_dir(p).is_err() {
                    break; // not empty
                }
                d = p.parent();
            }
        }
    }

    if known != Some(&ledger) {
        let bytes = canonical::to_vec(&serde_json::to_value(&ledger).expect("ledger json"));
        let lp = root.join(LEDGER_FILE);
        if fs::read(&lp).ok().as_deref() != Some(&bytes)
            && let Err(e) = write_file(&lp, &mut |w| w.write_all(&bytes))
        {
            diags.push(fail(LEDGER_FILE, &e));
        }
    }
    (rep, diags, ledger)
}
