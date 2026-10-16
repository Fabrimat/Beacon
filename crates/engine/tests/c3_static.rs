//! Static outputs (4.3) and world pack lists: passthrough, validation, duplicate UUIDs, order.

mod b1_common;
mod c1_common;
use b1_common::{errors, has, opts, session, write};
use beacon_engine::diag::Code;
use c1_common::fixture_bytes;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const BP_UUID: &str = "6b1d3c1e-2f4a-4c7b-9d3e-1a2b3c4d5e6f";

/// BOM + JSONC, on purpose.
fn bp_manifest(uuid: &str) -> String {
    format!(
        "\u{feff}{{\n  // vendor\n  \"format_version\": 2,\n  \"header\": {{ \"name\": \"V\", \"uuid\": \"{uuid}\", \"version\": [1, 2, 0] /* v */ }},\n  \"modules\": [{{ \"type\": \"data\", \"uuid\": \"0f9e8d7c-6b5a-4f3e-8d2c-1b0a9f8e7d6c\", \"version\": [1, 2, 0] }}]\n}}\n"
    )
}

const RP_MANIFEST: &str = r#"{"format_version":2,"header":{"name":"V","uuid":"a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d","version":[3,4,5]},"modules":[]}"#;

fn config(outputs: &str) -> String {
    format!(
        r#"{{
  "type": "minecraftBedrock", "name": "c3 static", "targetVersion": "1.21.100", "namespace": "demo",
  "beacon": {{
    "outputs": {outputs},
    "targets": {{
      "dev": {{ "edition": "bedrock", "artifact": "dev" }},
      "addon": {{ "edition": "bedrock", "artifact": "mcaddon" }},
      "world": {{ "edition": "bedrock", "artifact": "mcworld", "world": "worlds/main" }}
    }}
  }}
}}"#
    )
}

/// Output order: vendor first, then game (not alphabetical).
const OUTPUTS: &str = r#"[
  { "id": "vendor", "static": { "bp": "./vendor/BP", "rp": "./vendor/RP" } },
  { "id": "game", "include": ["src/game/**"] }
]"#;

fn project(outputs: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "config.json", &config(outputs));
    write(
        r,
        "src/game/items/sword.item.json",
        r#"{"format_version":"1.21.100","minecraft:item":{"description":{"identifier":"demo:sword"},"components":{}}}"#,
    );
    write(r, "vendor/BP/manifest.json", &bp_manifest(BP_UUID));
    write(r, "vendor/BP/functions/a/b.mcfunction", "say  hi  \r\n");
    write(
        r,
        "vendor/BP/entities/x.json",
        "{ // keep\n \"b\":1,\"a\":2 }",
    );
    write(r, "vendor/RP/manifest.json", RP_MANIFEST);
    write(r, "vendor/RP/texts/en_US.lang", "a=b\n");
    std::fs::write(r.join("vendor/RP/blob.bin"), [0u8, 255, 13, 10, 7]).unwrap();
    std::fs::create_dir_all(r.join("worlds/main/db")).unwrap();
    std::fs::write(r.join("worlds/main/level.dat"), fixture_bytes()).unwrap();
    d
}

fn build(root: &Path, target: &str) -> beacon_engine::build::BuildOutcome {
    let runs = Arc::new(AtomicUsize::new(0));
    session(opts(root, Some(target)), root, &runs).build()
}

fn zip_entries(path: &str) -> BTreeMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().unwrap().to_string(), b)
        })
        .collect()
}

fn json(b: &[u8]) -> Value {
    // the vendor BP manifest has a BOM and the two comments of `bp_manifest`
    let s = String::from_utf8_lossy(b)
        .replace("// vendor", "")
        .replace("/* v */", "");
    serde_json::from_str(s.trim_start_matches('\u{feff}')).unwrap()
}

#[test]
fn static_files_pass_through_byte_identical() {
    let p = project(OUTPUTS);
    let r = build(p.path(), "addon");
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(!has(&r, Code::ManifestDrift));
    let z = zip_entries(&r.report.artifacts[0]);
    for (zip, src) in [
        (
            "c3 static vendor-BP/manifest.json",
            "vendor/BP/manifest.json",
        ),
        (
            "c3 static vendor-BP/functions/a/b.mcfunction",
            "vendor/BP/functions/a/b.mcfunction",
        ),
        (
            "c3 static vendor-BP/entities/x.json",
            "vendor/BP/entities/x.json",
        ),
        (
            "c3 static vendor-RP/manifest.json",
            "vendor/RP/manifest.json",
        ),
        ("c3 static vendor-RP/blob.bin", "vendor/RP/blob.bin"),
        (
            "c3 static vendor-RP/texts/en_US.lang",
            "vendor/RP/texts/en_US.lang",
        ),
    ] {
        assert_eq!(z[zip], std::fs::read(p.path().join(src)).unwrap(), "{zip}");
    }
    // the BOM is still there; nothing was generated into a static pack
    assert!(z["c3 static vendor-BP/manifest.json"].starts_with(b"\xEF\xBB\xBF"));
    assert!(!z.contains_key("c3 static vendor-RP/texts/languages.json"));
    assert!(z.contains_key("c3 static game-BP/manifest.json"));
}

#[test]
fn dev_deploy_has_static_packs_as_own_folders() {
    let p = project(OUTPUTS);
    let r = build(p.path(), "dev");
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let dir = |kind: &str| {
        std::fs::read_dir(p.path().join("out").join(kind))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.to_string_lossy().contains("-vendor-") || {
                    // the projected (first with BP) output has no output id in its folder name
                    json(&std::fs::read(p.join("manifest.json")).unwrap())["header"]["uuid"]
                        == BP_UUID
                }
            })
            .unwrap()
    };
    let bp = dir("development_behavior_packs");
    assert_eq!(
        std::fs::read(bp.join("manifest.json")).unwrap(),
        std::fs::read(p.path().join("vendor/BP/manifest.json")).unwrap()
    );
    assert_eq!(
        std::fs::read(bp.join("functions/a/b.mcfunction")).unwrap(),
        b"say  hi  \r\n"
    );
}

#[test]
fn bom_jsonc_manifest_parses() {
    let p = project(OUTPUTS);
    let r = build(p.path(), "world");
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
}

#[test]
fn missing_or_invalid_static_pack_is_static_pack_invalid() {
    let p = project(OUTPUTS);
    std::fs::remove_dir_all(p.path().join("vendor/BP")).unwrap();
    write(
        p.path(),
        "vendor/RP/manifest.json",
        r#"{"header":{"uuid":"x"}}"#,
    );
    let r = build(p.path(), "addon");
    let bad: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::StaticPackInvalid)
        .collect();
    assert_eq!(bad.len(), 2, "{:?}", r.diagnostics);
    assert!(bad[0].message.contains("vendor/BP") || bad[1].message.contains("vendor/BP"));
    assert!(
        r.report.artifacts.is_empty(),
        "no artifact from a failed build"
    );
}

#[test]
fn duplicate_pack_uuid_writes_nothing() {
    let outs = r#"[
      { "id": "vendor", "static": { "bp": "./vendor/BP", "rp": "./vendor/RP" } },
      { "id": "copy", "static": { "bp": "./vendor/BP2" } },
      { "id": "game", "include": ["src/game/**"] }
    ]"#;
    let p = project(outs);
    std::fs::create_dir_all(p.path().join("vendor/BP2")).unwrap();
    write(p.path(), "vendor/BP2/manifest.json", &bp_manifest(BP_UUID));
    for target in ["addon", "dev", "world"] {
        let r = build(p.path(), target);
        assert!(
            has(&r, Code::DuplicatePackUuid),
            "{target}: {:?}",
            r.diagnostics
        );
        assert!(r.report.artifacts.is_empty());
        assert!(!p.path().join("out").exists(), "{target}: wrote something");
    }
}

#[test]
fn world_pack_lists_follow_outputs_order_and_manifests() {
    let p = project(OUTPUTS);
    let r = build(p.path(), "world");
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let z = zip_entries(&r.report.artifacts[0]);
    let manifest = |folder: &str| json(&z[&format!("behavior_packs/{folder}/manifest.json")]);
    let rmanifest = |folder: &str| json(&z[&format!("resource_packs/{folder}/manifest.json")]);
    let bp = json(&z["world_behavior_packs.json"]);
    let rp = json(&z["world_resource_packs.json"]);
    let entry = |m: &Value| (m["header"]["uuid"].clone(), m["header"]["version"].clone());
    let want = |e: &Value| (e["pack_id"].clone(), e["version"].clone());
    let v = manifest("c3 static vendor-BP");
    assert_eq!(v["header"]["uuid"], BP_UUID);
    assert_eq!(bp.as_array().unwrap().len(), 2);
    assert_eq!(want(&bp[0]), entry(&v)); // vendor first: outputs-array order
    assert_eq!(want(&bp[1]), entry(&manifest("c3 static game-BP")));
    assert_eq!(bp[0]["version"], serde_json::json!([1, 2, 0]));
    // game has no RP content: only the vendor RP, at its own version
    assert_eq!(rp.as_array().unwrap().len(), 1);
    assert_eq!(want(&rp[0]), entry(&rmanifest("c3 static vendor-RP")));
    assert_eq!(rp[0]["version"], serde_json::json!([3, 4, 5]));
}
