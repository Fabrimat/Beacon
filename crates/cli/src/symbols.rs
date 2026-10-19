//! `beacon rename <from> <to>` and `beacon symbols` (5.7): both read the symbol table of a
//! `check` run (restricted unless the project is trusted). Rename writes all files or none.

use super::{BuildArgs, Opts};
use beacon_engine::diag::{Diagnostic, exit};
use beacon_engine::symbols::{self, SYMBOLS_JSON_SCHEMA, SymbolKind, SymbolTable, SymbolsJson};
use serde_json::json;

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
        let files = match symbols::plan(&o.project, t, &fixes) {
            Ok(f) => f,
            Err(d) => return fail(d),
        };
        if !dry_run && let Err(d) = symbols::write_plan(&o.project, &files) {
            eprintln!("error: {}", d.message);
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
                    .find(|e| e.path == f.path)
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
