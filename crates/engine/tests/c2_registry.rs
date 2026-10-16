//! Derived registries (5.3): item/terrain textures and hooks.

mod b1_common;
use b1_common::*;
use beacon_engine::diag::Code;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

/// Builds `extra` on top of the b1 project (module `m`: `sword` + png); returns the outcome.
fn build(extra: &[(&str, &str)]) -> (tempfile::TempDir, beacon_engine::build::BuildOutcome) {
    let p = project(extra);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), None), p.path(), &runs).build();
    (p, r)
}

fn json_at(root: &Path, rel: &str) -> Value {
    let f = root.join("out/main").join(rel);
    serde_json::from_slice(&std::fs::read(&f).unwrap_or_else(|e| panic!("{f:?}: {e}"))).unwrap()
}

fn text_at(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("out/main").join(rel)).unwrap()
}

const BLOCK: &str = r#"{"minecraft:block":{"description":{"identifier":"demo:ore"}}}"#;

#[test]
fn sibling_png_registers() {
    let (p, r) = build(&[
        ("src/m/blocks/ore.block.json", BLOCK),
        ("src/m/blocks/ore.png", "PNG"),
    ]);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let it = json_at(p.path(), "RP/textures/item_texture.json");
    assert_eq!(it["texture_name"], "atlas.items");
    assert_eq!(
        it["texture_data"],
        json!({ "demo:sword": { "textures": "textures/items/sword" } })
    );
    let t = json_at(p.path(), "RP/textures/terrain_texture.json");
    assert_eq!(t["texture_name"], "atlas.terrain");
    assert_eq!(
        t["texture_data"],
        json!({ "demo:ore": { "textures": "textures/blocks/ore" } })
    );
}

#[test]
fn explicit_entries_and_equal_keys_merge() {
    // `sword` equals the convention entry; `gem` is explicit only
    let (p, r) = build(&[
        ("src/m/items/gem.png", "PNG"),
        (
            "src/m/module.json",
            r#"{"itemTextures":{"sword":"items/sword.png","gem":"items/gem.png"}}"#,
        ),
    ]);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let d = &json_at(p.path(), "RP/textures/item_texture.json")["texture_data"];
    assert_eq!(d["demo:sword"]["textures"], "textures/items/sword");
    assert_eq!(d["demo:gem"]["textures"], "textures/items/gem");
}

#[test]
fn conflicting_key_across_modules() {
    let (_p, r) = build(&[
        ("src/n/items/other.png", "PNG"),
        (
            "src/n/module.json",
            r#"{"itemTextures":{"sword":"items/other.png"}}"#,
        ),
    ]);
    assert!(has(&r, Code::MergeConflict), "{:?}", r.diagnostics);
}

#[test]
fn explicit_entry_without_texture_is_reported() {
    let (_p, r) = build(&[(
        "src/m/module.json",
        r#"{"itemTextures":{"x":"items/no.png"}}"#,
    )]);
    assert!(has(&r, Code::ModuleJsonInvalid), "{:?}", r.diagnostics);
}

#[test]
fn handwritten_registry_merges_with_derived() {
    let hand = r#"{"resource_pack_name":"vanilla","texture_name":"atlas.items",
        "texture_data":{"demo:legacy":{"textures":"textures/items/legacy"}}}"#;
    let module = r#"{"onConflict":"merge","paths":{"textures/*.json":"RP/textures"}}"#;
    let (p, r) = build(&[
        ("src/m/textures/item_texture.json", hand),
        ("src/m/module.json", module),
    ]);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let d = &json_at(p.path(), "RP/textures/item_texture.json")["texture_data"];
    assert!(
        d["demo:legacy"].is_object() && d["demo:sword"].is_object(),
        "{d}"
    );
    // without `onConflict: merge` the hand-written file collides
    let (_p, r) = build(&[
        ("src/m/textures/item_texture.json", hand),
        (
            "src/m/module.json",
            r#"{"paths":{"textures/*.json":"RP/textures"}}"#,
        ),
    ]);
    assert!(has(&r, Code::RouteCollision), "{:?}", r.diagnostics);
}

const FN: &str = "say hi\n";

#[test]
fn hooks_order_and_init() {
    let (p, r) = build(&[
        ("src/m/functions/a.mcfunction", FN),
        ("src/m/functions/b.mcfunction", FN),
        ("src/n/functions/c.mcfunction", FN),
        (
            "src/m/module.json",
            r#"{"hooks":{"tick":{"functions/a":5,"functions/b":0},"init":{"functions/a":1}}}"#,
        ),
        (
            "src/n/module.json",
            r#"{"hooks":{"tick":{"functions/c":5},"init":{"functions/c":0}}}"#,
        ),
    ]);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    // priority first, then origin (m before n)
    assert_eq!(
        json_at(p.path(), "BP/functions/tick.json"),
        json!({ "values": ["m/b", "m/a", "n/c"] })
    );
    assert_eq!(
        text_at(p.path(), "BP/functions/beacon/init.mcfunction"),
        "function n/c\nfunction m/a\n"
    );
}

#[test]
fn unknown_hook_function() {
    let (_p, r) = build(&[(
        "src/m/module.json",
        r#"{"hooks":{"tick":{"functions/nope":0}}}"#,
    )]);
    assert!(has(&r, Code::HookUnknownFunction), "{:?}", r.diagnostics);
}
