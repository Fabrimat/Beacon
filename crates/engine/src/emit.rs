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

type Ledger = BTreeMap<String, String>;

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

/// Absent dest: write directly. Replacing: temp file in the same dir + atomic rename.
fn write_file(
    dest: &Path,
    fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>,
) -> io::Result<()> {
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    retry(|| {
        let target = if dest.exists() {
            let mut t = dest.as_os_str().to_owned();
            t.push(".beacon-tmp");
            PathBuf::from(t)
        } else {
            dest.to_owned()
        };
        let res = (|| {
            let mut w = BufWriter::new(fs::File::create(&target)?);
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
    vfs: &dyn Vfs,
) -> (EmitReport, Vec<Diagnostic>) {
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
            return (rep, diags);
        }
    };
    let mut old = read_ledger(&root);
    if let Some(l) = &mut old {
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
    let non_empty = fs::read_dir(&root).is_ok_and(|mut d| d.next().is_some());
    if old.is_none() && non_empty {
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
    for (rel, body) in files {
        let dest = at(rel.as_str());
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
        let unchanged = old.as_ref().and_then(|l| l.get(rel.as_str())) == Some(&hash)
            && fs::metadata(&dest)
                .is_ok_and(|m| bytes.as_ref().is_none_or(|b| m.len() == b.len() as u64));
        if unchanged {
            rep.unchanged += 1;
            ledger.insert(rel.to_string(), hash);
            continue;
        }
        let res = write_file(&dest, &mut |w| match (&bytes, body) {
            (Some(b), _) => w.write_all(b),
            (None, Body::Passthrough(src)) => vfs.stream(src, w).map(|_| ()),
            _ => unreachable!(),
        });
        match res {
            Ok(()) => {
                rep.written += 1;
                ledger.insert(rel.to_string(), hash);
            }
            Err(e) => diags.push(fail(&format!("cannot write {rel}"), &e)),
        }
    }

    if let Some(old) = &old {
        let wanted: std::collections::HashSet<&str> =
            files.iter().map(|(r, _)| r.as_str()).collect();
        for (rel, hash) in old {
            if wanted.contains(rel.as_str()) {
                // a failed write keeps its old ownership
                ledger.entry(rel.clone()).or_insert_with(|| hash.clone());
                continue;
            }
            let path = at(rel);
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

    let bytes = canonical::to_vec(&serde_json::to_value(&ledger).expect("ledger json"));
    let lp = root.join(LEDGER_FILE);
    if fs::read(&lp).ok().as_deref() != Some(&bytes)
        && let Err(e) = write_file(&lp, &mut |w| w.write_all(&bytes))
    {
        diags.push(fail(LEDGER_FILE, &e));
    }
    (rep, diags)
}
