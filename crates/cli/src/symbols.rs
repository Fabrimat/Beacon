//! `beacon rename <from> <to>` and `beacon symbols` (5.7): both read the symbol table of a
//! `check` run (restricted unless the project is trusted). Rename writes all files or none.

use super::{BuildArgs, Opts};
use beacon_engine::diag::{Code, Diagnostic, Fix, exit};
use beacon_engine::symbols::{self, SYMBOLS_JSON_SCHEMA, SymbolKind, SymbolTable, SymbolsJson};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};

/// `--kind` values: the `symbols --json` kind names (`entity`, `lang_key`, ...).
pub fn parse_kind(s: &str) -> Result<SymbolKind, String> {
    serde_json::from_value(s.into()).map_err(|_| {
        format!("unknown kind {s:?} (entity, item, block, lang_key, function, animation, animation_controller, render_controller, texture, sound)")
    })
}

/// Runs `check` (stored trust, nothing written) and hands its symbol table to `f`.
fn with_table(o: &Opts, f: impl FnOnce(&SymbolTable) -> i32) -> i32 {
    let a = BuildArgs {
        target: None,
        all: false,
        out: None,
        trust: false,
        frozen: false,
    };
    let mut s = super::session(o, &a);
    let r = s.check();
    match s.symbols() {
        Some(t) => f(t),
        None => {
            super::print_diags(&r.diagnostics);
            super::exit_code(&r.diagnostics).max(exit::DIAGNOSTICS)
        }
    }
}

pub fn symbols(o: &Opts, find: Option<&str>, kind: Option<SymbolKind>) -> i32 {
    with_table(o, |t| {
        let kinds: Vec<SymbolKind> = kind.into_iter().collect();
        let list = t.find(find.unwrap_or(""), &kinds, usize::MAX);
        if o.json {
            let j = SymbolsJson {
                schema: SYMBOLS_JSON_SCHEMA,
                symbols: list,
            };
            println!("{}", serde_json::to_string(&j).unwrap());
        } else {
            for e in list {
                let at = e
                    .definitions
                    .first()
                    .map_or(String::new(), |d| match d.line {
                        Some(l) => format!("{}:{l}", d.path),
                        None => d.path.to_string(),
                    });
                let kind = serde_json::to_value(e.kind).unwrap();
                println!(
                    "{:<20} {:<40} {at} ({} references)",
                    kind.as_str().unwrap_or_default(),
                    e.name,
                    e.references
                );
            }
        }
        exit::OK
    })
}

/// 1-based line and byte column of `offset`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..offset];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let col = offset
        - before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1)
        + 1;
    (line, col)
}

/// A file with its edits applied, or why not (nothing is written then).
struct Edited {
    path: PathBuf,
    old: Vec<u8>,
    new: String,
}

/// Applies `fixes` (sorted, per file) in memory, checking that every range still holds the
/// text the table saw (`from`, or its local name for template heads).
fn edit(root: &Path, from: &str, to: &str, fixes: &[Fix]) -> Result<Vec<Edited>, String> {
    let local = |s: &str| s.split_once(':').map_or("", |(_, n)| n).to_owned();
    let mut out: Vec<Edited> = Vec::new();
    for group in fixes.chunk_by(|a, b| a.path == b.path) {
        let rel = &group[0].path;
        let path = root.join(rel.as_str());
        let old = fs::read(&path).map_err(|e| format!("cannot read {rel}: {e}"))?;
        let mut new =
            String::from_utf8(old.clone()).map_err(|_| format!("{rel} is not UTF-8 text"))?;
        for f in group.iter().rev() {
            let want = if f.replacement == to {
                from.to_owned()
            } else {
                local(from)
            };
            if new.get(f.start..f.end) != Some(want.as_str()) {
                return Err(format!(
                    "{rel} changed since it was scanned (expected {want:?} at byte {}); run again",
                    f.start
                ));
            }
            new.replace_range(f.start..f.end, &f.replacement);
        }
        out.push(Edited { path, old, new });
    }
    Ok(out)
}

/// Writes every file or none: all temp files first, then renames; a failed rename restores
/// the files already replaced.
fn write_all(files: &[Edited]) -> Result<(), String> {
    let tmp = |p: &Path| {
        let mut t = p.as_os_str().to_owned();
        t.push(format!(".{}.rename.tmp", std::process::id()));
        PathBuf::from(t)
    };
    for (i, f) in files.iter().enumerate() {
        if let Err(e) = fs::write(tmp(&f.path), &f.new) {
            for g in &files[..=i] {
                let _ = fs::remove_file(tmp(&g.path));
            }
            return Err(format!("cannot write {}: {e}", f.path.display()));
        }
    }
    for (i, f) in files.iter().enumerate() {
        if let Err(e) = fs::rename(tmp(&f.path), &f.path) {
            for g in &files[..i] {
                let _ = fs::write(&g.path, &g.old);
            }
            for g in &files[i..] {
                let _ = fs::remove_file(tmp(&g.path));
            }
            return Err(format!("cannot replace {}: {e}", f.path.display()));
        }
    }
    Ok(())
}

pub fn rename(o: &Opts, from: &str, to: &str, kind: Option<SymbolKind>, dry_run: bool) -> i32 {
    with_table(o, |t| {
        let fail = |d: Diagnostic| {
            if o.json {
                let j = json!({ "schema": 1, "from": from, "to": to, "dryRun": dry_run,
                    "applied": false, "edits": [], "diagnostics": [d] });
                println!("{j}");
            } else {
                super::print_diags(std::slice::from_ref(&d));
            }
            exit::DIAGNOSTICS
        };
        let fixes = match symbols::rename(t, kind, from, to) {
            Ok(f) => f,
            Err(d) => return fail(d),
        };
        let files = match edit(&o.project, from, to, &fixes) {
            Ok(f) => f,
            Err(m) => return fail(Diagnostic::new(Code::RenameBlocked, m)),
        };
        if !dry_run && let Err(m) = write_all(&files) {
            eprintln!("error: {m}; nothing was renamed");
            return exit::INTERNAL;
        }
        if o.json {
            let j = json!({ "schema": 1, "from": from, "to": to, "dryRun": dry_run,
                "applied": !dry_run, "edits": fixes, "diagnostics": [] });
            println!("{j}");
        } else {
            for f in &fixes {
                let text = files
                    .iter()
                    .find(|e| e.path == o.project.join(f.path.as_str()))
                    .map(|e| String::from_utf8_lossy(&e.old).into_owned())
                    .unwrap_or_default();
                let (l, c) = line_col(&text, f.start.min(text.len()));
                println!(
                    "{}:{l}:{c}: {} -> {}",
                    f.path,
                    &text[f.start..f.end.min(text.len())],
                    f.replacement
                );
            }
            let verb = if dry_run { "would rename" } else { "renamed" };
            println!(
                "{verb} {from} to {to}: {} edits in {} files{}",
                fixes.len(),
                files.len(),
                if dry_run {
                    " (dry run, nothing written)"
                } else {
                    ""
                }
            );
        }
        exit::OK
    })
}
