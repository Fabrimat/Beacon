//! Phase 3 A2: `Lockfile::import_manifests` and `rename_output`.

use beacon_engine::diag::Code;
use beacon_engine::lock::{Lockfile, Release};
use beacon_engine::model::{Edition, Pack};
use serde_json::{Value, json};

fn manifest(
    header: &str,
    module: &str,
    kind: &str,
    version: [u32; 3],
    script: Option<&str>,
) -> Value {
    let mut modules = vec![json!({"type": kind, "uuid": module, "version": [1, 0, 0]})];
    if let Some(s) = script {
        modules.push(json!({"type": "script", "language": "javascript", "uuid": s, "entry": "scripts/main.js", "version": [1, 0, 0]}));
    }
    json!({"format_version": 2, "header": {"uuid": header, "version": version}, "modules": modules})
}

#[test]
fn imports_uuids_versions_and_script_module() {
    let mut l = Lockfile::default();
    let bp = manifest("h-bp", "m-bp", "data", [1, 4, 2], Some("s-1"));
    let rp = manifest("h-rp", "m-rp", "resources", [2, 0, 0], None);
    l.import_manifests("main", Some(Edition::Bedrock), Some(&bp), Some(&rp))
        .unwrap();
    let ed = Edition::Bedrock;
    assert_eq!(l.pack_uuid("main", Pack::Behavior, ed).unwrap(), "h-bp");
    assert_eq!(l.module_uuid("main", Pack::Behavior, ed).unwrap(), "m-bp");
    assert_eq!(l.pack_uuid("main", Pack::Resource, ed).unwrap(), "h-rp");
    assert_eq!(l.module_uuid("main", Pack::Resource, ed).unwrap(), "m-rp");
    assert_eq!(l.script_module_uuid("main").unwrap(), "s-1");
    assert_eq!(l.version_of("main", Pack::Behavior, ed), [1, 4, 2]);
    assert_eq!(l.version_of("main", Pack::Resource, ed), [2, 0, 0]);
    let rel = l.entry("main/BP", ed).unwrap().release.clone();
    assert_eq!(
        rel,
        Some(Release {
            version: [1, 4, 2],
            hash: String::new()
        })
    );
}

#[test]
fn never_overwrites_an_existing_entry() {
    let mut l = Lockfile::default();
    let a = manifest("h1", "m1", "data", [1, 0, 1], Some("s1"));
    l.import_manifests("main", None, Some(&a), None).unwrap();
    let before = l.clone();
    let b = manifest("h2", "m2", "data", [9, 9, 9], Some("s2"));
    // same slot: untouched
    l.import_manifests("main", None, Some(&b), None).unwrap();
    assert_eq!(l, before);
    // another edition of an existing output: added, the script module stays
    l.import_manifests("main", Some(Edition::Education), Some(&b), None)
        .unwrap();
    assert_eq!(l.script_module_uuid("main").unwrap(), "s1");
    assert_eq!(l.outputs["main"].shared.as_ref().unwrap().bp.header, "h1");
    assert_eq!(
        l.outputs["main"].editions[&Edition::Education].bp.header,
        "h2"
    );
    // a pack without a manifest gets fresh identities
    assert_eq!(
        l.outputs["main"].editions[&Edition::Education]
            .rp
            .header
            .len(),
        36
    );
}

#[test]
fn invalid_manifest_is_a_diagnostic_and_changes_nothing() {
    let mut l = Lockfile::default();
    let bad = json!({"header": {}});
    let d = l
        .import_manifests("main", None, Some(&bad), None)
        .unwrap_err();
    assert_eq!(d.code, Code::LockfileInvalid);
    assert!(l.outputs.is_empty());
}

#[test]
fn rename_output_keeps_imported_identity() {
    let mut l = Lockfile::default();
    let bp = manifest("h", "m", "data", [1, 2, 3], Some("s"));
    l.import_manifests("old", Some(Edition::Bedrock), Some(&bp), None)
        .unwrap();
    l.rename_output("old", "new").unwrap();
    assert_eq!(
        l.pack_uuid("new", Pack::Behavior, Edition::Bedrock)
            .unwrap(),
        "h"
    );
    assert_eq!(
        l.version_of("new", Pack::Behavior, Edition::Bedrock),
        [1, 2, 3]
    );
}
