//! Phase 3 A2: `packRoots`, `conflicts`, `autoTextures`, `scriptEntry`, `packMeta.manifest`,
//! `autoNames` (the `name_ninja` filter) and `texturesList` (the `texture_list` filter).

mod b1_common;
use b1_common::*;
use beacon_engine::build::BuildOutcome;
use beacon_engine::diag::Code;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// A project with `beacon` as the `beacon` section (no targets: the packs build) plus `extra`.
fn build(beacon: &str, extra: &[(&str, &str)]) -> (tempfile::TempDir, BuildOutcome) {
    let config = format!(
        r#"{{"type":"minecraftBedrock","name":"d1 test","targetVersion":"1.21.100","namespace":"demo","beacon":{beacon}}}"#
    );
    let mut files = vec![("config.json", config.as_str())];
    files.extend_from_slice(extra);
    let p = project(&files);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), None), p.path(), &runs).build();
    (p, r)
}

fn text(root: &Path, rel: &str) -> String {
    let f = root.join("out/main").join(rel);
    std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{f:?}: {e}"))
}

fn json_at(root: &Path, rel: &str) -> Value {
    serde_json::from_str(&text(root, rel)).unwrap()
}

#[test]
fn pack_roots_route_verbatim() {
    let (p, r) = build(
        "{}",
        &[
            (
                "src/p/module.json",
                r#"{"packRoots":{"BP":"bp","RP":"rp"}}"#,
            ),
            ("src/p/bp/entities/x.json", r#"{"a":1}"#),
            // would be a convention rule (`*.entity.json`, scripts entry) outside a pack root
            ("src/p/bp/scripts/main.ts", "export {}"),
            ("src/p/bp/weird/Thing.entity.json", "{}"),
            ("src/p/rp/odd/any.thing", "x"),
        ],
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(text(p.path(), "BP/entities/x.json"), r#"{"a":1}"#);
    assert_eq!(text(p.path(), "BP/scripts/main.ts"), "export {}");
    assert_eq!(text(p.path(), "BP/weird/Thing.entity.json"), "{}");
    assert_eq!(text(p.path(), "RP/odd/any.thing"), "x");
    // not a bundle entry: no script module in the manifest
    let m = json_at(p.path(), "BP/manifest.json");
    assert_eq!(m["modules"].as_array().unwrap().len(), 1);
}

#[test]
fn conflicts_beat_on_conflict_for_pack_roots_and_convention_files() {
    // convention puts `functions/a.mcfunction` of module `m` at `functions/m/a.mcfunction`
    let (p, r) = build(
        r#"{"sources":["./src","./src2"]}"#,
        &[
            ("src/m/functions/a.mcfunction", "one"),
            ("src/m/functions/b.mcfunction", "bee"),
            // pack root file: `conflicts` (skip) wins over `onConflict` (error)
            (
                "src/p/module.json",
                r#"{"packRoots":{"BP":"bp"},"onConflict":"error","conflicts":{"bp/functions/*/*":"skip"}}"#,
            ),
            ("src/p/bp/functions/m/a.mcfunction", "two"),
            // a convention-routed file of another module `m`: `conflicts` applies as well
            (
                "src2/m/module.json",
                r#"{"conflicts":{"functions/b.mcfunction":"skip"}}"#,
            ),
            ("src2/m/functions/b.mcfunction", "other"),
        ],
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(text(p.path(), "BP/functions/m/a.mcfunction"), "one");
    assert_eq!(text(p.path(), "BP/functions/m/b.mcfunction"), "bee");
    // without `conflicts` the same collision is an error
    let (_p, r) = build(
        "{}",
        &[
            ("src/m/functions/a.mcfunction", "one"),
            ("src/p/module.json", r#"{"packRoots":{"BP":"bp"}}"#),
            ("src/p/bp/functions/m/a.mcfunction", "two"),
        ],
    );
    assert!(has(&r, Code::RouteCollision), "{:?}", r.diagnostics);
}

#[test]
fn pack_root_merge_follows_conflicts() {
    let (p, r) = build(
        "{}",
        &[
            (
                "src/p/module.json",
                r#"{"packRoots":{"RP":"rp"},"conflicts":{"rp/texts/*.lang":"merge"}}"#,
            ),
            ("src/p/rp/texts/en_US.lang", "extra.key=Extra\n"),
        ],
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let lang = text(p.path(), "RP/texts/en_US.lang");
    assert!(lang.contains("extra.key=Extra") && lang.contains("item.demo:sword=Sword"));
}

const GEM: &str =
    r#"{"format_version":"1.21.100","minecraft:item":{"description":{"identifier":"demo:gem"}}}"#;

#[test]
fn auto_textures_false_emits_no_auto_entries() {
    let files = |mj: &'static str| {
        [
            ("src/g/items/gem.item.json", GEM),
            ("src/g/items/gem.png", "PNG"),
            ("src/g/module.json", mj),
        ]
    };
    let data = |p: &tempfile::TempDir| {
        json_at(p.path(), "RP/textures/item_texture.json")["texture_data"].clone()
    };
    let (p, r) = build("{}", &files("{}"));
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(data(&p).get("demo:gem").is_some());

    let (p, r) = build("{}", &files(r#"{"autoTextures":false}"#));
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    // module `m` still registers its own sibling png
    assert_eq!(
        data(&p),
        json!({ "demo:sword": { "textures": "textures/items/sword" } })
    );

    // explicit entries still apply
    let (p, r) = build(
        "{}",
        &files(r#"{"autoTextures":false,"itemTextures":{"gem":"items/gem.png"}}"#),
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(data(&p)["demo:gem"]["textures"], "textures/items/gem");
}

#[test]
fn script_entry_moves_bundle_map_and_manifest_entry() {
    let scripts = [("src/s/scripts/main.ts", "export {}")];
    let (p, r) = build(
        r#"{"outputs":[{"id":"main","include":["src/**"],"scriptEntry":"scripts/game/index.js"}]}"#,
        &scripts,
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(text(p.path(), "BP/scripts/game/index.js").contains("entries"));
    assert!(
        p.path()
            .join("out/main/BP/scripts/game/index.js.map")
            .exists()
    );
    assert!(!p.path().join("out/main/BP/scripts/main.js").exists());
    let m = json_at(p.path(), "BP/manifest.json");
    assert_eq!(m["modules"][1]["entry"], "scripts/game/index.js");

    let (p, r) = build("{}", &scripts);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(p.path().join("out/main/BP/scripts/main.js").exists());
    assert_eq!(
        json_at(p.path(), "BP/manifest.json")["modules"][1]["entry"],
        "scripts/main.js"
    );

    for bad in ["scripts/main.ts", "../x.js", "/abs.js", "a/./b.js"] {
        let (_p, r) = build(
            &format!(
                r#"{{"outputs":[{{"id":"main","include":["src/**"],"scriptEntry":"{bad}"}}]}}"#
            ),
            &[],
        );
        assert!(has(&r, Code::ConfigField), "{bad}: {:?}", r.diagnostics);
    }
}

#[test]
fn pack_meta_manifest_extras_merge_but_identity_is_locked() {
    let (p, r) = build(
        r#"{"packMeta":{"manifest":{
            "BP":{"header":{"pack_scope":"world"},"metadata":{"authors":["me"]},"capabilities":["script_eval"]},
            "RP":{"capabilities":["pbr"]}}}}"#,
        &[],
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let bp = json_at(p.path(), "BP/manifest.json");
    assert_eq!(bp["header"]["pack_scope"], "world");
    assert_eq!(bp["header"]["name"], "pack.name");
    assert_eq!(bp["header"]["uuid"].as_str().unwrap().len(), 36);
    assert_eq!(bp["metadata"]["authors"][0], "me");
    assert_eq!(bp["capabilities"][0], "script_eval");
    assert_eq!(
        json_at(p.path(), "RP/manifest.json")["capabilities"][0],
        "pbr"
    );

    for bad in [
        r#"{"BP":{"header":{"uuid":"x"}}}"#,
        r#"{"RP":{"header":{"version":[2,0,0]}}}"#,
        r#"{"BP":{"modules":[{"uuid":"x"}]}}"#,
        r#"{"BP":{"modules":[{"version":[1,0,0]}]}}"#,
        r#"{"BP":{"format_version":1}}"#,
        r#"{"BP":{"header":"x"}}"#,
        r#"{"BP":{"header":null}}"#,
        r#"{"BP":{"modules":{}}}"#,
        r#"{"BP":{"modules":"x"}}"#,
        r#"{"RP":{"modules":[]}}"#,
        r#"{"BP":"x"}"#,
    ] {
        let (_p, r) = build(&format!(r#"{{"packMeta":{{"manifest":{bad}}}}}"#), &[]);
        assert!(has(&r, Code::ConfigField), "{bad}: {:?}", r.diagnostics);
    }
}

/// The expected lines come from running the real `name_ninja.py` (reticulator, Python 3.12) in
/// a scratch project with the same identifiers and
/// `{"entities":{"auto_name":true},"items":{"auto_name":true},"blocks":{"auto_name":true},
///   "spawn_eggs":{"auto_name":true,"prefix":"S: ","postfix":"!"}}`. The filter rewrites a
/// whole file (CRLF on Windows); only the generated lines are compared, as appended here.
#[test]
fn auto_names_match_name_ninja() {
    let def = |root: &str, fv: &str, id: &str| {
        format!(r#"{{"format_version":"{fv}","{root}":{{"description":{{"identifier":"{id}"}}}}}}"#)
    };
    let files = [
        (
            "src/n/entities/giant_frog.entity.json",
            def("minecraft:entity", "1.17.0", "demo:giant_frog"),
        ),
        (
            "src/n/entities/boss_2fight.entity.json",
            def("minecraft:entity", "1.17.0", "demo:boss_2fight"),
        ),
        // explicit `description.name`: translated as is (name_ninja pops it into the lang)
        (
            "src/n/entities/named.entity.json",
            r#"{"format_version":"1.17.0","minecraft:entity":{"description":{"identifier":"demo:named","name":"The Named One"}}}"#.to_string(),
        ),
        (
            "src/n/entities/zombie.entity.json",
            def("minecraft:entity", "1.17.0", "minecraft:zombie"),
        ),
        (
            "src/n/items/ruby_sword.item.json",
            def("minecraft:item", "1.20.0", "demo:ruby_sword"),
        ),
        (
            "src/n/items/old_item.item.json",
            def("minecraft:item", "1.10.0", "demo:old_item"),
        ),
        (
            "src/n/blocks/ore_block.block.json",
            def("minecraft:block", "1.20.0", "demo:ore_block"),
        ),
    ];
    let mut extra: Vec<(&str, &str)> = files.iter().map(|(p, b)| (*p, b.as_str())).collect();
    // an existing key: untouched (name_ninja runs with overwrite false)
    extra.push((
        "src/n/texts/en_US.lang",
        "entity.demo:giant_frog.name=Frog Custom\n",
    ));
    let (p, r) = build(
        r#"{"autoNames":{"entities":true,"items":true,"blocks":true,
            "spawnEggs":{"prefix":"S: ","postfix":"!"}}}"#,
        &extra,
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    // `demo:sword` (module m) already has its name: not repeated
    assert_eq!(
        text(p.path(), "RP/texts/en_US.lang"),
        "pack.name=d1 test\npack.description=\n\
         item.demo:sword=Sword\nentity.demo:giant_frog.name=Frog Custom\n\
         item.spawn_egg.entity.demo:boss_2fight.name=S: Boss 2Fight!\t##\n\
         item.spawn_egg.entity.demo:giant_frog.name=S: Giant Frog!\t##\n\
         item.spawn_egg.entity.demo:named.name=The Named One\t##\n\
         item.demo:old_item.name=Old Item\t##\n\
         item.demo:ruby_sword=Ruby Sword\t##\n\
         tile.demo:ore_block.name=Ore Block\t##\n\
         entity.demo:boss_2fight.name=Boss 2Fight\t##\n\
         entity.demo:named.name=The Named One\t##\n"
    );
    assert!(!text(p.path(), "RP/texts/en_US.lang").contains("zombie"));
    // oracle: the migration's shared rule set (`migrate::codemod`) yields the same pairs
    let opts = beacon_engine::migrate::codemod::AutoNameOpts {
        entities: true,
        items: true,
        blocks: true,
        spawn_eggs: Some(("S: ".into(), "!".into())),
        ignored_namespaces: None,
    };
    let lang = text(p.path(), "RP/texts/en_US.lang");
    for (_, body) in files.iter() {
        let v: Value = serde_json::from_str(body).unwrap();
        for (k, val) in beacon_engine::migrate::codemod::auto_name_keys(&v, &opts) {
            // the pre-existing key keeps its own value
            let want = if k == "entity.demo:giant_frog.name" {
                "Frog Custom\n".to_string()
            } else {
                format!("{val}\t##\n")
            };
            assert!(lang.contains(&format!("{k}={want}")), "{k}");
        }
    }
}

#[test]
fn auto_names_off_by_default() {
    let (p, _r) = build("{}", &[]);
    assert!(!text(p.path(), "RP/texts/en_US.lang").contains("\t##"));
}

/// Expected: `texture_list.py` on the same textures: `json.dump(list, indent='\t')`, paths
/// relative to the RP without extension, every png then every tga, non-ASCII escaped.
#[test]
fn textures_list_matches_texture_list() {
    let (p, r) = build(
        r#"{"texturesList":true}"#,
        &[
            ("src/t/blocks/ore.png", "x"),
            ("src/t/blocks/a.b.png", "x"),
            ("src/t/blocks/z.tga", "x"),
            ("src/t/items/Zap.png", "x"),
            ("src/t/items/gem.tga", "x"),
            ("src/t/items/\u{c4}pfel.png", "x"),
        ],
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        text(p.path(), "RP/textures/textures_list.json"),
        "[\n\t\"textures/blocks/a.b\",\n\t\"textures/blocks/ore\",\n\t\"textures/items/Zap\",\n\
         \t\"textures/items/sword\",\n\t\"textures/items/\\u00c4pfel\",\n\t\"textures/blocks/z\",\n\
         \t\"textures/items/gem\"\n]"
    );
    let (p, _r) = build("{}", &[("src/t/blocks/ore.png", "x")]);
    assert!(
        !p.path()
            .join("out/main/RP/textures/textures_list.json")
            .exists()
    );
}
