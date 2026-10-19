//! Phase 3 B1, wave C decisions: npm dependencies moved out of the data dir, subfunctions
//! syntax frozen from `--oracle`, `packMeta.packDependencies` from the legacy manifests'
//! dependency directions.

mod b1_common;
use b1_common::write;
use beacon_engine::migrate::{
    self, Applied, Change, Contributor, EvalOutput, LegacyEval, MigrateOptions, MigrationPlan,
    Policy, Template, ToPortKind,
};
use beacon_engine::model::Body;
use beacon_engine::path::RelPath;
use serde_json::{Value, json};
use std::path::Path;

const D: &str = "packs/data/modular_mc";

fn config(filters: &str) -> String {
    format!(
        r#"{{"name":"dec","packs":{{"behaviorPack":"./packs/BP","resourcePack":"./packs/RP"}},
          "regolith":{{"dataPath":"./packs/data","filterDefinitions":{{"modular_mc":{{"url":"x","version":"1"}}}},
          "profiles":{{"default":{{"export":{{"target":"development"}},"filters":[{filters}]}}}}}}}}"#
    )
}

fn rel(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn plan(root: &Path, eval: &LegacyEval, oracle: Option<&Path>) -> MigrationPlan {
    let p = migrate::detect(root).unwrap();
    let o = MigrateOptions {
        out: root.join("../out"),
        oracle: oracle.map(Path::to_path_buf),
        ..Default::default()
    };
    migrate::plan(&p, eval, &o).unwrap()
}

fn body(plan: &MigrationPlan, to: &str) -> Option<Body> {
    plan.changes.iter().find_map(|c| match c {
        Change::Write { to: t, body } if t.as_str() == to => Some(body.clone()),
        Change::Copy { from, to: t } if t.as_str() == to => Some(Body::Passthrough(from.clone())),
        _ => None,
    })
}

fn config_of(plan: &MigrationPlan) -> Value {
    plan.changes
        .iter()
        .find_map(|c| match c {
            Change::Config(v) => Some(v.clone()),
            _ => None,
        })
        .unwrap()
}

#[test]
fn npm_dependencies_move_to_the_root_with_vendored_file_packages() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("legacy");
    write(&r, "config.json", &config(r#"{"filter":"modular_mc"}"#));
    write(
        &r,
        "package.json",
        r#"{"name":"root","dependencies":{"a":"0.9.0","r":"1.0.0"}}"#,
    );
    write(
        &r,
        "packs/data/package.json",
        r#"{"dependencies":{"a":"1.0.0","local":"file:esb/local_mod","y":"file:.yalc/@s/y","t":"file:../../tgz/t-1.0.0.tgz","abs":"file:/abs/x","drive":"file:C:\\x\\y","unc":"file://server/x"}}"#,
    );
    write(&r, "packs/data/package-lock.json", "{}");
    write(&r, "packs/data/esb/local_mod/index.ts", "export {};\n");
    write(&r, "packs/data/esb/local_mod/package.json", "{}");
    write(&r, "packs/data/.yalc/@s/y/package.json", "{}");
    write(&r, "tgz/t-1.0.0.tgz", "TGZ");
    write(&r, "packs/data/node_modules/a/index.js", "x");
    write(&r, &format!("{D}/m/_map.ts"), "export const MAP = [];");
    let plan = plan(&r, &LegacyEval::default(), None);
    let Some(Body::Json(pkg)) = body(&plan, "package.json") else {
        panic!("{:?}", body(&plan, "package.json"))
    };
    assert_eq!(pkg["name"], "root");
    assert_eq!(
        pkg["dependencies"],
        json!({
            "a": "1.0.0", "r": "1.0.0",
            "local": "file:vendor/packs/data/esb/local_mod",
            "y": "file:vendor/packs/data/.yalc/@s/y",
            "t": "file:vendor/tgz/t-1.0.0.tgz",
            // absolute specs leave the project: kept as they are, never "copied"
            "abs": "file:/abs/x",
            "drive": "file:C:\\x\\y",
            "unc": "file://server/x"
        })
    );
    for f in [
        "vendor/packs/data/esb/local_mod/index.ts",
        "vendor/packs/data/esb/local_mod/package.json",
        "vendor/packs/data/.yalc/@s/y/package.json",
        "vendor/tgz/t-1.0.0.tgz",
    ] {
        assert!(body(&plan, f).is_some(), "{f}");
    }
    assert!(
        !plan
            .changes
            .iter()
            .any(|c| matches!(c, Change::Copy { to, .. } if to.as_str().contains("node_modules")))
    );
    assert!(matches!(
        body(&plan, "package-lock.json"),
        Some(Body::Passthrough(_))
    ));
    for n in ["abs", "drive", "unc"] {
        assert!(
            plan.changes.iter().any(|c| matches!(c, Change::Note(d)
                if d.message.contains(&format!("{n} `file:")) && d.message.contains("leaves the project"))),
            "{n}"
        );
    }
    assert!(!plan.changes.iter().any(
        |c| matches!(c, Change::Copy { to, .. } if to.as_str().contains("vendor/abs")
                || to.as_str().contains("vendor/packs/data/abs"))
    ));
    let e = plan
        .report
        .codemods
        .iter()
        .find(|e| e.path == "package.json")
        .unwrap();
    assert!(
        e.why.contains("npm install")
            && e.why.contains("root \"0.9.0\"")
            && !e.why.contains("abs:")
            && !e.why.contains("drive:")
            && !e.why.contains("unc:"),
        "{}",
        e.why
    );
}

const SUB: &str = "for <i 1..3 1>:\n    say `eval:i`\nfunction <child>:\n    say c\n";
const EXPANDED: &str = "say 1\nsay 2\nfunction m/f/child\nfunction gen/helper\nfunction m/other\n";

fn subfunction_project(t: &Path) -> (std::path::PathBuf, LegacyEval) {
    let r = t.join("legacy");
    write(
        &r,
        "config.json",
        &config(r#"{"filter":"modular_mc"},{"filter":"subfunctions"}"#),
    );
    write(&r, &format!("{D}/m/_map.ts"), "export const MAP = [];");
    write(&r, &format!("{D}/m/f.mcfunction"), SUB);
    write(&r, &format!("{D}/m/other.mcfunction"), "say other\n");
    let mut e = LegacyEval::default();
    for name in ["f", "other"] {
        let src = rel(&format!("{D}/m/{name}.mcfunction"));
        let path = format!("BP/functions/m/{name}.mcfunction");
        e.outputs.insert(
            path.clone(),
            EvalOutput {
                path,
                body: Body::Passthrough(src.clone()),
                contributors: vec![Contributor {
                    map: rel(&format!("{D}/m/_map.ts")),
                    entry: 0,
                    source: src,
                    policy: Policy::Stop,
                    template: Template::None,
                    applied: Applied::Created,
                }],
            },
        );
    }
    (r, e)
}

#[test]
fn subfunctions_syntax_is_frozen_from_the_oracle() {
    let t = tempfile::tempdir().unwrap();
    let (r, e) = subfunction_project(t.path());
    let oracle = t.path().join("oracle");
    write(&oracle, "BP/functions/m/f.mcfunction", EXPANDED);
    write(&oracle, "BP/functions/m/f/child.mcfunction", "say c\n");
    write(&oracle, "BP/functions/gen/helper.mcfunction", "say h\n");
    // accounted for by a migrated source: never taken from the oracle
    write(&oracle, "BP/functions/m/other.mcfunction", "say ORACLE\n");
    let plan = plan(&r, &e, Some(&oracle));
    assert_eq!(
        body(&plan, "src/m/f.mcfunction"),
        Some(Body::Text(EXPANDED.into()))
    );
    assert_eq!(
        body(&plan, "src/m/bp/functions/m/f/child.mcfunction"),
        Some(Body::Text("say c\n".into()))
    );
    assert_eq!(
        body(&plan, "src/m/bp/functions/gen/helper.mcfunction"),
        Some(Body::Text("say h\n".into()))
    );
    assert!(matches!(
        body(&plan, "src/m/other.mcfunction"),
        Some(Body::Passthrough(_))
    ));
    let r = &plan.report;
    let frozen: Vec<&str> = r
        .flattened
        .iter()
        .filter(|e| {
            e.why
                .contains("frozen subfunctions expansion (from the oracle)")
        })
        .map(|e| e.path.as_str())
        .collect();
    assert_eq!(frozen.len(), 3, "{frozen:?}");
    let tp: Vec<_> = r
        .to_port
        .iter()
        .filter(|t| t.kind == ToPortKind::SubfunctionSource)
        .collect();
    assert_eq!(tp.len(), 1, "{tp:?}");
    assert!(tp[0].why.contains("frozen"));

    // without --oracle: reported, left as is
    let plan = self::plan(&t.path().join("legacy"), &e, None);
    assert!(matches!(
        body(&plan, "src/m/f.mcfunction"),
        Some(Body::Passthrough(_))
    ));
    assert!(
        plan.report
            .to_port
            .iter()
            .any(|t| t.kind == ToPortKind::SubfunctionSource
                && t.why.contains("still uses subfunctions syntax"))
    );
}

#[test]
fn legacy_dependency_directions_set_pack_dependencies() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("legacy");
    write(&r, "config.json", &config(r#"{"filter":"modular_mc"}"#));
    write(&r, &format!("{D}/m/_map.ts"), "export const MAP = [];");
    let m = |uuid: &str, kind: &str, dep: Option<&str>| {
        let mut v = json!({"format_version": 2,
            "header": {"uuid": uuid, "version": [1, 0, 0], "min_engine_version": [1, 21, 0]},
            "modules": [{"type": kind, "uuid": format!("{uuid}-m"), "version": [1, 0, 0]}]});
        if let Some(d) = dep {
            v["dependencies"] = json!([{"uuid": d, "version": [1, 0, 0]}]);
        }
        v.to_string()
    };
    write(
        &r,
        "packs/RP/manifest.json",
        &m("rp-u", "resources", Some("bp-u")),
    );
    write(&r, "packs/BP/manifest.json", &m("bp-u", "data", None));
    let deps = |r: &Path| {
        let p = plan(r, &LegacyEval::default(), None);
        assert!(p.report.dropped.is_empty(), "{:?}", p.report.dropped);
        config_of(&p)["beacon"]["packMeta"]["packDependencies"].clone()
    };
    // RP -> BP only
    assert_eq!(deps(&r), "rp-bp");
    // both directions: both kept
    write(
        &r,
        "packs/BP/manifest.json",
        &m("bp-u", "data", Some("rp-u")),
    );
    assert_eq!(deps(&r), "mutual");
    // BP -> RP only (Beacon's default) or none: nothing to set
    write(&r, "packs/RP/manifest.json", &m("rp-u", "resources", None));
    assert_eq!(deps(&r), Value::Null);
    write(&r, "packs/BP/manifest.json", &m("bp-u", "data", None));
    assert_eq!(deps(&r), Value::Null);
}

/// `oracle_compare` on script bundles: module order after mapping migrated paths back.
#[test]
fn bundle_headers_map_back_to_the_legacy_sources() {
    let t = tempfile::tempdir().unwrap();
    let legacy = "// .regolith/tmp/.temp_esbuild_entry_123.ts\n\
        // data/node_modules/local-mod/util.ts\nx\n\
        // data/system_template_esbuild/subscripts/a.ts\n// change.\ny\n";
    let migrated = "// src/_scripts/scripts/main.ts\n\
        // vendor/filters_data/esb/local_mod/util.ts\nx\n\
        // src/_scripts/system_template_esbuild/subscripts/a.ts\n// per-window accumulator\ny\n";
    write(&t.path().join("o"), "BP/scripts/main.js", legacy);
    write(&t.path().join("b"), "BP/scripts/main.js", migrated);
    let plan = MigrationPlan::default();
    let r = migrate::write::oracle_compare(&t.path().join("o"), &t.path().join("b"), &plan);
    assert_eq!(r.diffs, [], "{r:?}");
    // another order is a difference
    let swapped = "// src/_scripts/system_template_esbuild/subscripts/a.ts\n\
        // vendor/filters_data/esb/local_mod/util.ts\n// src/_scripts/scripts/main.ts\n";
    write(&t.path().join("b"), "BP/scripts/main.js", swapped);
    let r = migrate::write::oracle_compare(&t.path().join("o"), &t.path().join("b"), &plan);
    assert_eq!(r.diffs.len(), 1, "{r:?}");
}

/// `--oracle-profile`: the migrated target that keeps the profile's own edition and
/// `targetVersion` (same-named targets of several profiles are merged in the config).
#[test]
fn oracle_profile_picks_the_matching_target() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("legacy");
    let ws = r#"{"filter":"shapescape_world_settings","settings":{"level_dat_path":"worlds/W/level.dat"}}"#;
    let cfg = format!(
        r#"{{"name":"dec","packs":{{"behaviorPack":"./packs/BP","resourcePack":"./packs/RP"}},
          "regolith":{{"dataPath":"./packs/data","filterDefinitions":{{"modular_mc":{{"url":"x","version":"1"}}}},
          "profiles":{{
            "default":{{"export":{{"target":"development"}},"filters":[{{"filter":"modular_mc"}}]}},
            "packaging":{{"export":{{"target":"local"}},"filters":[{{"profile":"default"}},{ws}]}},
            "bedrock_packaging":{{"export":{{"target":"local"}},"filters":[{{"profile":"default"}},{ws},
              {{"filter":"convert_to_bedrock","settings":{{"release_config_path":"rc.json"}}}}]}}}}}}}}"#
    );
    write(&r, "config.json", &cfg);
    write(&r, "rc.json", r#"{"target_format_version":[1,20,50]}"#);
    write(&r, &format!("{D}/m/_map.ts"), "export const MAP = [];");
    let p = migrate::detect(&r).unwrap();
    let plan = plan(&r, &LegacyEval::default(), None);
    let c = config_of(&plan);
    // the merged `world` is bedrock_packaging's (first profile by name)
    assert_eq!(c["beacon"]["targets"]["world"]["targetVersion"], "1.20.50");
    let pick = |prof: &str| migrate::plan::oracle_target(&p, prof, &c);
    assert_eq!(pick("bedrock_packaging").unwrap(), "world");
    assert_eq!(pick("default").unwrap(), "dev");
    // packaging's world has no targetVersion of its own: the merged one is not it
    assert!(pick("packaging").unwrap_err().contains("targetVersion"));
    assert!(pick("nope").is_err());
    // a world target without its world folder cannot build: a world-free target with the
    // same edition and targetVersion (dev) stands in; none matching is an error
    let mut c2 = c.clone();
    let w = c2["beacon"]["targets"]["world"].as_object_mut().unwrap();
    w.remove("world");
    w.remove("targetVersion");
    assert_eq!(
        migrate::plan::oracle_target(&p, "packaging", &c2).unwrap(),
        "dev"
    );
    let mut c3 = c.clone();
    c3["beacon"]["targets"]["world"]
        .as_object_mut()
        .unwrap()
        .remove("world");
    assert!(migrate::plan::oracle_target(&p, "bedrock_packaging", &c3).is_err());
}

/// The packs of a built archive, nested `.mcpack`s included; the world is left out.
#[test]
fn archive_packs_are_extracted_by_manifest() {
    use std::io::Write;
    let zip = |entries: &[(&str, &[u8])]| -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, b) in entries {
            w.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    let bp = br#"{"modules":[{"type":"data"}]}"#;
    let rp = br#"{"modules":[{"type":"resources"}]}"#;
    let inner = zip(&[("manifest.json", rp), ("texts/en_US.lang", b"a=b")]);
    let world = zip(&[
        ("level.dat", b"x"),
        ("behavior_packs/BP/manifest.json", bp),
        ("behavior_packs/BP/items/a.json", b"{}"),
        ("rp.mcpack", &inner),
        (
            "manifest.json",
            br#"{"modules":[{"type":"world_template"}]}"#,
        ),
    ]);
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("w.mcworld"), world).unwrap();
    let into = t.path().join("cmp");
    let mut taken = std::collections::BTreeSet::new();
    let d = migrate::write::extract_packs(&t.path().join("w.mcworld"), &into, &mut taken).unwrap();
    assert!(d.is_empty(), "{d:?}");
    assert!(into.join("BP/manifest.json").is_file());
    assert!(into.join("BP/items/a.json").is_file());
    assert_eq!(
        std::fs::read_to_string(into.join("RP/texts/en_US.lang")).unwrap(),
        "a=b"
    );
    assert!(!into.join("BP/level.dat").exists() && !into.join("level.dat").exists());

    // an .mcaddon of nested .mcpacks; a second BP is reported, never written over the first;
    // a pack nested two levels deep is reported and skipped
    let bp2 = zip(&[("manifest.json", bp), ("items/second.json", b"{}")]);
    let deep = zip(&[("deeper.mcpack", &inner)]);
    let addon = zip(&[
        (
            "bp.mcpack",
            &zip(&[("manifest.json", bp), ("items/b.json", b"{}")]),
        ),
        ("rp.mcpack", &inner),
        ("bp2.mcpack", &bp2),
        ("deep.mcpack", &deep),
    ]);
    std::fs::write(t.path().join("a.mcaddon"), addon).unwrap();
    let into = t.path().join("cmp2");
    let mut taken = std::collections::BTreeSet::new();
    let d = migrate::write::extract_packs(&t.path().join("a.mcaddon"), &into, &mut taken).unwrap();
    assert!(into.join("RP/texts/en_US.lang").is_file());
    let bps = [
        into.join("BP/items/b.json").is_file(),
        into.join("BP/items/second.json").is_file(),
    ];
    assert_eq!(
        bps.iter().filter(|x| **x).count(),
        1,
        "one BP only: {bps:?}"
    );
    let msgs: Vec<&str> = d.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.iter().any(|m| m.contains("a second BP")), "{msgs:?}");
    assert!(
        msgs.iter().any(|m| m.contains("more than one level")),
        "{msgs:?}"
    );
    assert!(
        d.iter()
            .all(|d| d.code == beacon_engine::diag::Code::MigrateOracleDiff)
    );
}

#[test]
fn subfunctions_syntax_the_expander_reproduces_stays_a_source() {
    let t = tempfile::tempdir().unwrap();
    let (r, e) = subfunction_project(t.path());
    write(
        &r,
        &format!("{D}/m/f.mcfunction"),
        "for <i 0..2>:\n    say `eval:i`\n",
    );
    let oracle = t.path().join("oracle");
    write(&oracle, "BP/functions/m/f.mcfunction", "say 0\nsay 1");
    write(&oracle, "BP/functions/m/other.mcfunction", "say other\n");
    let plan = plan(&r, &e, Some(&oracle));
    assert!(matches!(
        body(&plan, "src/m/f.mcfunction"),
        Some(Body::Passthrough(_))
    ));
    assert!(body(&plan, "src/m/scope.json").is_none());
    assert!(plan.changes.iter().any(|c| matches!(c,
        Change::Note(n) if n.code == beacon_engine::diag::Code::MigrateTranslated)));
    assert!(
        !plan
            .report
            .to_port
            .iter()
            .any(|t| t.kind == ToPortKind::SubfunctionSource)
    );
    // the oracle has a function the template does not make: frozen, with the reason
    write(&oracle, "BP/functions/m/f/extra.mcfunction", "say x\n");
    let plan = self::plan(&r, &e, Some(&oracle));
    assert_eq!(
        body(&plan, "src/m/f.mcfunction"),
        Some(Body::Text("say 0\nsay 1".into()))
    );
    let tp = plan
        .report
        .to_port
        .iter()
        .find(|t| t.kind == ToPortKind::SubfunctionSource)
        .unwrap();
    assert!(tp.why.contains("does not produce"), "{}", tp.why);
}
