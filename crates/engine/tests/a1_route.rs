use beacon_engine::config::ProjectConfig;
use beacon_engine::diag::{Code, Severity};
use beacon_engine::model::{Body, Merge, Pack};
use beacon_engine::path::RelPath;
use beacon_engine::route::{load, route};
use beacon_engine::vfs::Index;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// Fresh corpus per run: a shared `target/corpus-medium` can be left half-written or
/// marker-less by other tools or CI caches.
fn corpus() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("corpus");
    {
        let st = std::process::Command::new(env!("CARGO"))
            .args([
                "run",
                "--release",
                "-p",
                "beacon-corpus",
                "--",
                "--size",
                "medium",
                "--force",
                "--out",
            ])
            .arg(&root)
            .status()
            .unwrap();
        assert!(st.success());
    }
    (dir, root)
}

#[test]
fn routes_medium_corpus() {
    let (_tmp, root) = corpus();
    let cfg = load(&root).unwrap();
    let packs: Vec<RelPath> = [&cfg.packs.behavior_pack, &cfg.packs.resource_pack]
        .into_iter()
        .flatten()
        .map(|p| RelPath::parse(p.trim_start_matches("./")).unwrap())
        .collect();
    let (idx, d) = Index::build(&root, &packs, None);
    assert!(d.is_empty(), "{d:?}");
    let r = route(&cfg, &idx);
    let errs: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "{errs:?}");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);

    // every file under src is accounted for exactly once
    let mut accounted: BTreeSet<&RelPath> = r.items.iter().map(|i| &i.origin).collect();
    assert_eq!(accounted.len(), r.items.len());
    accounted.extend(r.generators.iter().map(|g| &g.path));
    accounted.extend(r.script_entries.values().flatten());
    let mut other = 0;
    for f in idx
        .files()
        .into_iter()
        .filter(|f| f.as_str().starts_with("src/"))
    {
        let n = f.file_name();
        if n == "module.json" || n.ends_with(".data.json") || n.ends_with(".ts") {
            other += 1;
        } else {
            assert!(accounted.contains(f), "unaccounted {f}");
        }
    }
    assert!(other > 0 && !r.generators.is_empty());
    assert!(
        r.script_entries
            .values()
            .all(|v| !v.is_empty() && v.windows(2).all(|w| w[0] < w[1]))
    );

    // both outputs used; the lib output has its own namespace
    let outs: BTreeSet<_> = r.items.iter().map(|i| i.dest.output.as_str()).collect();
    assert_eq!(outs, BTreeSet::from(["lib", "main"]));
    assert!(
        r.generators
            .iter()
            .any(|g| g.output == "lib" && g.namespace == "lib")
    );
    assert!(
        r.generators
            .iter()
            .any(|g| g.output == "main" && g.namespace == "demo")
    );
    assert_eq!(r.script_entries.len(), 2);

    // functions use the module layout; items sorted; all passthrough
    let f: Vec<_> = r
        .items
        .iter()
        .filter(|i| i.dest.path.as_str().starts_with("functions/"))
        .collect();
    assert!(!f.is_empty());
    for i in &f {
        let m = i.module.file_name();
        let want = format!("functions/{m}/");
        assert!(i.dest.path.as_str().starts_with(&want), "{:?}", i.dest);
        assert_eq!(i.dest.pack, Pack::Behavior);
    }
    assert!(
        r.items
            .iter()
            .all(|i| matches!(i.body, Body::Passthrough(_)))
    );
    assert!(
        r.items
            .windows(2)
            .all(|w| (&w[0].dest, &w[0].origin) <= (&w[1].dest, &w[1].origin))
    );
}

const CFG: &str =
    r#"{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"demo"}"#;

fn project(files: &[(&str, &str)]) -> (tempfile::TempDir, ProjectConfig, Index) {
    let d = tempfile::tempdir().unwrap();
    if !files.iter().any(|f| f.0 == "config.json") {
        fs::write(d.path().join("config.json"), CFG).unwrap();
    }
    for (p, c) in files {
        let f = d.path().join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, c).unwrap();
    }
    let cfg = load(d.path()).unwrap();
    let (idx, _) = Index::build(d.path(), &[], None);
    (d, cfg, idx)
}

#[test]
fn implicit_main_module_json_paths_and_conflicts() {
    let (_d, cfg, idx) = project(&[
        (
            "src/ui/module.json",
            r#"{"onConflict":"skip","paths":{"ui/*.json":"RP/ui","fonts/**":"RP/font"}}"#,
        ),
        ("src/ui/ui/a.json", "{}"),
        ("src/ui/ui/deep/b.json", "{}"),
        ("src/ui/fonts/x/y.ttf", "f"),
        ("src/ui/sword.item.json", "{}"),
        ("src/ui/notes.txt", "n"),
        ("src/ui/types.d.ts", ""),
        ("src/ui/scripts/main.ts", ""),
        ("src/ui/scripts/sub/x.ts", ""),
        ("src/ui/scripts/x.d.ts", ""),
        ("src/stray.txt", "s"),
    ]);
    let r = route(&cfg, &idx);
    let items: Vec<_> = r
        .items
        .iter()
        .map(|i| {
            (
                i.dest.output.as_str(),
                i.dest.pack,
                i.dest.path.as_str(),
                i.merge,
            )
        })
        .collect();
    assert_eq!(
        items,
        [
            ("main", Pack::Behavior, "items/sword.json", Merge::Skip),
            ("main", Pack::Resource, "font/y.ttf", Merge::Skip),
            ("main", Pack::Resource, "ui/a.json", Merge::Skip),
        ]
    );
    let unknown: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::UnknownSuffix)
        .map(|d| d.span.as_ref().unwrap().path.as_str())
        .collect();
    assert_eq!(
        unknown,
        ["src/stray.txt", "src/ui/notes.txt", "src/ui/ui/deep/b.json"]
    );
    assert_eq!(
        r.script_entries["main"],
        [RelPath::parse("src/ui/scripts/main.ts").unwrap()]
    );
}

#[test]
fn module_errors() {
    let cfg = r#"{"type":"t","name":"x","targetVersion":"1","namespace":"demo","beacon":{"outputs":[{"id":"a","include":["src/a/**"]}]}}"#;
    let (_d, cfg, idx) = project(&[
        ("config.json", cfg),
        ("src/a/x.item.json", "{}"),
        ("src/orphan/x.item.json", "{}"),
        ("src/bad/module.json", "{"),
        ("src/ghost/module.json", r#"{"output":"nope"}"#),
        (
            "src/weird/module.json",
            r#"{"output":"a","paths":{"*.x":"RP/a. "}}"#,
        ),
        ("src/weird/f.x", "x"),
    ]);
    let r = route(&cfg, &idx);
    let has = |c| r.diagnostics.iter().filter(|d| d.code == c).count();
    assert_eq!(has(Code::ModuleJsonInvalid), 2, "{:?}", r.diagnostics);
    assert_eq!(has(Code::ModuleWithoutOutput), 1);
    assert_eq!(has(Code::NonPortablePath), 1);
    assert_eq!(r.items.iter().filter(|i| i.dest.output == "a").count(), 1);
}

#[test]
fn load_validates() {
    let try_load = |body: &str| {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("config.json"), body).unwrap();
        load(d.path()).map(|_| ()).unwrap_err()
    };
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(load(empty.path()).unwrap_err()[0].code, Code::ConfigInvalid);
    assert_eq!(try_load("{")[0].code, Code::ConfigInvalid);
    assert_eq!(try_load(r#"{"name":1}"#)[0].code, Code::ConfigField);
    let base = |ns: &str, outs: &str| {
        format!(
            "{{\"type\":\"t\",\"name\":\"x\",\"targetVersion\":\"1\",\n\"namespace\":\"{ns}\",\"beacon\":{{\"outputs\":{outs}}}}}"
        )
    };
    for ns in ["minecraft", "Demo", "a-b", ""] {
        let e = try_load(&base(ns, "[]"));
        assert_eq!(e[0].code, Code::ConfigField, "{ns}");
        assert_eq!(e[0].span.as_ref().unwrap().line, Some(2));
    }
    let e = try_load(&base("demo", r#"[{"id":"a"},{"id":"a"}]"#));
    assert_eq!(e[0].code, Code::ConfigField);
    let e = try_load(&base("demo", r#"[{"id":"a","namespace":"minecraft"}]"#));
    assert_eq!(e[0].code, Code::ConfigField);
}

#[test]
fn on_conflict_merge_only_for_json_and_lang() {
    let (_d, cfg, idx) = project(&[
        ("src/m/module.json", r#"{"onConflict":"merge"}"#),
        ("src/m/items/a.item.json", "{}"),
        ("src/m/texts/en.lang", "a=b"),
        ("src/m/sounds/s.ogg", "o"),
    ]);
    let r = route(&cfg, &idx);
    let m = |suffix: &str| {
        r.items
            .iter()
            .find(|i| i.dest.path.as_str().ends_with(suffix))
            .unwrap()
            .merge
    };
    assert_eq!(m("a.json"), Merge::JsonDeep);
    assert_eq!(m("en.lang"), Merge::LangLines);
    assert_eq!(m("s.ogg"), Merge::Error);
}
