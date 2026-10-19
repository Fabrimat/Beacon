//! `beacon api` (phase 4): the schema-1 summary, status, lookup and the install consent gate.
//! Offline: the index is seeded into `BEACON_HOME`; install is only run unconfirmed.

use beacon_engine::api_index::{API_INDEX_SCHEMA, ApiIndex, ApiItem, ApiKind, ScriptModule};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn beacon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(args)
        .env("BEACON_HOME", home)
        .env_remove("BEACON_DOWNLOAD")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn seed(home: &Path) {
    let ix = ApiIndex {
        schema: API_INDEX_SCHEMA,
        game_version: "1.26.50".into(),
        sources: vec![],
        script_modules: [(
            "@minecraft/server".to_owned(),
            vec![ScriptModule {
                version: "2.10.0".into(),
                beta: false,
                exports: [(
                    "World".to_owned(),
                    ApiItem {
                        kind: ApiKind::Class,
                        members: [(
                            "getDimension".to_owned(),
                            "getDimension(id: string): Dimension".to_owned(),
                        )]
                        .into(),
                        deprecated: false,
                        doc: None,
                    },
                )]
                .into(),
            }],
        )]
        .into(),
        vanilla: [(
            "entity".to_owned(),
            ["minecraft:zombie".to_owned(), "minecraft:cow".to_owned()].into(),
        )]
        .into(),
        components: Default::default(),
    };
    let dir = home.join("api/1.26.50");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.json"), serde_json::to_vec(&ix).unwrap()).unwrap();
}

#[test]
fn summary_json_reports_modules_and_counts() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["api", "--game", "1.26.50", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(
        (v["schema"].clone(), v["installed"].clone()),
        (1.into(), false.into())
    );
    assert_eq!(v["gameVersion"], "1.26.50");

    seed(h.path());
    let v = json(&beacon(h.path(), &["api", "--game", "1.26.50", "--json"]));
    assert_eq!(v["installed"], true);
    assert_eq!(v["modules"]["@minecraft/server"][0], "2.10.0");
    assert_eq!(v["vanilla"]["entity"], 2);
}

#[test]
fn status_and_lookup() {
    let h = tempfile::tempdir().unwrap();
    let v = json(&beacon(h.path(), &["api", "status", "--json"]));
    assert_eq!(v["versions"][0]["installed"], false);
    seed(h.path());
    let v = json(&beacon(h.path(), &["api", "status", "--json"]));
    assert_eq!(v["versions"][0]["installed"], true);
    let v = json(&beacon(
        h.path(),
        &[
            "api",
            "lookup",
            "World.getDimension",
            "--game",
            "1.26.50",
            "--json",
        ],
    ));
    assert_eq!(
        v["hits"][0]["path"],
        "@minecraft/server@2.10.0#World.getDimension"
    );
    // a missing index is a diagnostic, not a crash
    let o = beacon(
        h.path(),
        &["api", "lookup", "x", "--game", "9.9.9", "--json"],
    );
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn install_without_consent_is_a_usage_error() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["api", "install", "--game", "1.26.50"]);
    assert_eq!(
        o.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(String::from_utf8_lossy(&o.stderr).contains("BCN0"));
}
