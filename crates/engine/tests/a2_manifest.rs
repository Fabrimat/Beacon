mod a2_common;
use a2_common::cfg;
use beacon_engine::config::ProjectConfig;
use beacon_engine::lock::{Lockfile, dev_uuid};
use beacon_engine::manifest::{ManifestInput, generate};
use beacon_engine::model::{Body, ContentItem, Edition, Merge, Pack};
use serde_json::Value;

fn manifest(items: &[ContentItem], pack: Pack) -> Value {
    let i = items
        .iter()
        .find(|i| i.dest.pack == pack && i.dest.path.as_str() == "manifest.json")
        .unwrap();
    assert_eq!(i.merge, Merge::Error);
    match &i.body {
        Body::Json(v) => v.clone(),
        _ => panic!(),
    }
}

const BEACON: &str = r#"{"scriptModules":{"@minecraft/server":"2.0.0"},"packMeta":{"name":"Shown","icon":"./art/icon.png"},
  "targets":{"b":{"edition":"bedrock","artifact":"dev"},"e":{"edition":"education","artifact":"dev","targetVersion":"1.21.50"}}}"#;

fn setup() -> (tempfile::TempDir, ProjectConfig, Lockfile) {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(BEACON);
    let l = Lockfile::ensure(d.path(), &c, &["main".to_string()], &[], &[], false).unwrap();
    (d, c, l)
}

fn input<'a>(
    c: &'a ProjectConfig,
    l: &'a Lockfile,
    out: &'a String,
    edition: Edition,
    dev: Option<&'a std::path::Path>,
    script: Option<&'a str>,
    has_rp: bool,
) -> ManifestInput<'a> {
    ManifestInput {
        cfg: c,
        lock: l,
        output: out,
        edition,
        target_version: match edition {
            Edition::Bedrock => &c.target_version,
            Edition::Education => "1.21.50",
        },
        dev_worktree: dev,
        script_entry: script,
        has_bp: true,
        has_rp,
    }
}

#[test]
fn release_manifest() {
    let (_d, c, l) = setup();
    let out = "main".to_string();
    let m = input(
        &c,
        &l,
        &out,
        Edition::Education,
        None,
        Some("scripts/main.js"),
        true,
    );
    let items = generate(&m);
    let bp = manifest(&items, Pack::Behavior);
    let rp = manifest(&items, Pack::Resource);
    assert_eq!(bp["format_version"], 2);
    assert_eq!(
        bp["header"]["min_engine_version"],
        serde_json::json!([1, 21, 50])
    );
    assert_eq!(
        bp["header"]["uuid"],
        l.pack_uuid("main", Pack::Behavior, Edition::Education)
            .unwrap()
    );
    assert_eq!(bp["header"]["name"], "pack.name");
    assert_eq!(bp["modules"][0]["type"], "data");
    assert_eq!(bp["modules"][1]["type"], "script");
    assert_eq!(bp["modules"][1]["language"], "javascript");
    assert_eq!(bp["modules"][1]["entry"], "scripts/main.js");
    assert_eq!(
        bp["modules"][1]["uuid"],
        l.script_module_uuid("main").unwrap()
    );
    assert_eq!(bp["dependencies"][0]["uuid"], rp["header"]["uuid"]);
    assert_eq!(
        bp["dependencies"][0]["version"],
        serde_json::json!([1, 0, 0])
    );
    assert_eq!(bp["dependencies"][1]["module_name"], "@minecraft/server");
    assert_eq!(bp["dependencies"][1]["version"], "2.0.0");
    assert_eq!(rp["modules"][0]["type"], "resources");
    assert!(rp.get("dependencies").is_none());
    let lang = items
        .iter()
        .find(|i| i.dest.pack == Pack::Resource && i.dest.path.as_str() == "texts/en_US.lang")
        .unwrap();
    assert_eq!(lang.merge, Merge::LangLines);
    assert_eq!(
        lang.body,
        Body::Text("pack.name=Shown\npack.description=\n".into())
    );
    assert!(items.iter().any(|i| i.dest.path.as_str() == "pack_icon.png"
        && matches!(&i.body, Body::Passthrough(p) if p.as_str() == "art/icon.png")));
}

#[test]
fn dev_uuids_consistent_and_default_min_engine() {
    let (d, c, l) = setup();
    let out = "main".to_string();
    let m = input(
        &c,
        &l,
        &out,
        Edition::Bedrock,
        Some(d.path()),
        Some("scripts/main.js"),
        true,
    );
    let items = generate(&m);
    let bp = manifest(&items, Pack::Behavior);
    let rp = manifest(&items, Pack::Resource);
    let rel_rp = l
        .pack_uuid("main", Pack::Resource, Edition::Bedrock)
        .unwrap();
    let rel_bp = l
        .pack_uuid("main", Pack::Behavior, Edition::Bedrock)
        .unwrap();
    assert_eq!(rp["header"]["uuid"], dev_uuid(&rel_rp, d.path()));
    assert_eq!(bp["dependencies"][0]["uuid"], rp["header"]["uuid"]);
    assert_eq!(bp["header"]["uuid"], dev_uuid(&rel_bp, d.path()));
    assert_eq!(
        bp["modules"][1]["uuid"],
        dev_uuid(&l.script_module_uuid("main").unwrap(), d.path())
    );
    assert_eq!(
        bp["modules"][0]["uuid"],
        dev_uuid(
            &l.module_uuid("main", Pack::Behavior, Edition::Bedrock)
                .unwrap(),
            d.path()
        )
    );
    assert_ne!(bp["header"]["uuid"], Value::String(rel_bp));
    assert_eq!(
        bp["header"]["min_engine_version"],
        serde_json::json!([1, 21, 100])
    );
}

#[test]
fn bp_only_has_no_rp_dependency_or_script() {
    let (_d, c, l) = setup();
    let out = "main".to_string();
    let items = generate(&input(&c, &l, &out, Edition::Bedrock, None, None, false));
    assert!(items.iter().all(|i| i.dest.pack == Pack::Behavior));
    let bp = manifest(&items, Pack::Behavior);
    assert!(bp.get("dependencies").is_none());
    assert_eq!(bp["modules"].as_array().unwrap().len(), 1);
}

#[test]
fn pack_dependencies_rp_bp_points_rp_at_bp() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(
        r#"{"scriptModules":{"@minecraft/server":"2.0.0"},"packMeta":{"packDependencies":"rp-bp"},
  "targets":{"b":{"edition":"bedrock","artifact":"dev"}}}"#,
    );
    let l = Lockfile::ensure(d.path(), &c, &["main".to_string()], &[], &[], false).unwrap();
    let out = "main".to_string();
    let items = generate(&input(
        &c,
        &l,
        &out,
        Edition::Bedrock,
        None,
        Some("scripts/main.js"),
        true,
    ));
    let (bp, rp) = (
        manifest(&items, Pack::Behavior),
        manifest(&items, Pack::Resource),
    );
    assert_eq!(rp["dependencies"][0]["uuid"], bp["header"]["uuid"]);
    assert_eq!(rp["dependencies"][0]["version"], bp["header"]["version"]);
    // never both ways: only the script module dependency stays on the BP
    assert_eq!(bp["dependencies"].as_array().unwrap().len(), 1);
    assert_eq!(bp["dependencies"][0]["module_name"], "@minecraft/server");
}

#[test]
fn pack_dependencies_mutual_links_both_ways() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(
        r#"{"scriptModules":{"@minecraft/server":"2.0.0"},"packMeta":{"packDependencies":"mutual"},
  "targets":{"b":{"edition":"bedrock","artifact":"dev"}}}"#,
    );
    let l = Lockfile::ensure(d.path(), &c, &["main".to_string()], &[], &[], false).unwrap();
    let out = "main".to_string();
    let items = generate(&input(
        &c,
        &l,
        &out,
        Edition::Bedrock,
        None,
        Some("scripts/main.js"),
        true,
    ));
    let (bp, rp) = (
        manifest(&items, Pack::Behavior),
        manifest(&items, Pack::Resource),
    );
    assert_eq!(rp["dependencies"][0]["uuid"], bp["header"]["uuid"]);
    assert_eq!(rp["dependencies"][0]["version"], bp["header"]["version"]);
    assert_eq!(bp["dependencies"][0]["uuid"], rp["header"]["uuid"]);
    assert_eq!(bp["dependencies"][0]["version"], rp["header"]["version"]);
    assert_eq!(bp["dependencies"][1]["module_name"], "@minecraft/server");
}
