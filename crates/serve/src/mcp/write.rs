//! Write tools. Every one takes `dryRun` (default true: a read, no grant needed); with
//! `dryRun: false` it needs the connection's grant (`GrantRequired`). None touches a protected
//! file except through its owning command, and none overwrites a file an LSP client has open
//! with unsaved changes (`DocumentDirty`).

use super::read::kind_arg;
use super::{arg, bad_args, failure, grant_name, ok, text};
use crate::Grant;
use crate::daemon::{EngineHandle, EngineState};
use beacon_engine::build::{BuildOptions, Bump, ReleaseOptions};
use beacon_engine::config::Artifact;
use beacon_engine::diag::{Code, Diagnostic, Fix, Severity};
use beacon_engine::path::RelPath;
use beacon_engine::scaffold::{self, Edit, Kind, generated_file};
use beacon_engine::{agents, route, symbols};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    Build,
    Deploy,
    Pack,
}

/// `dryRun` defaults to true.
fn dry_run(a: &Value) -> bool {
    a["dryRun"].as_bool().unwrap_or(true)
}

/// A real write without its grant: `GrantRequired`, naming the flag.
fn need(h: &EngineHandle, tool: &str, g: Grant, dry: bool) -> Result<(), Value> {
    if dry || h.allows(g) {
        return Ok(());
    }
    let f = grant_name(g);
    Err(failure(&[Diagnostic::new(
        Code::GrantRequired,
        format!(
            "`{tool}` with dryRun: false needs the operator's `{f}` grant: start the server with `--allow {f}` (beacon serve --stdio --mcp --allow {f}); dryRun: true previews without it"
        ),
    )]))
}

/// Why a tool must not write `rel`: a protected file, or a document with unsaved changes.
fn refuse(open: &dyn Fn(&RelPath) -> bool, root: &Path, rel: &RelPath) -> Option<Diagnostic> {
    if let Some(why) = generated_file(Some(root), &root.join(rel.as_str()), &Edit::Opaque) {
        return Some(Diagnostic::new(
            Code::RenameBlocked,
            format!("refusing to write {rel}: {why}"),
        ));
    }
    dirty(open, rel)
}

/// `DocumentDirty`: the file is open with unsaved changes in an LSP client (a symbol-table
/// overlay, `EngineState::has_overlay`, either session). A table/disk mismatch (`symbols::plan`) is the
/// fallback guard: an overlay-scanned file no longer matches the text on disk.
fn dirty(open: &dyn Fn(&RelPath) -> bool, rel: &RelPath) -> Option<Diagnostic> {
    open(rel).then(|| {
        Diagnostic::new(
            Code::DocumentDirty,
            format!(
                "{rel} has unsaved changes in an editor; save or revert the buffer, then retry"
            ),
        )
    })
}

/// After a write: tell the session what changed and re-check, so the next read is fresh (the
/// watcher would get there too; this does not wait for it).
fn refresh(st: &mut EngineState, trusted: bool, files: impl IntoIterator<Item = PathBuf>) {
    let root = st.root().to_path_buf();
    let s = st.session(trusted);
    s.changed(files.into_iter().map(|f| root.join(f)));
    s.check();
}

pub(super) fn rename_symbol(h: &EngineHandle, a: &Value) -> Value {
    let (from, to) = match (arg(a, "from"), arg(a, "to")) {
        (Ok(f), Ok(t)) => (f.to_owned(), t.to_owned()),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let kind = match kind_arg(a) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let dry = dry_run(a);
    if let Err(e) = need(h, "rename_symbol", Grant::Edit, dry) {
        return e;
    }
    let h2 = h.clone(); // trust is read on the engine thread, after any demotion
    h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        let diags = st.session(t).check().diagnostics; // fresh table: the watcher may lag a save
        let fixes = {
            let Some(table) = st.session(t).symbols() else {
                return failure(&diags);
            };
            match symbols::rename(table, kind, &from, &to) {
                Ok(f) => f,
                Err(d) => return failure(&[d]),
            }
        };
        for f in &fixes {
            if let Some(d) = refuse(&|p| st.has_overlay(p), &root, &f.path) {
                return failure(&[d]);
            }
        }
        // all-or-nothing; also the dry run's check that every range still holds its text
        let files = {
            let table = st.session(t).symbols().expect("checked above");
            match symbols::plan(&root, table, &fixes) {
                Ok(f) => f,
                Err(d) => return failure(&[d]),
            }
        };
        let paths: Vec<RelPath> = files.iter().map(|f| f.path.clone()).collect();
        if dry {
            return ok(json!({ "dryRun": true, "applied": false, "edits": fixes, "files": paths }));
        }
        match symbols::write_plan(&root, &files) {
            Ok(()) => {
                refresh(st, t, paths.iter().map(|p| PathBuf::from(p.as_str())));
                ok(json!({ "dryRun": false, "applied": true, "edits": fixes, "files": paths }))
            }
            Err(d) => failure(&[d]),
        }
    })
}

/// Safe fix-its: today the generated AGENTS.md section (8.3), which `fix` owns.
pub(super) fn fix(h: &EngineHandle, a: &Value) -> Value {
    let dry = dry_run(a);
    if let Err(e) = need(h, "fix", Grant::Edit, dry) {
        return e;
    }
    let h2 = h.clone(); // trust is read on the engine thread, after any demotion
    h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        let diags = route::load(&root)
            .map(|c| agents::check(&root, &c))
            .unwrap_or_default();
        let fixes: Vec<&Fix> = diags
            .iter()
            .flat_map(|d| &d.fixes)
            .filter(|f| f.safe)
            .collect();
        if let Some(d) = fixes
            .iter()
            .find_map(|f| dirty(&|p| st.has_overlay(p), &f.path))
        {
            return failure(&[d]);
        }
        if dry {
            return ok(json!({ "dryRun": true, "applied": 0, "fixes": fixes }));
        }
        match agents::apply_safe_fixes(&root, &diags) {
            Ok(n) => {
                let files: Vec<PathBuf> = fixes
                    .iter()
                    .map(|f| PathBuf::from(f.path.as_str()))
                    .collect();
                refresh(st, t, files);
                ok(json!({ "dryRun": false, "applied": n }))
            }
            Err(m) => text(m, true),
        }
    })
}

pub(super) fn add_content(h: &EngineHandle, a: &Value) -> Value {
    let (kind, name) = match (arg(a, "kind"), arg(a, "name")) {
        (Ok(k), Ok(n)) => (k, n.to_owned()),
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let kind = match kind {
        "entity" => Kind::Entity,
        "item" => Kind::Item,
        "block" => Kind::Block,
        "module" => Kind::Module,
        k => return bad_args(&format!("unknown kind `{k}` (entity, item, block, module)")),
    };
    let module = a["module"].as_str().map(str::to_owned);
    let force = a["forceConvention"].as_bool().unwrap_or(false);
    let dry = dry_run(a);
    if let Err(e) = need(h, "add_content", Grant::Edit, dry) {
        return e;
    }
    let h2 = h.clone(); // trust is read on the engine thread, after any demotion
    h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        let planned = match scaffold::plan_add(&root, kind, &name, module.as_deref(), force) {
            Ok(p) => p,
            Err((_, m)) => return text(m.trim_end().to_owned(), true),
        };
        for f in &planned.files {
            let Ok(rel) = RelPath::parse(f.trim_end_matches('/')) else {
                continue;
            };
            if let Some(d) = refuse(&|p| st.has_overlay(p), &root, &rel) {
                return failure(&[d]);
            }
        }
        let added = if dry {
            planned
        } else {
            match scaffold::add(&root, kind, &name, module.as_deref(), force) {
                Ok(p) => p,
                Err((_, m)) => return text(m.trim_end().to_owned(), true),
            }
        };
        if !dry {
            let files = added.files.iter().map(|f| PathBuf::from(f.trim_end_matches('/')));
            refresh(st, t, files.collect::<Vec<_>>());
        }
        ok(json!({ "dryRun": dry, "module": added.module, "files": added.files, "note": added.note }))
    })
}

/// `build`, `deploy` and `pack`: a throwaway session with the connection's trust; the output
/// is the build report. `dryRun` runs the same pipeline as `check` for the chosen target and
/// release options: diagnostics and nothing written (the engine has no emit plan yet).
pub(super) fn build(h: &EngineHandle, a: &Value, flavor: Flavor) -> Value {
    let (tool, grant) = match flavor {
        Flavor::Build => ("build", Grant::Build),
        Flavor::Deploy => ("deploy", Grant::Deploy),
        Flavor::Pack => ("pack", Grant::Release),
    };
    let dry = dry_run(a);
    if let Err(e) = need(h, tool, grant, dry) {
        return e;
    }
    let bump = match a["bump"].as_str() {
        None => None,
        Some("minor") => Some(Bump::Minor),
        Some("major") => Some(Bump::Major),
        Some(b) => return bad_args(&format!("unknown bump `{b}` (minor, major)")),
    };
    let set_version = match a["setVersion"].as_str() {
        None => None,
        Some(s) => {
            let v: Vec<Option<u32>> = s.split('.').map(|n| n.parse().ok()).collect();
            match v[..] {
                [Some(x), Some(y), Some(z)] => Some([x, y, z]),
                _ => return bad_args(&format!("setVersion {s:?} is not x.y.z")),
            }
        }
    };
    let only: Vec<String> = a["pack"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let all = flavor == Flavor::Pack && a["all"].as_bool().unwrap_or(false);
    let mut target = a["target"].as_str().map(str::to_owned);
    let h2 = h.clone(); // trust is read on the engine thread, after any demotion
    h.call(move |st| {
        let trusted = h2.trusted();
        let cfg = match route::load(st.root()) {
            Ok(c) => c,
            Err(d) => return failure(&d),
        };
        let is_dev = |t: &str| cfg.beacon.targets.get(t).is_some_and(|d| d.artifact == Artifact::Dev);
        match flavor {
            Flavor::Build if target.as_deref().is_some_and(is_dev) => {
                return text("`build` is for non-dev targets; use `deploy` for a dev target".into(), true);
            }
            Flavor::Deploy => {
                let dev = target.clone().or_else(|| {
                    cfg.beacon.targets.iter().find(|(_, d)| d.artifact == Artifact::Dev).map(|(n, _)| n.clone())
                });
                match dev {
                    Some(d) if is_dev(&d) => target = Some(d),
                    Some(d) => return text(format!("`deploy` needs a dev target; `{d}` is not one"), true),
                    None => return text("no dev target in beacon.targets".into(), true),
                }
            }
            _ => {}
        }
        let opts = BuildOptions {
            target: target.clone(),
            all,
            out: None, // never a client-chosen destination
            frozen: false,
            trusted,
            release: (flavor == Flavor::Pack).then_some(ReleaseOptions { bump, set_version, only }),
        };
        let mut s = st.fresh_session(opts);
        let r = if dry { s.check() } else { s.build() };
        let errors = r.diagnostics.iter().filter(|d| d.severity == Severity::Error).count();
        let mut out = json!({
            "dryRun": dry, "target": target, "emitted": r.report.emitted,
            "artifacts": r.report.artifacts, "packs": r.report.packs,
            "errors": errors, "diagnostics": r.diagnostics,
        });
        if dry {
            out["note"] = json!("dryRun validates and writes nothing; the emitted file diff needs engine support that is not there yet");
        }
        text(out.to_string(), errors > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_and_dirty_files_are_refused() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.json"), r#"{"beacon":{}}"#).unwrap();
        let rel = |s| RelPath::parse(s).unwrap();
        let root = d.path();
        let none = |_: &RelPath| false;
        let lock = refuse(&none, root, &rel("beacon.lock.json")).unwrap();
        assert_eq!(lock.code, Code::RenameBlocked);
        assert!(refuse(&none, root, &rel("src/a/entities/x.entity.json")).is_none());
        let open = |p: &RelPath| p.as_str() == "src/a/entities/x.entity.json";
        let dirty = refuse(&open, root, &rel("src/a/entities/x.entity.json")).unwrap();
        assert_eq!(dirty.code, Code::DocumentDirty);
    }
}
