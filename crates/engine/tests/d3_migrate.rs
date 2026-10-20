//! Phase 3 B1: `migrate::plan` / `apply` on the system_template fixture (no Deno, no Python:
//! its committed real output is the `--evaluated` tree), `--out` validation, determinism,
//! and the first release after migration.

mod b1_common;
use b1_common::{errors, has, session};
use beacon_engine::build::{BuildOptions, ReleaseOptions};
use beacon_engine::diag::Code;
use beacon_engine::migrate::{
    self, Change, LegacyProject, MigrateOptions, MigrationPlan, ToPortKind, plan::OUTPUT,
    system_template, write,
};
use beacon_engine::model::Body;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/migrate")
}

fn st() -> (LegacyProject, PathBuf) {
    let p = migrate::detect(&fixtures().join("system_template")).unwrap();
    (p, fixtures().join("system_template.expected"))
}

fn opts(out: &Path) -> MigrateOptions {
    MigrateOptions {
        out: out.to_path_buf(),
        evaluated: Some(fixtures().join("system_template.expected")),
        ..Default::default()
    }
}

fn plan_st(out: &Path) -> MigrationPlan {
    let (p, ev) = st();
    let eval = system_template::ingest(&p, &ev).unwrap();
    assert!(eval.diagnostics.is_empty(), "{:?}", eval.diagnostics);
    migrate::plan(&p, &eval, &opts(out)).unwrap()
}

fn copies(plan: &MigrationPlan) -> Vec<(String, String)> {
    plan.changes
        .iter()
        .filter_map(|c| match c {
            Change::Copy { from, to } => Some((to.to_string(), from.to_string())),
            Change::Write { to, body } => Some((
                to.to_string(),
                match body {
                    Body::Passthrough(p) => p.to_string(),
                    _ => "<write>".into(),
                },
            )),
            _ => None,
        })
        .collect()
}

fn listing(root: &Path) -> Vec<String> {
    let mut v = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                todo.push(e.path());
            } else {
                v.push(e.path().display().to_string());
            }
        }
    }
    v.sort();
    v
}

#[test]
fn round_trip_equals_the_real_output() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("migrated");
    let plan = plan_st(&out);
    let (p, _) = st();
    migrate::apply(&p, &plan, &opts(&out)).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let bo = BuildOptions {
        out: Some(t.path().join("built")),
        trusted: true,
        ..Default::default()
    };
    let r = session(bo, &out, &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let rep = write::oracle_compare(
        &fixtures().join("system_template.expected"),
        &t.path().join("built").join(OUTPUT),
        &plan,
    );
    assert_eq!(rep.diffs, [], "{rep:?}");
    assert_eq!(rep.compared, 19, "{rep:?}"); // every BP/RP file, manifests without identities
    // a translated subfunctions source is a plain copy; the build expands it from the module scope
    let src = out.join("src/golem");
    assert_eq!(
        std::fs::read(src.join("waves.mcfunction")).unwrap(),
        std::fs::read(
            fixtures().join("system_template/data/system_template/golem/waves.mcfunction")
        )
        .unwrap()
    );
    let scope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(src.join("scope.json")).unwrap()).unwrap();
    assert_eq!(scope, serde_json::json!({"label": "Golem", "waves": 3}));
    // `if` is expanded too (B10 slice 2): the gated one is a plain copy as well
    assert_eq!(
        std::fs::read(src.join("gated.mcfunction")).unwrap(),
        std::fs::read(
            fixtures().join("system_template/data/system_template/golem/gated.mcfunction")
        )
        .unwrap()
    );
    assert!(!src.join("bp/functions/golem").exists());
    // a `.lang` template is translated the same way; the dict `foreach` one keeps the expansion
    assert_eq!(
        std::fs::read(src.join("fr_FR.lang")).unwrap(),
        std::fs::read(fixtures().join("system_template/data/system_template/golem/fr_FR.lang"))
            .unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(src.join("rp/texts/de_DE.lang")).unwrap(),
        "golem.icon0=alpha
golem.icon1=beta"
    );
    assert!(!out.join("src/_generated").exists());
    // the legacy project is untouched and nothing escaped `out`
    assert!(!fixtures().join("system_template/src").exists());
}

#[test]
fn placement_decisions() {
    let t = tempfile::tempdir().unwrap();
    let plan = plan_st(&t.path().join("m"));
    let c = copies(&plan);
    let has = |to: &str, from: &str| c.iter().any(|(t, f)| t == to && f == from);
    let d = "data/system_template";
    // overwrite: the winner reproduces by convention, kept at its module root
    assert!(has(
        "src/ruby/golem.recipe.json",
        &format!("{d}/ruby/golem.recipe.json")
    ));
    // plain copies whose suffix rules differ: pack roots
    assert!(has(
        "src/golem/rp/entity/golem.entity.json",
        &format!("{d}/golem/sub/golem.entity.json")
    ));
    assert!(has(
        "src/golem/bp/items/shared.item.json",
        &format!("{d}/golem/shared.item.json")
    ));
    // templates: evaluated, static
    assert!(has("src/golem/bp/items/paint.json", "<write>"));
    assert!(has("src/golem/bp/spawn_rules/golem_red.json", "<write>"));
    // merge / append fragments
    assert!(has(
        "src/golem/en_US.lang",
        &format!("{d}/golem/en_US.lang")
    ));
    assert!(has("src/ruby/en_US.lang", &format!("{d}/ruby/en_US.lang")));
    assert!(has(
        "src/ruby/rp/textures/item_texture.json",
        &format!("{d}/ruby/item_texture.json")
    ));
    let mj = plan
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Write {
                to,
                body: Body::Json(v),
            } if to.as_str() == "src/ruby/module.json" => Some(v.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(mj["packRoots"]["BP"], "bp");
    assert_eq!(mj["conflicts"]["rp/textures/item_texture.json"], "merge");
    assert_eq!(mj["autoTextures"], false);
    // the base manifests are the lockfile, not files
    assert!(!c.iter().any(|(t, _)| t.ends_with("manifest.json")));
    let lock = plan
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Lock(l) => Some(l.clone()),
            _ => None,
        })
        .unwrap();
    let e = &lock.outputs[OUTPUT].editions[&beacon_engine::model::Edition::Bedrock];
    assert_eq!(e.bp.header, "8a2e4c61-7d15-4f3a-b0c9-2e5d7f1a3c01");
    assert_eq!(e.rp.module, "8a2e4c61-7d15-4f3a-b0c9-2e5d7f1a3d02");
    assert_eq!(e.bp.release.as_ref().unwrap().hash, "");
    let cfg = plan
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Config(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(cfg["namespace"], "stf");
    assert_eq!(cfg["targetVersion"], "1.26.50");
    assert_eq!(cfg["beacon"]["targets"]["dev"]["artifact"], "dev");
    assert_eq!(
        cfg["beacon"]["packMeta"]["manifest"]["BP"]["header"]["name"],
        "ST Fixture BP"
    );
    let r = &plan.report;
    assert!(
        r.dropped
            .iter()
            .any(|e| e.path == "BP/items/shared.item.json")
    );
    assert!(r.dropped.iter().any(|e| e.path == "BP/recipes/golem.json"));
    assert!(
        r.dropped
            .iter()
            .any(|e| e.path == "data/st_notes/golem.txt")
    );
    assert!(r.flattened.iter().any(|e| e.path == "BP/items/paint.json"));
    // to_port: the list comprehension / fan-out of golem (`gated`'s `if` now translates)
    assert_eq!(
        r.to_port
            .iter()
            .map(|t| (t.kind, t.path.to_string()))
            .collect::<Vec<_>>(),
        [
            (
                ToPortKind::GeneratorCandidate,
                "data/system_template/golem/_map.py".to_string()
            ),
            (
                ToPortKind::SubfunctionSource,
                "data/system_template/golem/de_DE.lang".to_string()
            )
        ]
    );
    let de = r
        .to_port
        .iter()
        .find(|t| t.path.as_str().ends_with("de_DE.lang"));
    let why = &de.unwrap().why;
    assert!(why.contains("iterating a dict"), "{why}");
    assert!(why.contains("`icons`"), "{why}");
    // translated: copied, its scope names written, the legacy expansion not imported
    assert!(has(
        "src/golem/waves.mcfunction",
        &format!("{d}/golem/waves.mcfunction")
    ));
    assert!(has("src/golem/scope.json", "<write>"));
    assert!(has(
        "src/golem/fr_FR.lang",
        &format!("{d}/golem/fr_FR.lang")
    ));
    assert!(!has(
        "src/golem/de_DE.lang",
        &format!("{d}/golem/de_DE.lang")
    ));
    assert!(!c.iter().any(|(t, _)| t.contains("waves/boss")));
    assert!(
        plan.changes
            .iter()
            .any(|c| matches!(c, Change::Note(n) if n.code == Code::MigrateTranslated && n.message.contains("waves.mcfunction")))
    );
    assert!(
        plan.changes
            .iter()
            .any(|c| matches!(c, Change::Note(n) if n.code == Code::MigrateTranslated && n.message.contains("fr_FR.lang")))
    );
}

#[test]
fn plan_is_deterministic() {
    let t = tempfile::tempdir().unwrap();
    let a = serde_json::to_string(&plan_st(&t.path().join("a"))).unwrap();
    let b = serde_json::to_string(&plan_st(&t.path().join("a"))).unwrap();
    assert_eq!(a, b);
    let md = plan_st(&t.path().join("a"));
    let md = md
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Write {
                to,
                body: Body::Text(t),
            } if to.as_str() == "MIGRATION.md" => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert!(md.contains("GeneratorCandidate"));
    assert!(md.contains("de_DE.lang"));
    assert!(!md.contains(&t.path().display().to_string()));
}

#[test]
fn dry_run_writes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    std::fs::create_dir_all(&out).unwrap();
    let before = listing(t.path());
    let plan = plan_st(&out);
    let (p, _) = st();
    let o = MigrateOptions {
        dry_run: true,
        ..opts(&out)
    };
    migrate::apply(&p, &plan, &o).unwrap();
    assert_eq!(listing(t.path()), before);
}

/// Directory link at `link` -> `target` (junction on Windows: no privilege needed).
fn dir_link(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let ok = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J failed");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn out_must_be_outside_unlinked_and_empty() {
    let t = tempfile::tempdir().unwrap();
    let proj = t.path().join("proj");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    let bad = |out: &Path, force: bool| {
        let d = write::check_out(&proj, out, force).unwrap_err();
        assert_eq!(d[0].code, Code::MigrateOutInvalid, "{out:?}");
    };
    bad(&proj, false);
    bad(&proj.join("sub/new"), false);
    bad(t.path(), true); // contains the project
    let link = t.path().join("link");
    dir_link(&proj, &link);
    bad(&link, false);
    let other = t.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let link2 = t.path().join("link2");
    dir_link(&other, &link2);
    bad(&link2.join("new"), false);
    let full = t.path().join("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("x.txt"), "x").unwrap();
    bad(&full, false);
    write::check_out(&proj, &full, true).unwrap();
    write::check_out(&proj, &t.path().join("fresh"), false).unwrap();
}

#[test]
fn apply_refuses_changed_sources() {
    let t = tempfile::tempdir().unwrap();
    let mut plan = plan_st(&t.path().join("m"));
    plan.report.source_hash = "0".into();
    let (p, _) = st();
    let d = migrate::apply(&p, &plan, &opts(&t.path().join("m"))).unwrap_err();
    assert_eq!(d[0].code, Code::MigrateUnsupported);
    assert!(!t.path().join("m").exists());
}

#[test]
fn first_release_keeps_the_imported_version() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    let plan = plan_st(&out);
    let (p, _) = st();
    migrate::apply(&p, &plan, &opts(&out)).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let bo = BuildOptions {
        out: Some(t.path().join("rel")),
        trusted: true,
        release: Some(ReleaseOptions::default()),
        ..Default::default()
    };
    let r = session(bo, &out, &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(!has(&r, Code::PackReleaseBehind));
    for key in ["main/BP", "main/RP"] {
        let v = r.report.packs.iter().find(|v| v.pack == key).unwrap();
        assert_eq!((v.version, v.bumped), ([1, 0, 0], false), "{key}");
    }
}

/// `UNPACK:SUBFUNCTION`: the logged `X.mcfunction` is a folder `X/` in the real output.
#[test]
fn unpacked_subfunctions_are_attributed_to_their_log_entry() {
    let t = tempfile::tempdir().unwrap();
    let w = |p: &Path, s: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    };
    let proj = t.path().join("legacy");
    w(
        &proj.join("config.json"),
        r#"{"name":"sub","packs":{"behaviorPack":"./packs/BP"},"regolith":{"dataPath":"./data",
          "filterDefinitions":{"system_template":{"url":"x","version":"1"}},
          "profiles":{"default":{"filters":[{"filter":"subfunctions"},{"filter":"system_template"}]}}}}"#,
    );
    let src = "data/system_template/s/f.mcfunction";
    w(
        &proj.join(src),
        "UNPACK:SUBFUNCTION\ndefinefunction <setup>:\n    say hi\n",
    );
    w(&proj.join("data/system_template/s/_map.py"), "[]\n");
    let ev = t.path().join("eval");
    let log = |extra: &str| {
        format!(
            r#"{{"BP/functions/s/f.mcfunction":{{"sources":[{{"path":"{src}","status":"Created."}}]}}{extra}}}"#
        )
    };
    w(&ev.join("BP/functions/s/f/setup.mcfunction"), "say hi\n");
    w(&ev.join("BP/functions/s/f/nested/x.mcfunction"), "say x\n");
    // system_template_esbuild root: the legacy `main.ts` is the entry, the tree is mirrored
    let es = ev.join("data/system_template_esbuild");
    w(&es.join("main.ts"), "import \"./subscripts/a\";\n");
    w(&es.join("subscripts/a.ts"), "import \"../menu/m\";\n");
    w(&es.join("subscripts/_scope.ts"), "export {};\n");
    w(&es.join("menu/m.ts"), "export {};\n");
    // the systems' own subscripts (the filter's input, also in tmp `data/`) are no root
    w(
        &ev.join("data/system_template/s/subscripts/own.ts"),
        "export {};\n",
    );
    w(&es.join("node_modules/p/i.js"), "export {};\n");
    w(&ev.join("BP/scripts/s/main.js"), "// bundle\n");
    w(
        &ev.join("system_template_log.json"),
        &log(
            r#","BP/functions/s/gone.mcfunction":{"sources":[{"path":"data/system_template/s/f.mcfunction","status":"Created."}]}"#,
        ),
    );
    let p = migrate::detect(&proj).unwrap();
    // neither the file nor the folder: still an error
    let e = system_template::ingest(&p, &ev).unwrap();
    assert_eq!(e.diagnostics.len(), 1, "{:?}", e.diagnostics);
    assert!(e.diagnostics[0].message.contains("gone.mcfunction"));
    // system_template itself writes the esbuild entry (logged, so it has contributors)
    w(
        &ev.join("system_template_log.json"),
        &log(
            r#","data/system_template_esbuild/main.ts":{"sources":[{"path":"data/system_template/s/f.mcfunction","status":"Created."}]}"#,
        ),
    );
    let e = system_template::ingest(&p, &ev).unwrap();
    assert!(e.diagnostics.is_empty(), "{:?}", e.diagnostics);
    for o in [
        "BP/functions/s/f/setup.mcfunction",
        "BP/functions/s/f/nested/x.mcfunction",
    ] {
        assert_eq!(e.outputs[o].contributors[0].source.as_str(), src, "{o}");
    }
    let o = MigrateOptions {
        out: t.path().join("out"),
        ..Default::default()
    };
    let plan = migrate::plan(&p, &e, &o).unwrap();
    let c = copies(&plan);
    assert!(
        c.iter()
            .any(|(to, _)| to == "src/s/bp/functions/s/f/nested/x.mcfunction")
    );
    let r = &plan.report;
    assert!(
        r.flattened
            .iter()
            .any(|e| e.path == "BP/functions/s/f/setup.mcfunction"
                && e.why.contains("unpacked subfunction"))
    );
    let subs: Vec<_> = r
        .to_port
        .iter()
        .filter(|t| t.kind == ToPortKind::SubfunctionSource)
        .collect();
    assert_eq!(subs.len(), 1, "{subs:?}");
    assert_eq!(subs[0].path.as_str(), src);
    for f in ["main.ts", "subscripts/a.ts", "menu/m.ts"] {
        let to = format!("src/_scripts/system_template_esbuild/{f}");
        assert!(c.iter().any(|(t, _)| *t == to), "{to}");
    }
    assert!(!c.iter().any(|(t, _)| t.contains("node_modules")));
    assert!(!c.iter().any(|(t, _)| t.contains("own.ts")));
    let body = plan
        .changes
        .iter()
        .find_map(|ch| match ch {
            Change::Write {
                to,
                body: Body::Text(t),
            } if to.as_str() == "src/_scripts/scripts/main.ts" => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        body.ends_with("\nimport \"../system_template_esbuild/main.ts\";\n"),
        "{body}"
    );
    let cfg = plan
        .changes
        .iter()
        .find_map(|ch| match ch {
            Change::Config(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        cfg["beacon"]["outputs"][0]["scriptEntry"],
        "scripts/s/main.js"
    );
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let t = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

/// Two templates of one module read `waves` with different values (a nested `_scope.json`
/// overrides the system's): the first keeps its source and the scope, the later one is frozen
/// from the legacy output with a note; the migrated project still builds equal.
#[test]
fn templates_with_conflicting_scope_values_fall_back() {
    let t = tempfile::tempdir().unwrap();
    let (proj, ev) = (t.path().join("proj"), t.path().join("ev"));
    copy_tree(&fixtures().join("system_template"), &proj);
    copy_tree(&fixtures().join("system_template.expected"), &ev);
    let sub = proj.join("data/system_template/golem/sub");
    std::fs::write(sub.join("_scope.json"), "{\"waves\": 5}").unwrap();
    std::fs::write(sub.join("more.mcfunction"), "say `eval:waves`\n").unwrap();
    std::fs::write(ev.join("BP/functions/golem/more.mcfunction"), "say 5").unwrap();
    let log = ev.join("system_template_log.json");
    let mut l: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    l["BP/functions/golem/more.mcfunction"] = serde_json::json!({"sources": [
        {"path": "data/system_template/golem/sub/more.mcfunction", "status": "Created."}]});
    std::fs::write(&log, l.to_string()).unwrap();

    let p = migrate::detect(&proj).unwrap();
    let o = MigrateOptions {
        out: t.path().join("out"),
        evaluated: Some(ev.clone()),
        ..Default::default()
    };
    let eval = system_template::ingest(&p, &ev).unwrap();
    let plan = migrate::plan(&p, &eval, &o).unwrap();
    let c = copies(&plan);
    // `gated` sorts first: it owns `waves = 3`; `sub/more` (5) is frozen, `waves` (3) agrees
    assert!(c.iter().any(|(to, _)| to == "src/golem/waves.mcfunction"));
    assert!(
        c.iter()
            .any(|(to, _)| to == "src/golem/bp/functions/golem/more.mcfunction")
    );
    assert!(!c.iter().any(|(to, _)| to == "src/golem/more.mcfunction"));
    let scope = plan.changes.iter().find_map(|c| match c {
        Change::Write {
            to,
            body: Body::Json(v),
        } if to.as_str() == "src/golem/scope.json" => Some(v.clone()),
        _ => None,
    });
    assert_eq!(
        scope,
        Some(serde_json::json!({"label": "Golem", "waves": 3}))
    );
    let why = plan
        .report
        .to_port
        .iter()
        .find(|t| t.path.as_str().ends_with("golem/sub/more.mcfunction"))
        .map(|t| t.why.clone())
        .unwrap();
    assert!(why.contains("scope name `waves`"), "{why}");

    migrate::apply(&p, &plan, &o).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let bo = BuildOptions {
        out: Some(t.path().join("built")),
        trusted: true,
        ..Default::default()
    };
    let r = session(bo, &o.out, &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let rep = write::oracle_compare(&ev, &t.path().join("built").join(OUTPUT), &plan);
    assert_eq!(rep.diffs, [], "{rep:?}");
}

/// A `.lang` and an `.mcfunction` template of one module disagree on a scope name (a nested
/// `_scope.json` overrides the system's): whichever comes later is frozen from the legacy
/// output with a note naming the name; the migrated project still builds equal.
#[test]
fn lang_and_mcfunction_templates_with_conflicting_scope_fall_back() {
    let t = tempfile::tempdir().unwrap();
    let (proj, ev) = (t.path().join("proj"), t.path().join("ev"));
    copy_tree(&fixtures().join("system_template"), &proj);
    copy_tree(&fixtures().join("system_template.expected"), &ev);
    let sub = proj.join("data/system_template/golem/sub");
    std::fs::write(
        sub.join("_scope.json"),
        "{\"label\": \"Other\", \"waves\": 5}",
    )
    .unwrap();
    std::fs::write(
        sub.join("more.mcfunction"),
        "say `eval:label`
",
    )
    .unwrap();
    std::fs::write(
        sub.join("zz.lang"),
        "k=`eval:waves`
",
    )
    .unwrap();
    std::fs::write(ev.join("BP/functions/golem/more.mcfunction"), "say Other").unwrap();
    std::fs::write(ev.join("RP/texts/zz.lang"), "k=5").unwrap();
    let log = ev.join("system_template_log.json");
    let mut l: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    for (out, src) in [
        ("BP/functions/golem/more.mcfunction", "sub/more.mcfunction"),
        ("RP/texts/zz.lang", "sub/zz.lang"),
    ] {
        l[out] = serde_json::json!({"sources": [
            {"path": format!("data/system_template/golem/{src}"), "status": "Created."}]});
    }
    std::fs::write(&log, l.to_string()).unwrap();

    let p = migrate::detect(&proj).unwrap();
    let o = MigrateOptions {
        out: t.path().join("out"),
        evaluated: Some(ev.clone()),
        ..Default::default()
    };
    let eval = system_template::ingest(&p, &ev).unwrap();
    let plan = migrate::plan(&p, &eval, &o).unwrap();
    let c = copies(&plan);
    // `fr_FR.lang` owns `label = Golem` and `waves = 3`; the nested scope disagrees on both
    assert!(c.iter().any(|(to, _)| to == "src/golem/fr_FR.lang"));
    for frozen in [
        "src/golem/rp/texts/zz.lang",
        "src/golem/bp/functions/golem/more.mcfunction",
    ] {
        let (to, _) = c
            .iter()
            .find(|(to, _)| to == frozen)
            .unwrap_or_else(|| panic!("{frozen}"));
        assert!(to.contains("/bp/") || to.contains("/rp/"));
    }
    assert!(
        !c.iter()
            .any(|(to, _)| to == "src/golem/more.mcfunction" || to == "src/golem/zz.lang")
    );
    for (src, name) in [("sub/zz.lang", "waves"), ("sub/more.mcfunction", "label")] {
        let why = plan
            .report
            .to_port
            .iter()
            .find(|t| t.path.as_str().ends_with(&format!("golem/{src}")))
            .map(|t| t.why.clone())
            .unwrap();
        assert!(why.contains(&format!("scope name `{name}`")), "{why}");
    }

    migrate::apply(&p, &plan, &o).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let bo = BuildOptions {
        out: Some(t.path().join("built")),
        trusted: true,
        ..Default::default()
    };
    let r = session(bo, &o.out, &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let rep = write::oracle_compare(&ev, &t.path().join("built").join(OUTPUT), &plan);
    assert_eq!(rep.diffs, [], "{rep:?}");
}

/// Lang templates Beacon's build would not accept as the legacy tool wrote them stay frozen
/// even when the oracle's key-map comparison agrees: a key emitted twice, a single-`#` line;
/// and a template whose `.lang` merges several sources (the common `en_US.lang`).
#[test]
fn lang_templates_that_beacon_would_not_build_or_cannot_attribute_freeze() {
    let t = tempfile::tempdir().unwrap();
    let (proj, ev) = (t.path().join("proj"), t.path().join("ev"));
    copy_tree(&fixtures().join("system_template"), &proj);
    copy_tree(&fixtures().join("system_template.expected"), &ev);
    let g = proj.join("data/system_template/golem");
    std::fs::write(
        g.join("dup.lang"),
        "for <i 0..2>:
    k=v
",
    )
    .unwrap();
    std::fs::write(
        g.join("hash.lang"),
        "k=`eval:1`
#x=y
",
    )
    .unwrap();
    // the appended-to `en_US.lang` of the fixture (golem created it, ruby appended)
    std::fs::write(
        g.join("en_US.lang"),
        "if <waves>:
    a=b
    c=d
",
    )
    .unwrap();
    // the oracle's key map equals the raw source's (literal keys under `if`): the fragment
    // split alone would accept it, so the placement must refuse the template
    std::fs::write(
        ev.join("RP/texts/en_US.lang"),
        "a=b
c=d

item.stf:ruby.name=Ruby
item.stf:sapphire.name=Sapphire",
    )
    .unwrap();
    std::fs::write(
        ev.join("RP/texts/dup.lang"),
        "k=v
k=v",
    )
    .unwrap();
    std::fs::write(
        ev.join("RP/texts/hash.lang"),
        "k=1
#x=y",
    )
    .unwrap();
    let log = ev.join("system_template_log.json");
    let mut l: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    for n in ["dup", "hash"] {
        l[format!("RP/texts/{n}.lang")] = serde_json::json!({"sources": [
            {"path": format!("data/system_template/golem/{n}.lang"), "status": "Created."}]});
    }
    std::fs::write(&log, l.to_string()).unwrap();
    let p = migrate::detect(&proj).unwrap();
    let o = MigrateOptions {
        out: t.path().join("out"),
        evaluated: Some(ev.clone()),
        ..Default::default()
    };
    let eval = system_template::ingest(&p, &ev).unwrap();
    let plan = migrate::plan(&p, &eval, &o).unwrap();
    let why = |name: &str| {
        plan.report
            .to_port
            .iter()
            .find(|t| t.path.as_str().ends_with(name))
            .map(|t| t.why.clone())
            .unwrap_or_else(|| panic!("no to_port for {name}"))
    };
    assert!(
        why("golem/dup.lang").contains("defined twice"),
        "{}",
        why("golem/dup.lang")
    );
    assert!(
        why("golem/hash.lang").contains("single `#`"),
        "{}",
        why("golem/hash.lang")
    );
    assert!(
        why("golem/en_US.lang").contains("merges 2 source(s) or a base file"),
        "{}",
        why("golem/en_US.lang")
    );
    let c = copies(&plan);
    assert!(
        !c.iter()
            .any(|(to, _)| to == "src/golem/dup.lang" || to == "src/golem/hash.lang")
    );
    // placement: the merged lang is pre-merged, the template is not copied live
    assert!(!c.iter().any(|(_, from)| from.ends_with("golem/en_US.lang")));
    assert!(
        c.iter()
            .any(|(to, f)| to == "src/_merged/rp/texts/en_US.lang" && f == "<write>")
    );
    assert!(
        plan.report
            .flattened
            .iter()
            .any(|e| e.path == "RP/texts/en_US.lang" && e.why.contains("subfunctions syntax")),
        "{:?}",
        plan.report.flattened
    );
}

/// `UNPACK:SUBFUNCTION` whose output folder repeats the file stem in the path
/// (`functions/unp/unp/`): the template's own path is the last `/unp/`, so it still translates.
#[test]
fn unpacked_template_in_a_folder_named_like_its_stem_translates() {
    let t = tempfile::tempdir().unwrap();
    let (proj, ev) = (t.path().join("proj"), t.path().join("ev"));
    copy_tree(&fixtures().join("system_template"), &proj);
    copy_tree(&fixtures().join("system_template.expected"), &ev);
    let src = "data/system_template/golem/unp.mcfunction";
    std::fs::write(
        proj.join(src),
        "UNPACK:SUBFUNCTION\ndefinefunction <setup>:\n    say hi\n",
    )
    .unwrap();
    std::fs::create_dir_all(ev.join("BP/functions/unp/unp")).unwrap();
    std::fs::write(ev.join("BP/functions/unp/unp/setup.mcfunction"), "say hi").unwrap();
    let log = ev.join("system_template_log.json");
    let mut l: serde_json::Value = serde_json::from_slice(&std::fs::read(&log).unwrap()).unwrap();
    l["BP/functions/unp/unp.mcfunction"] =
        serde_json::json!({"sources": [{"path": src, "status": "Created."}]});
    std::fs::write(&log, l.to_string()).unwrap();

    let p = migrate::detect(&proj).unwrap();
    let o = MigrateOptions {
        out: t.path().join("out"),
        evaluated: Some(ev.clone()),
        ..Default::default()
    };
    let eval = system_template::ingest(&p, &ev).unwrap();
    let plan = migrate::plan(&p, &eval, &o).unwrap();
    assert!(
        copies(&plan)
            .iter()
            .any(|(to, from)| to == "src/golem/bp/functions/unp/unp.mcfunction" && from == src),
        "{:?}",
        copies(&plan)
    );
}
