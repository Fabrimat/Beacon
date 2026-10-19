//! API checks of `check` (apicheck.rs) against a synthetic API index, and the debug mirror's
//! bundle copy. `BEACON_HOME` points at a temp data dir holding the index of `1.26.50`; the
//! game version `1.0.1` has none.

mod b1_common;
use b1_common::*;
use beacon_engine::api_index::{
    API_INDEX_SCHEMA, ApiIndex, ApiItem, ApiKind, Component, ScriptModule, shared,
};
use beacon_engine::build::BuildOutcome;
use beacon_engine::diag::{Code, Severity};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicUsize;

const GAME: &str = "1.26.50";

fn item(deprecated: bool) -> ApiItem {
    ApiItem {
        kind: ApiKind::Class,
        members: BTreeMap::new(),
        deprecated,
        doc: None,
    }
}

fn home() {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        let module = |version: &str, beta: bool| ScriptModule {
            version: version.into(),
            beta,
            exports: [
                ("world".to_string(), item(false)),
                ("system".to_string(), item(false)),
                ("OldThing".to_string(), item(true)),
            ]
            .into(),
        };
        let comp = |_: &str| Component {
            description: None,
            experimental: None,
        };
        let set =
            |ids: &[&str]| -> BTreeSet<String> { ids.iter().map(|s| s.to_string()).collect() };
        let ix = ApiIndex {
            schema: API_INDEX_SCHEMA,
            game_version: GAME.into(),
            sources: vec![],
            script_modules: [(
                "@minecraft/server".to_string(),
                vec![module("2.6.0", false), module("2.7.0-beta", true)],
            )]
            .into(),
            vanilla: [
                ("entity".to_string(), set(&["minecraft:zombie"])),
                ("item".to_string(), set(&["minecraft:diamond"])),
                ("block".to_string(), set(&["minecraft:stone"])),
            ]
            .into(),
            components: [(
                "entity".to_string(),
                [("minecraft:health".to_string(), comp("h"))].into(),
            )]
            .into(),
        };
        let dir = d.path().join("api").join(GAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.json"), serde_json::to_vec(&ix).unwrap()).unwrap();
        // SAFETY: set once, before any test of this file reads the environment through the engine
        unsafe { std::env::set_var("BEACON_HOME", d.path()) };
        d
    });
}

fn config(version: &str, module: &str) -> String {
    format!(
        r#"{{ "type": "minecraftBedrock", "name": "t", "targetVersion": "{version}", "namespace": "demo",
  "beacon": {{ "scriptModules": {{ "@minecraft/server": "{module}" }},
    "targets": {{ "dev": {{ "edition": "bedrock", "artifact": "dev" }},
                  "addon": {{ "edition": "bedrock", "artifact": "mcaddon" }} }} }} }}"#
    )
}

fn project_with(version: &str, module: &str, extra: &[(&str, &str)]) -> tempfile::TempDir {
    home();
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "config.json", &config(version, module));
    for (p, b) in extra {
        write(d.path(), p, b);
    }
    d
}

fn check(root: &Path) -> BuildOutcome {
    let runs = Arc::new(AtomicUsize::new(0));
    let o = beacon_engine::build::BuildOptions {
        all: true,
        ..opts(root, None)
    };
    session(o, root, &runs).check()
}

fn codes(o: &BuildOutcome, c: Code) -> Vec<&beacon_engine::diag::Diagnostic> {
    o.diagnostics.iter().filter(|d| d.code == c).collect()
}

const FN: &str = "summon minecraft:zomblie\ngive @s minecraft:diamond\nsetblock ~ ~ ~ minecraft:stone\n# summon minecraft:nope\nexecute as @e[type=minecraft:cow] run say hi\n";

/// A target with no index of its own uses the nearest installed one (here 1.26.50 for 1.0.1):
/// one Note says so and the checks run. "Missing" is `e3_apicheck_missing.rs`.
#[test]
fn other_version_index_is_used_with_one_note() {
    let p = project_with(
        "1.0.1",
        "2.6.0",
        &[
            ("src/m/functions/a.mcfunction", FN),
            ("src/m/functions/b.mcfunction", FN),
        ],
    );
    assert!(shared("1.0.1").is_err()); // no exact index
    let o = check(p.path());
    let n = codes(&o, Code::ApiIndexMissing);
    assert_eq!(n.len(), 1, "{:?}", o.diagnostics);
    assert_eq!(n[0].severity, Severity::Note);
    assert!(
        n[0].message
            .contains("using API index 1.26.50 for targetVersion 1.0.1"),
        "{}",
        n[0].message
    );
    assert!(!codes(&o, Code::ApiUnknownVanillaId).is_empty());
}

#[test]
fn older_index_skips_the_error_checks() {
    // target newer than every installed index: its APIs may postdate the index, so no Errors
    let p = project_with(
        "9.0.0",
        "99.0.0",
        &[
            ("src/m/functions/a.mcfunction", FN),
            (
                "src/m/scripts/main.ts",
                "import { Bogus } from '@minecraft/server';",
            ),
        ],
    );
    let o = check(p.path());
    let n = codes(&o, Code::ApiIndexMissing);
    assert_eq!(n.len(), 1, "{:?}", o.diagnostics);
    assert!(n[0].message.contains("older"), "{}", n[0].message);
    assert!(codes(&o, Code::ApiUnknownScriptMember).is_empty());
    assert!(codes(&o, Code::ApiScriptModuleVersion).is_empty());
    assert!(!codes(&o, Code::ApiUnknownVanillaId).is_empty());
}

#[test]
fn unknown_vanilla_id() {
    let p = project_with(
        GAME,
        "2.6.0",
        &[
            ("src/m/functions/a.mcfunction", FN),
            (
                "src/m/recipes/r.recipe.json",
                r#"{"minecraft:recipe_shapeless":{"ingredients":[{"item":"minecraft:diamond"}],"result":{"item":"minecraft:diamnd"}}}"#,
            ),
        ],
    );
    let o = check(p.path());
    assert!(
        codes(&o, Code::ApiIndexMissing).is_empty(),
        "{:?}",
        o.diagnostics
    );
    let mut msgs: Vec<_> = codes(&o, Code::ApiUnknownVanillaId)
        .iter()
        .map(|d| d.message.clone())
        .collect();
    msgs.sort();
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    for want in ["minecraft:zomblie", "minecraft:diamnd", "minecraft:cow"] {
        assert!(msgs.iter().any(|m| m.contains(want)), "{want}: {msgs:?}");
    }
    let z = codes(&o, Code::ApiUnknownVanillaId)
        .into_iter()
        .find(|d| d.message.contains("zomblie"))
        .unwrap();
    assert_eq!(z.span.as_ref().unwrap().line, Some(1));
    assert_eq!(z.severity, Severity::Warning);
}

#[test]
fn unknown_named_import() {
    let p = project_with(
        GAME,
        "2.6.0",
        &[(
            "src/m/scripts/main.ts",
            "// import { Commented } from '@minecraft/server';\n\
             import { world, Bogus as B, type system, OldThing } from \"@minecraft/server\";\n\
             import { Other } from \"./local\";\n\
             const s = \"import { InString } from '@minecraft/server'\";\n",
        )],
    );
    let o = check(p.path());
    let bad = codes(&o, Code::ApiUnknownScriptMember);
    assert_eq!(bad.len(), 1, "{:?}", o.diagnostics);
    assert!(bad[0].message.contains("Bogus"));
    assert_eq!(bad[0].severity, Severity::Error);
    let sp = bad[0].span.as_ref().unwrap();
    assert_eq!((sp.line, sp.column), (Some(2), Some(17)));
    let dep = codes(&o, Code::ApiDeprecated);
    assert_eq!(dep.len(), 1);
    assert!(dep[0].message.contains("OldThing"));
}

#[test]
fn module_version_too_new() {
    let p = project_with(GAME, "2.9.0", &[]);
    let o = check(p.path());
    let d = codes(&o, Code::ApiScriptModuleVersion);
    assert_eq!(d.len(), 1, "{:?}", o.diagnostics);
    assert_eq!(d[0].severity, Severity::Error);
    assert!(d[0].message.contains("2.6.0"), "{}", d[0].message);
    // a beta is compared with the pinned beta
    let p = project_with(GAME, "2.7.0-beta", &[]);
    assert!(codes(&check(p.path()), Code::ApiScriptModuleVersion).is_empty());
    let p = project_with(GAME, "2.8.0-beta", &[]);
    assert_eq!(
        codes(&check(p.path()), Code::ApiScriptModuleVersion).len(),
        1
    );
    // installed typings
    let p = project_with(
        GAME,
        "2.6.0",
        &[(
            "node_modules/@minecraft/server/package.json",
            r#"{"version":"2.7.0-beta.1.26.50-stable"}"#,
        )],
    );
    assert!(codes(&check(p.path()), Code::ApiScriptModuleVersion).is_empty());
    let p = project_with(
        GAME,
        "2.6.0",
        &[(
            "node_modules/@minecraft/server/package.json",
            r#"{"version":"2.8.0"}"#,
        )],
    );
    let o = check(p.path());
    let d = codes(&o, Code::ApiScriptModuleVersion);
    assert_eq!(d.len(), 1, "{:?}", o.diagnostics);
    assert!(d[0].message.contains("installed"));
    assert_eq!(d[0].severity, beacon_engine::diag::Severity::Warning);
}

#[test]
fn unknown_component_is_a_note() {
    let p = project_with(
        GAME,
        "2.6.0",
        &[(
            "src/m/entities/z.entity.json",
            r#"{"minecraft:entity":{"description":{"identifier":"demo:z"},
                "components":{"minecraft:health":{"value":1},"minecraft:helth":{},"demo:custom":{}},
                "component_groups":{"g":{"minecraft:nonsense":{}}}}}"#,
        )],
    );
    let o = check(p.path());
    let c = codes(&o, Code::ApiUnknownComponent);
    assert_eq!(c.len(), 1, "{:?}", o.diagnostics);
    assert_eq!(c[0].severity, Severity::Note);
    assert!(c[0].message.contains("minecraft:helth"));
}

#[test]
fn builds_carry_no_api_notes() {
    let p = project_with("1.0.1", "2.6.0", &[("src/m/functions/a.mcfunction", FN)]);
    let runs = Arc::new(AtomicUsize::new(0));
    let o = session(opts(p.path(), Some("addon")), p.path(), &runs).build();
    assert!(!has(&o, Code::ApiIndexMissing));
}

fn find(dir: &Path, name: &str, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            find(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

#[test]
fn mirror_has_bundle_and_map_atomically() {
    let p = project_with(GAME, "2.6.0", &[("src/m/scripts/main.ts", "export {};")]);
    let runs = Arc::new(AtomicUsize::new(0));
    let o = session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert!(errors(&o).is_empty(), "{:?}", o.diagnostics);
    let dir = p.path().join(".beacon/debug/dev/main/scripts");
    let mirrored = std::fs::read(dir.join("main.js")).unwrap();
    assert!(dir.join("main.js.map").is_file());
    let mut emitted = Vec::new();
    find(&p.path().join("out"), "main.js", &mut emitted);
    assert!(!emitted.is_empty());
    for e in emitted {
        assert_eq!(std::fs::read(e).unwrap(), mirrored);
    }
    // no temp files left behind
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        assert!(!e.file_name().to_string_lossy().ends_with(".tmp"), "{e:?}");
    }
}
