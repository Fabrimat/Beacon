//! Phase 3 B1: planner rules the fixtures do not reach, on a synthetic project with a
//! hand-made `LegacyEval`: base-pack fragments and copies, a merge Beacon cannot reproduce,
//! codemods in profile order (shapescape_default, text_replacer, name_ninja), the scripts
//! entry from `esbuild.settings.outfile`, manifest extras, to-port detection.

mod b1_common;
use b1_common::{errors, session, write};
use beacon_engine::build::BuildOptions;
use beacon_engine::migrate::{
    self, Applied, Change, Contributor, EvalOutput, LegacyEval, MigrateOptions, MigrationPlan,
    Policy, Template, ToPortKind,
};
use beacon_engine::model::Body;
use beacon_engine::path::RelPath;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const CONFIG: &str = r#"{
  "name": "syn", "author": "me",
  "packs": { "behaviorPack": "./packs/BP", "resourcePack": "./packs/RP" },
  "regolith": {
    "dataPath": "./packs/data",
    "filterDefinitions": { "modular_mc": { "url": "x", "version": "1" } },
    "profiles": { "default": { "export": { "target": "development" }, "filters": [
      { "filter": "modular_mc", "settings": { "esbuild": { "settings": { "outfile": "BP/scripts/game/index.js" } } } },
      { "filter": "shapescape_default" },
      { "filter": "text_replacer", "settings": { "rules": [ { "source": "OLD", "target": "NEW" } ] } },
      { "filter": "name_ninja", "settings": { "entities": { "auto_name": true }, "sort": true } },
      { "filter": "strange_filter" }
    ] } }
  }
}"#;

const BP_MANIFEST: &str = r#"{"format_version":2,
  "header":{"name":"pack.name","description":"pack.description","uuid":"b0000000-0000-4000-8000-000000000001","version":[2,3,4],"min_engine_version":[1,21,0]},
  "modules":[{"type":"data","uuid":"b0000000-0000-4000-8000-000000000002","version":[1,0,0]},
             {"type":"script","language":"javascript","entry":"scripts/game/index.js","uuid":"b0000000-0000-4000-8000-000000000003","version":[1,0,0]}],
  "dependencies":[{"uuid":"r0000000-0000-4000-8000-000000000001","version":[1,0,0]},{"module_name":"@minecraft/server","version":"1.9.0"}],
  "metadata":{"authors":["me"]}}"#;

const RP_MANIFEST: &str = r#"{"format_version":2,
  "header":{"name":"pack.name","description":"pack.description","uuid":"r0000000-0000-4000-8000-000000000001","version":[1,0,0],"min_engine_version":[1,21,0]},
  "modules":[{"type":"resources","uuid":"r0000000-0000-4000-8000-000000000002","version":[1,0,0]}]}"#;

const D: &str = "packs/data/modular_mc";

/// `text_replacer` (OLD -> NEW) ran on the legacy bundle, after imports were resolved: in the
/// sources, module specifiers must survive and the rest is rewritten.
const UTIL_TS: &str = "import \"./OLD\";\nexport * from \"./OLD\";\nimport { a } from './OLD';\nexport const x = \"OLD\";\n";

fn project() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().join("legacy");
    for (f, b) in [
        ("config.json", CONFIG),
        (
            "package.json",
            r#"{"scripts":{"preregolith":"node gen.js"}}"#,
        ),
        ("packs/BP/manifest.json", BP_MANIFEST),
        ("packs/RP/manifest.json", RP_MANIFEST),
        ("packs/BP/pack_icon.png", "PNG"),
        (
            "packs/BP/scripts/game/index.js",
            "// stale legacy bundle
",
        ),
        (
            "packs/RP/texts/en_US.lang",
            "pack.name=Legacy Name\nbase.key=1\n",
        ),
        ("packs/data/_plugins/p.py", "x = 1\n"),
        // autoNames regenerates `Frog` in en_US only: other languages keep the line
        ("packs/RP/texts/en_GB.lang", "entity.syn:frog.name=Frog\n"),
        (
            &format!("{D}/m/_map.ts"),
            "export const MAP = []; export const SCRIPTS = ['scripts/main.ts'];",
        ),
        (&format!("{D}/n/_map.ts"), "export const MAP = [];"),
        (
            &format!("{D}/m/frog.json"),
            r#"{"format_version":"1.21.0","minecraft:entity":{"description":{"identifier":"syn:frog","name":"Froggy"},"components":{"x":"OLD"}}}"#,
        ),
        (&format!("{D}/m/a.lang"), "m.key=1\n"),
        (&format!("{D}/m/x.json"), r#"{"a":1}"#),
        (&format!("{D}/n/x.json"), r#"{"a":2}"#),
        (&format!("{D}/m/scripts/main.ts"), "import './util.ts';\n"),
        (&format!("{D}/m/scripts/util.ts"), UTIL_TS),
    ] {
        write(&p, f, b);
    }
    t
}

fn rel(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn c(module: &str, src: &str, policy: Policy, applied: Applied) -> Contributor {
    Contributor {
        map: rel(&format!("{D}/{module}/_map.ts")),
        entry: 0,
        source: rel(&format!("{D}/{module}/{src}")),
        policy,
        template: Template::None,
        applied,
    }
}

fn eval() -> LegacyEval {
    let mut e = LegacyEval::default();
    let mut add = |path: &str, body: Body, cs: Vec<Contributor>| {
        e.outputs.insert(
            path.into(),
            EvalOutput {
                path: path.into(),
                body,
                contributors: cs,
            },
        );
    };
    add(
        "BP/entities/frog.json",
        Body::Passthrough(rel(&format!("{D}/m/frog.json"))),
        vec![c("m", "frog.json", Policy::Stop, Applied::Created)],
    );
    add(
        "RP/texts/en_US.lang",
        Body::Text("pack.name=Legacy Name\nbase.key=1\nm.key=1\n".into()),
        vec![c("m", "a.lang", Policy::AppendEnd, Applied::Appended)],
    );
    // ModularMC's merge lets the later scalar win; Beacon's is a conflict
    add(
        "BP/x.json",
        Body::Json(json!({"a": 2})),
        vec![
            c("m", "x.json", Policy::Stop, Applied::Created),
            c("n", "x.json", Policy::Merge, Applied::Merged),
        ],
    );
    add(
        "weird/out.txt",
        Body::Text("x".into()),
        vec![c("m", "a.lang", Policy::Stop, Applied::Created)],
    );
    // `appendStart` on a lang: written pre-merged under `_merged`'s pack root
    add(
        "RP/texts/de_DE.lang",
        Body::Text("n.key=2\nm.key=1\n".into()),
        vec![
            c("m", "a.lang", Policy::Stop, Applied::Created),
            c("n", "a.lang", Policy::AppendStart, Applied::Appended),
        ],
    );
    e.scripts = vec![rel(&format!("{D}/m/scripts/main.ts"))];
    e
}

fn plan(t: &Path) -> (migrate::LegacyProject, MigrationPlan, MigrateOptions) {
    let p = migrate::detect(&t.join("legacy")).unwrap();
    let o = MigrateOptions {
        out: t.join("out"),
        ..Default::default()
    };
    let plan = migrate::plan(&p, &eval(), &o).unwrap();
    (p, plan, o)
}

fn written(plan: &MigrationPlan, to: &str) -> Option<Body> {
    plan.changes.iter().find_map(|c| match c {
        Change::Write { to: t, body } if t.as_str() == to => Some(body.clone()),
        Change::Copy { from, to: t } if t.as_str() == to => Some(Body::Passthrough(from.clone())),
        _ => None,
    })
}

fn text(b: Option<Body>) -> String {
    match b {
        Some(Body::Text(t)) => t,
        other => panic!("{other:?}"),
    }
}

#[test]
fn synthetic_rules() {
    let t = project();
    let (_, plan, _) = plan(t.path());
    // codemods in profile order: families + despawn, text_replacer, name moved to the lang
    let frog: Value =
        serde_json::from_str(&text(written(&plan, "src/m/bp/entities/frog.json"))).unwrap();
    let e = &frog["minecraft:entity"];
    assert_eq!(e["components"]["x"], "NEW");
    assert_eq!(
        e["components"]["minecraft:type_family"]["family"],
        json!(["shapescape", "shapescape_"])
    );
    assert!(e["description"].get("name").is_none());
    assert_eq!(
        text(written(&plan, "src/m/texts/en_US.lang")),
        // name_ninja: spawn egg first, the explicit name for both
        "item.spawn_egg.entity.syn:frog.name=Froggy\nentity.syn:frog.name=Froggy\n"
    );
    // base lang is the first fragment; Beacon's own pack.name line is removed from it
    assert_eq!(
        text(written(&plan, "src/_base/rp/texts/en_US.lang")),
        "base.key=1\n"
    );
    assert!(matches!(
        written(&plan, "src/m/rp/texts/en_US.lang"),
        Some(Body::Passthrough(_))
    ));
    // untouched base files are copied, manifests are not
    assert!(written(&plan, "src/_base/bp/pack_icon.png").is_some());
    assert!(written(&plan, "src/_base/bp/manifest.json").is_none());
    assert!(matches!(
        written(&plan, "src/_base/rp/texts/en_GB.lang"),
        Some(Body::Passthrough(_))
    ));
    // a stale legacy bundle in the base pack would collide with Beacon's bundle
    assert!(written(&plan, "src/_base/bp/scripts/game/index.js").is_none());
    assert!(
        plan.report
            .dropped
            .iter()
            .any(|e| e.path == "BP/scripts/game/index.js")
    );
    // a merge Beacon would not reproduce: written pre-merged
    assert_eq!(
        written(&plan, "src/_merged/bp/x.json"),
        Some(Body::Json(json!({"a": 2})))
    );
    let r = &plan.report;
    assert!(r.flattened.iter().any(|e| e.path == "BP/x.json"));
    assert!(r.dropped.iter().any(|e| e.path == "weird/out.txt"));
    // scripts: sources mirrored, barrel in bundle order
    assert_eq!(
        text(written(&plan, "src/_scripts/modular_mc/m/scripts/util.ts")),
        UTIL_TS.replace("const x = \"OLD\"", "const x = \"NEW\"")
    );
    // a lang under a pack root merges with the pack's other langs (Beacon's base lang)
    let mj = match written(&plan, "src/_merged/module.json") {
        Some(Body::Json(v)) => v,
        other => panic!("{other:?}"),
    };
    assert_eq!(mj["conflicts"]["rp/texts/de_DE.lang"], "merge");
    // game scripts may import constants from a map (`./_map.js`)
    assert!(written(&plan, "src/_scripts/modular_mc/m/_map.ts").is_some());
    assert_eq!(
        text(written(&plan, "src/_scripts/scripts/main.ts"))
            .lines()
            .nth(1),
        Some("import \"../modular_mc/m/scripts/main.ts\";")
    );
    let cfg = plan
        .changes
        .iter()
        .find_map(|c| match c {
            Change::Config(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap();
    let b = &cfg["beacon"];
    assert_eq!(b["outputs"][0]["scriptEntry"], "scripts/game/index.js");
    assert_eq!(b["scriptModules"]["@minecraft/server"], "1.9.0");
    assert_eq!(b["packMeta"]["name"], "Legacy Name");
    assert_eq!(
        b["packMeta"]["manifest"]["BP"],
        json!({"metadata": {"authors": ["me"]}})
    );
    assert_eq!(b["autoNames"]["entities"], true);
    assert_eq!(cfg["targetVersion"], "1.21.0");
    assert_eq!(cfg["namespace"], "syn");
    let kinds: Vec<(ToPortKind, String)> = r
        .to_port
        .iter()
        .map(|t| (t.kind, t.path.to_string()))
        .collect();
    for want in [
        (ToPortKind::PrebuildScript, "package.json"),
        (ToPortKind::PythonPlugin, "packs/data/_plugins/p.py"),
        (ToPortKind::LegacyFilter, "config.json"),
    ] {
        assert!(
            kinds.iter().any(|(k, p)| *k == want.0 && p == want.1),
            "{want:?} in {kinds:?}"
        );
    }
    assert!(r.to_port.iter().any(|t| t.why.contains("strange_filter")));
    assert!(r.to_port.iter().any(|t| t.why.contains("sort")));
}

#[test]
fn synthetic_project_builds() {
    let t = project();
    let (p, plan, o) = plan(t.path());
    migrate::apply(&p, &plan, &o).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let bo = BuildOptions {
        out: Some(t.path().join("built")),
        trusted: true,
        ..Default::default()
    };
    let r = session(bo, &t.path().join("out"), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let built = t.path().join("built/main");
    let lang = std::fs::read_to_string(built.join("RP/texts/en_US.lang")).unwrap();
    for l in ["pack.name=Legacy Name", "base.key=1", "m.key=1"] {
        assert!(lang.contains(l), "{lang}");
    }
    let m: Value =
        serde_json::from_str(&std::fs::read_to_string(built.join("BP/manifest.json")).unwrap())
            .unwrap();
    assert_eq!(m["header"]["uuid"], "b0000000-0000-4000-8000-000000000001");
    assert_eq!(m["header"]["version"], json!([2, 3, 4]));
    assert_eq!(
        m["modules"][1]["uuid"],
        "b0000000-0000-4000-8000-000000000003"
    );
    assert_eq!(m["modules"][1]["entry"], "scripts/game/index.js");
    assert!(built.join("BP/scripts/game/index.js").is_file());
    assert!(built.join("BP/pack_icon.png").is_file());
}

#[test]
fn content_guide_fields_are_stripped_like_the_filter() {
    use beacon_engine::migrate::codemod::strip_content_guide;
    let e = br#"{"minecraft:entity":{"description":{"identifier":"x:a","is_spawnable":true,
        "description":"d","category":"character","locations":["1 2 3"],
        "spawn_egg_description":"s","spawn_egg_player_facing":true}}}"#;
    let (b, _) = strip_content_guide("BP/entities/a.json", e).unwrap();
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(
        v["minecraft:entity"]["description"],
        json!({"identifier": "x:a", "is_spawnable": true})
    );
    assert!(b.starts_with(b"{\n\t\""));
    // vanilla spawnable entities are skipped entirely
    let van = br#"{"minecraft:entity":{"description":{"identifier":"minecraft:pig","is_spawnable":true,"description":"d"}}}"#;
    assert!(strip_content_guide("BP/entities/p.json", van).is_none());
    let i = br#"{"minecraft:item":{"description":{"identifier":"x:i","description":["a","b"],"player_facing":false,"category":"items"}}}"#;
    let (b, _) = strip_content_guide("BP/items/i.json", i).unwrap();
    let v: Value = serde_json::from_slice(&b).unwrap();
    assert_eq!(
        v["minecraft:item"]["description"],
        json!({"identifier": "x:i", "category": "items"})
    );
    let blk = br#"{"minecraft:block":{"description":{"identifier":"x:b","player_facing":true}}}"#;
    assert!(strip_content_guide("BP/blocks/b.json", blk).is_some());
    assert!(strip_content_guide("RP/entity/a.json", e).is_none());
}
