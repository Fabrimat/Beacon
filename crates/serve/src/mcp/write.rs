//! Write tools. Every one takes `dryRun` (default true: a read, no grant needed); with
//! `dryRun: false` it needs the connection's grant (`GrantRequired`). None touches a protected
//! file except through its owning command, and none overwrites a file an LSP client has open
//! with unsaved changes (`DocumentDirty`).

use super::read::kind_arg;
use super::{arg, bad_args, failure, grant_name, ok, text};
use crate::Grant;
use crate::daemon::{EngineHandle, EngineState};
use beacon_engine::build::{BuildOptions, Bump, ReleaseOptions};
use beacon_engine::config::{Artifact, ProjectConfig};
use beacon_engine::diag::{Code, Diagnostic, Fix, Severity};
use beacon_engine::lock::Lockfile;
use beacon_engine::path::RelPath;
use beacon_engine::scaffold::{self, Edit, Kind, generated_file};
use beacon_engine::{agents, route, settings, symbols};
use serde_json::{Value, json};
use std::collections::BTreeSet;
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

/// The edits of a rename and the files they touch, verified against the disk; the failure is
/// the tool result.
fn plan_rename(
    st: &mut EngineState,
    t: bool,
    root: &Path,
    kind: Option<symbols::SymbolKind>,
    from: &str,
    to: &str,
    diags: &[Diagnostic],
) -> Result<(Vec<Fix>, Vec<symbols::FileEdit>), Value> {
    let Some(table) = st.session(t).symbols() else {
        return Err(failure(diags));
    };
    let fixes = symbols::rename(table, kind, from, to).map_err(|d| failure(&[d]))?;
    for f in &fixes {
        if let Some(d) = refuse(&|p| st.has_overlay(p), root, &f.path) {
            return Err(failure(&[d]));
        }
    }
    let files = match st.session(t).symbols() {
        Some(table) => symbols::plan(root, table, &fixes).map_err(|d| failure(&[d]))?,
        None => return Err(failure(diags)),
    };
    Ok((fixes, files))
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
        // A real rename always checks first: `plan` re-verifies only the ranges the table knows,
        // so a reference added since the last scan would be missed. A dry run checks only when
        // the table is missing, or stale (the watcher lagged a save, so a range no longer holds
        // its text).
        let mut checked = !dry || st.session(t).symbols().is_none();
        let mut diags = if checked {
            st.session(t).check().diagnostics
        } else {
            vec![]
        };
        let (fixes, files) = loop {
            match plan_rename(st, t, &root, kind, &from, &to, &diags) {
                Ok(r) => break r,
                Err(_) if !checked => {
                    checked = true;
                    diags = st.session(t).check().diagnostics;
                }
                Err(e) => return e,
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

/// `write_settings`: replace one module's `module.json` with `value`, keeping its key order and
/// indentation, against the `base` hash the caller read (`DocumentStale` when the file moved on).
pub(super) fn write_settings(h: &EngineHandle, a: &Value) -> Value {
    let rel = match arg(a, "path").map(RelPath::parse) {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return bad_args(&e.0),
        Err(e) => return e,
    };
    let base = match arg(a, "base") {
        Ok(b) => b.to_owned(),
        Err(e) => return e,
    };
    let value = a["value"].clone();
    let dry = dry_run(a);
    if let Err(e) = need(h, "write_settings", Grant::Edit, dry) {
        return e;
    }
    let h2 = h.clone();
    h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        if let Some(d) = refuse(&|p| st.has_overlay(p), &root, &rel) {
            return failure(&[d]);
        }
        let done = if dry {
            settings::plan(&root, &rel, &base, &value).map(|(_, d)| d)
        } else {
            settings::write(&root, &rel, &base, &value)
        };
        match done {
            Ok(d) => {
                if !dry {
                    refresh(st, t, [PathBuf::from(rel.as_str())]);
                }
                ok(json!({ "dryRun": dry, "path": rel.as_str(), "base": d.base, "value": d.value }))
            }
            Err(d) => failure(&[d]),
        }
    })
}

/// What a real `pack` checks before writing and `check` skips, for a dry run, without writing
/// anything: the lockfile under `frozen` (the `debug_config` pattern), `--pack` ids against the
/// packs of the release, and `--set-version` against the released versions.
/// ponytail: world/skin lock identities and content-dependent staleness (needs hashes of the
/// built packs) are left to the real run.
fn validate_release(
    root: &Path,
    cfg: &ProjectConfig,
    target: Option<&str>,
    all: bool,
    only: &[String],
    set_version: Option<[u32; 3]>,
) -> Vec<Diagnostic> {
    let is_static = |id: &str| {
        cfg.beacon
            .outputs
            .iter()
            .any(|o| o.id == id && o.static_packs.is_some())
    };
    let all_outputs: Vec<String> = cfg.beacon.outputs.iter().map(|o| o.id.clone()).collect();
    let minted: Vec<String> = all_outputs
        .iter()
        .filter(|o| !is_static(o))
        .cloned()
        .collect();
    let (lock, mut out) = match Lockfile::ensure(root, cfg, &minted, &[], &[], true) {
        Ok(l) => (Some(l), vec![]),
        Err(d) => (None, vec![d]),
    };
    // (outputs, world folder, skins folder) of each release plan; none for a dev target
    let mut plans: Vec<(Vec<String>, Option<String>, Option<String>)> = vec![];
    match (target, all) {
        (None, false) => plans.push((all_outputs.clone(), None, None)),
        (t, _) => {
            for (name, d) in &cfg.beacon.targets {
                if t.is_some_and(|t| t != name) || d.artifact == Artifact::Dev {
                    continue;
                }
                let outs = match &d.outputs {
                    Some(l) if !l.iter().any(|o| o == "*") => all_outputs
                        .iter()
                        .filter(|o| l.contains(o))
                        .cloned()
                        .collect(),
                    _ => all_outputs.clone(),
                };
                let trim = |s: &Option<String>| {
                    s.as_deref()
                        .map(|s| s.trim_start_matches("./").trim_end_matches('/').to_owned())
                };
                let (world, skins) = match d.artifact {
                    Artifact::McTemplate => (trim(&d.world.dir().cloned()), None),
                    Artifact::Marketplace => (trim(&d.world.dir().cloned()), trim(&d.skins)),
                    Artifact::SkinPack => (None, trim(&d.skins)),
                    _ => (None, None),
                };
                plans.push((outs, world, skins));
            }
        }
    }
    let mut ids = BTreeSet::new();
    for (outs, world, skins) in &plans {
        for o in outs.iter().filter(|o| !is_static(o)) {
            ids.extend([o.clone(), format!("{o}/BP"), format!("{o}/RP")]);
        }
        ids.extend(world.iter().map(|w| format!("world:{w}")));
        ids.extend(skins.iter().map(|s| format!("skins:{s}")));
    }
    if let Some(bad) = only.iter().find(|o| !ids.contains(*o)) {
        let valid: Vec<&str> = ids.iter().map(String::as_str).collect();
        out.push(Diagnostic::new(
            Code::ConfigField,
            format!(
                "--pack {bad:?} matches no pack of this release; valid ids: {}",
                valid.join(", ")
            ),
        ));
    }
    if let (Some(set), Some(lock)) = (set_version, &lock) {
        for (o, ol) in lock.outputs.iter().filter(|(o, _)| ids.contains(*o)) {
            for e in ol.shared.iter().chain(ol.editions.values()) {
                for (suffix, pack) in [("BP", &e.bp), ("RP", &e.rp)] {
                    let key = format!("{o}/{suffix}");
                    let mine = only.is_empty() || only.iter().any(|x| *x == key || x == o);
                    let mut released = pack.release.iter().chain(pack.releases.values());
                    if let Some(r) = released.find(|r| mine && set < r.version) {
                        out.push(Diagnostic::new(
                            Code::PackVersionNotMonotonic,
                            format!(
                                "{key}: {}.{}.{} is lower than the released {}.{}.{}",
                                set[0], set[1], set[2], r.version[0], r.version[1], r.version[2]
                            ),
                        ));
                    }
                }
            }
        }
    }
    out
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
    let only_ids = only.clone();
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
        let mut r = if dry { s.check() } else { s.build() };
        if dry && flavor == Flavor::Pack {
            let root = st.root().to_path_buf();
            let tgt = target.as_deref();
            r.diagnostics.extend(validate_release(&root, &cfg, tgt, all, &only_ids, set_version));
        }
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
    use crate::daemon::tests::test_handle;

    /// A copy of `fixtures/rename` (a restricted session indexes it without generators).
    fn project() -> (tempfile::TempDir, PathBuf) {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap() {
                let e = e.unwrap();
                let dest = to.join(e.file_name());
                if e.path().is_dir() {
                    copy(&e.path(), &dest);
                } else {
                    std::fs::copy(e.path(), dest).unwrap();
                }
            }
        }
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rename"),
            &root,
        );
        (d, root)
    }

    #[test]
    fn dry_rename_checks_only_a_missing_or_stale_table() {
        let (_d, root) = project();
        let h = test_handle(root);
        let gen_after = |h: &EngineHandle| h.call(|st| st.session(false).check().generation);
        let a = json!({ "from": "demo:car", "to": "demo:truck" });
        let g0 = gen_after(&h); // the table exists now
        let r = rename_symbol(&h, &a);
        assert_eq!(r["isError"], false, "{r}");
        // exactly our own check ran in between: the dry runs did not check
        rename_symbol(&h, &a);
        assert_eq!(gen_after(&h), g0 + 1);
        // no table yet: the first call checks (a different, fresh engine)
        let (_d2, root2) = project();
        let h2 = test_handle(root2);
        let r = rename_symbol(&h2, &a);
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(gen_after(&h2), 2);
    }

    #[test]
    fn dry_pack_rejects_an_unknown_pack_id() {
        let (_d, root) = project();
        let h = test_handle(root);
        let r = build(&h, &json!({ "pack": ["nope"] }), Flavor::Pack);
        assert_eq!(r["isError"], true, "{r}");
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(t.contains("matches no pack of this release"), "{t}");
        // a real id passes that check
        let r = build(&h, &json!({ "pack": ["main/BP"] }), Flavor::Pack);
        let t = r["content"][0]["text"].as_str().unwrap();
        assert!(!t.contains("matches no pack"), "{t}");
    }

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
