//! Phase 3 A3: one-time migration codemods.

use beacon_engine::migrate::codemod::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn scope(_p: &str, v: &str) -> Option<String> {
    Some(
        match v {
            "underscore_namespace" => "dc_",
            "colon_namespace" => "dc:",
            "subfunction_path_namespace" => "sub/",
            "path_namespace" => "dc/",
            _ => return None,
        }
        .into(),
    )
}

fn sv(v: &str) -> Value {
    json!({"type": "scope_variable", "path": "scope.json", "value": v})
}

fn run(path: &str, text: &str, b: &[Batch]) -> Option<String> {
    text_replace(path, text.as_bytes(), b).map(|(b, _)| String::from_utf8(b).unwrap())
}

#[test]
fn text_replacer_batches_are_ordered_and_one_pass() {
    // the real project's shape: batch 1 = shapescape_ -> ns, batch 2 fixes up the result
    let s = json!({"batches": [
        {"rules": [
            {"source": "shapescape_", "target": sv("underscore_namespace")},
            {"source": "shapescape:", "target": sv("colon_namespace")},
            {"source": "shapescape/", "target": "sub/"},
        ]},
        {"rules": [{"source": sv("subfunction_path_namespace"), "target": sv("path_namespace")}]},
    ]});
    let b = text_replacer_batches(&s, &scope).unwrap();
    assert_eq!(b.len(), 2);
    // rule 2 fixup after rule 1: shapescape/ -> sub/ -> dc/
    assert_eq!(
        run("BP/x.json", "shapescape/a shapescape:b shapescape_c", &b).unwrap(),
        "dc/a dc:b dc_c"
    );
    // within one batch replaced text is not rescanned
    let one = text_replacer_batches(
        &json!({"rules": [{"source": "a", "target": "b"}, {"source": "b", "target": "c"}]}),
        &scope,
    )
    .unwrap();
    assert_eq!(run("RP/x.txt", "ab", &one).unwrap(), "bc");
    // `rules` and `batches` together are an error; unresolved scope variable too
    assert!(text_replacer_batches(&json!({"rules": [], "batches": []}), &scope).is_err());
    assert!(
        text_replacer_batches(
            &json!({"rules": [{"source": "a", "target": sv("nope")}]}),
            &scope
        )
        .is_err()
    );
}

#[test]
fn text_replacer_exclusions_and_crlf() {
    let b = vec![vec![("x".to_string(), "y".to_string())]];
    assert_eq!(
        run("BP/a.mcfunction", "x\r\nx\r\n", &b).unwrap(),
        "y\r\ny\r\n"
    );
    assert!(text_replace("BP/a.png", b"x", &b).is_none());
    assert!(text_replace("BP/a.nbt", b"x", &b).is_none());
    assert!(text_replace("BP/scripts/node_modules/p/i.js", b"x", &b).is_none());
    assert!(text_replace("src/x.txt", b"x", &b).is_none());
    assert!(text_replace("BP/bin.dat", &[0xff, 0xfe, b'x'], &b).is_none());
    assert!(text_replace("BP/a.txt", b"nothing", &b).is_none());
}

fn entity(id: &str) -> Vec<u8> {
    json!({"format_version": "1.20.0", "minecraft:entity": {
        "description": {"identifier": id},
        "component_groups": {"g": {"minecraft:type_family": {"family": ["mob"]}}},
        "components": {"minecraft:health": {"value": 1}}
    }})
    .to_string()
    .into_bytes()
}

#[test]
fn entity_patch_matches_shapescape_default_and_is_idempotent() {
    let (out, _) = entity_patch("BP/entities/a/b.json", &entity("dc:golem")).unwrap();
    let v: Value = serde_json::from_slice(&out).unwrap();
    let e = &v["minecraft:entity"];
    assert_eq!(
        e["components"]["minecraft:type_family"]["family"],
        json!(["shapescape", "shapescape_"])
    );
    assert_eq!(
        e["component_groups"]["g"]["minecraft:type_family"]["family"],
        json!(["mob", "shapescape", "shapescape_"])
    );
    assert_eq!(
        e["component_groups"]["shapescape:despawn"],
        json!({"minecraft:instant_despawn": {}})
    );
    assert_eq!(
        e["events"]["shapescape:despawn"],
        json!({"add": {"component_groups": ["shapescape:despawn"]}})
    );
    assert!(
        String::from_utf8(out.clone())
            .unwrap()
            .contains("\n    \"format_version\"")
    );
    assert!(
        entity_patch("BP/entities/a/b.json", &out).is_none(),
        "second run changes nothing"
    );
    // vanilla, non-entity paths and non-entities are skipped
    assert!(entity_patch("BP/entities/z.json", &entity("minecraft:zombie")).is_none());
    assert!(entity_patch("BP/items/z.json", &entity("dc:golem")).is_none());
    assert!(entity_patch("BP/entities/z.json", b"{\"minecraft:item\":{}}").is_none());
}

#[test]
fn converted_from_is_stripped_from_blocks_only() {
    let b = br#"{"minecraft:block": {"description": {"identifier": "dc:b", "converted_from": ["minecraft:stone"]}}}"#;
    let (out, _) = strip_converted_from("BP/blocks/b.json", b).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(!s.contains("converted_from") && s.contains("\n\t\t"), "{s}");
    assert!(strip_converted_from("BP/entities/b.json", b).is_none());
    let falsy = br#"{"minecraft:block": {"description": {"converted_from": ""}}}"#;
    assert!(strip_converted_from("BP/blocks/b.json", falsy).is_none());
}

#[test]
fn lang_dedupe_keeps_last_value() {
    let src = "a=1\r\n## c\r\nb=2\r\na=3\r\nb=4\r\nc=5\r\n";
    let (out, e) = lang_dedupe("RP/texts/en_US.lang", src.as_bytes()).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "## c\r\na=3\r\nb=4\r\nc=5\r\n"
    );
    assert!(e.why.contains("a, b"));
    assert!(lang_dedupe("RP/texts/en_US.lang", b"a=1\nb=2\n").is_none());
}

#[test]
fn auto_names_follow_name_ninja() {
    assert_eq!(
        auto_name_value("dc:iron_golem_2x").unwrap(),
        "Iron Golem 2X"
    );
    let o = AutoNameOpts {
        entities: true,
        items: true,
        blocks: true,
        spawn_eggs: Some(("\u{a7}fSpawn \u{a7}f".into(), "!".into())),
        ..Default::default()
    };
    let ent = json!({"minecraft:entity": {"description": {"identifier": "dc:ruby_golem"}}});
    assert_eq!(
        auto_name_keys(&ent, &o),
        [
            (
                "item.spawn_egg.entity.dc:ruby_golem.name".into(),
                "\u{a7}fSpawn \u{a7}fRuby Golem!".into()
            ),
            (
                "entity.dc:ruby_golem.name".to_string(),
                "Ruby Golem".to_string()
            ),
        ]
    );
    let item = json!({"format_version": "1.20.0", "minecraft:item": {"description": {"identifier": "dc:gem"}}});
    assert_eq!(auto_name_keys(&item, &o)[0].0, "item.dc:gem");
    let old = json!({"format_version": "1.10.0", "minecraft:item": {"description": {"identifier": "dc:gem"}}});
    assert_eq!(auto_name_keys(&old, &o)[0].0, "item.dc:gem.name");
    let block = json!({"minecraft:block": {"description": {"identifier": "dc:ore"}}});
    assert_eq!(auto_name_keys(&block, &o)[0].0, "tile.dc:ore.name");
    // vanilla namespace is skipped
    let van = json!({"minecraft:block": {"description": {"identifier": "minecraft:ore"}}});
    assert!(auto_name_keys(&van, &o).is_empty());
    let named =
        json!({"minecraft:block": {"description": {"identifier": "dc:ore", "name": "Ore"}}});
    assert_eq!(auto_name_keys(&named, &o)[0].1, "Ore");

    let gen_: BTreeMap<String, String> = auto_name_keys(&block, &o).into_iter().collect();
    let lang = "tile.dc:ore.name=Edited\ntile.dc:ore2.name=Ore2\n";
    assert!(strip_auto_names("RP/texts/en_US.lang", lang.as_bytes(), &gen_).is_none());
    let lang = "tile.dc:ore.name=Ore\t## x\nkeep=1\n";
    let g2 = BTreeMap::from([("tile.dc:ore.name".to_string(), "Ore".to_string())]);
    let (out, _) = strip_auto_names("RP/texts/en_US.lang", lang.as_bytes(), &g2).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "keep=1\n");
}

#[test]
fn explicit_names_are_emitted_even_when_auto_name_is_off() {
    let off = AutoNameOpts::default();
    let ent = json!({"minecraft:entity": {"description": {"identifier": "dc:golem", "name": "Big Golem"}}});
    let egg = (
        "item.spawn_egg.entity.dc:golem.name".to_string(),
        "Big Golem".to_string(),
    );
    let name = ("entity.dc:golem.name".to_string(), "Big Golem".to_string());
    // spawn egg first (name_ninja gathers spawn eggs before entities)
    assert_eq!(auto_name_keys(&ent, &off), [egg.clone(), name.clone()]);
    // the explicit name is used raw for the egg: no prefix/postfix
    let with = AutoNameOpts {
        spawn_eggs: Some(("Spawn ".into(), "!".into())),
        ..Default::default()
    };
    assert_eq!(auto_name_keys(&ent, &with), [egg, name]);
}

#[test]
fn spawn_egg_name_overrides_name_for_the_egg_only() {
    // name_ninja >= 1.3.1 reads `spawn_egg_name` (raw) before `name` for the egg
    let o = AutoNameOpts {
        entities: true,
        items: false,
        blocks: false,
        spawn_eggs: Some(("S: ".into(), "!".into())),
        ignored_namespaces: None,
    };
    let e = json!({"minecraft:entity": {"description": {
        "identifier": "demo:frog", "name": "Frog", "spawn_egg_name": "Frog Egg"}}});
    assert_eq!(
        auto_name_keys(&e, &o),
        vec![
            (
                "item.spawn_egg.entity.demo:frog.name".to_string(),
                "Frog Egg".to_string()
            ),
            ("entity.demo:frog.name".to_string(), "Frog".to_string()),
        ]
    );
}
