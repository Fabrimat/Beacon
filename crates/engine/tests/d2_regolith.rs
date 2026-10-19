//! Phase 3 A3: Regolith profiles -> Beacon targets (fixture configs, plus the two real
//! projects when they are on disk; never copied into the repo).

use beacon_engine::diag::Code;
use beacon_engine::migrate::RegolithConfig;
use beacon_engine::migrate::regolith::map_profiles;
use serde_json::{Value, json};
use std::path::Path;

fn load(p: &Path) -> Option<RegolithConfig> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn fixture(name: &str) -> RegolithConfig {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../fixtures/migrate/regolith_config/{name}.json"
    ));
    load(&p).unwrap()
}

fn keys(v: &Value) -> Vec<&str> {
    v.as_object().unwrap().keys().map(String::as_str).collect()
}

#[test]
fn default_profile_is_dev() {
    let (t, d) = map_profiles(&fixture("default"), None);
    assert_eq!(t, json!({"dev": {"edition": "bedrock", "artifact": "dev"}}));
    assert!(d.is_empty());
}

#[test]
fn packaging_fixture_uses_pack_anything_output() {
    let (t, d) = map_profiles(&fixture("packaging"), None);
    assert_eq!(keys(&t), ["dev", "package"]);
    assert_eq!(
        t["package"],
        json!({"edition": "bedrock", "artifact": "mcaddon", "outputs": ["*"]})
    );
    // strip_debug has no equivalent
    assert!(
        d.iter()
            .any(|d| d.code == Code::MigrateToPort && d.message.contains("strip_debug"))
    );
    assert!(!d.iter().any(|d| d.message.contains("modular_mc")));
}

#[test]
fn edu_world_fixture() {
    let (t, d) = map_profiles(&fixture("edu_world"), None);
    assert_eq!(keys(&t), ["dev", "edu"]);
    assert_eq!(
        t["edu"],
        json!({"edition": "education", "artifact": "mcworld", "worldName": "Sample Lesson"})
    );
    assert!(
        d.iter()
            .any(|d| d.code == Code::MigrateToPort && d.message.contains("targets.edu"))
    );
}

#[test]
fn the_great_adventure() {
    let root = Path::new("D:/Repos/The-Great-Adventure/regolith");
    let Some(cfg) = load(&root.join("config.json")) else {
        return;
    };
    let (t, d) = map_profiles(&cfg, Some(root));
    assert_eq!(
        keys(&t),
        ["dev", "edu", "edu-dev", "edu-template", "template", "world"]
    );
    assert_eq!(t["dev"], json!({"edition": "bedrock", "artifact": "dev"}));
    assert_eq!(
        t["edu-dev"],
        json!({"edition": "education", "artifact": "dev"})
    );
    let w = &t["world"];
    assert_eq!(w["artifact"], "mcworld");
    assert_eq!(w["world"], "worlds/The_Great_Adventure");
    assert_eq!(w["worldName"], "Dexi Ville");
    assert_eq!(w["targetVersion"], "1.20.30");
    assert_eq!(w["levelDat"]["commandsEnabled"], false);
    assert_eq!(w["levelDat"]["XBLBroadcastIntent"], 1);
    assert_eq!(w["levelDat"]["Difficulty"], 2);
    assert_eq!(w["levelDat"]["GameType"], 2);
    assert_eq!(t["template"]["artifact"], "mctemplate");
    let e = &t["edu"];
    assert_eq!(
        (e["edition"].as_str(), e["world"].as_str()),
        (Some("education"), Some("worlds/The_Great_Adventure_EDU"))
    );
    assert!(e.get("targetVersion").is_none());
    assert_eq!(t["edu-template"]["artifact"], "mctemplate");
    for f in ["debug_say_function_name", "print_lang_diff"] {
        assert!(
            d.iter()
                .any(|d| d.code == Code::MigrateDropped && d.message.contains(f)),
            "{f}"
        );
    }
    assert!(d.iter().any(|d| d.code == Code::MigrateToPort
        && d.message.contains("shapescape_custom_blocks_generator")));
}

#[test]
fn dimension_clash() {
    let root = Path::new("D:/Repos/Dimension-Clash/regolith");
    let Some(cfg) = load(&root.join("config.json")) else {
        return;
    };
    let (t, d) = map_profiles(&cfg, Some(root));
    assert_eq!(keys(&t), ["dev", "template", "world"]);
    assert_eq!(t["world"]["world"], "worlds/Dimension_Clash");
    assert_eq!(t["world"]["worldName"], "Dimension Clash");
    assert!(t["world"].get("levelDat").is_none());
    assert!(
        d.iter()
            .any(|d| d.code == Code::MigrateToPort && d.message.contains("minify_json"))
    );
}
