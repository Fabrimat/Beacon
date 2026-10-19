//! `beacon pack` (4.2.1): a script-free project, so no esbuild is needed.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const CONFIG: &str = r#"{
  "type": "minecraftBedrock", "name": "pack test", "targetVersion": "1.21.100", "namespace": "demo",
  "beacon": { "targets": { "addon": { "edition": "bedrock", "artifact": "mcaddon" } } }
}"#;

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "config.json", CONFIG);
    write(d.path(), "src/m/items/sword.png", "PNG");
    write(
        d.path(),
        "src/m/texts/en_US.lang",
        "item.demo:sword=Sword\n",
    );
    write(d.path(), "src/m/functions/hi.mcfunction", "say hi\n");
    d
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn pack(p: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(["pack", "--target", "addon", "--out"])
        .arg(p.join("out"))
        .args(args)
        .arg("--project")
        .arg(p)
        .env("BEACON_HOME", p.join("home"))
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn report(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {:?}", o))
}

fn versions(r: &Value) -> Vec<(String, Value)> {
    r["packs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["pack"].as_str().unwrap().into(), p["version"].clone()))
        .collect()
}

#[test]
fn json_reports_packs_and_second_run_is_stable() {
    let p = project();
    let o = pack(p.path(), &["--json"]);
    assert_eq!(o.status.code(), Some(0), "{o:?}");
    let first = report(&o);
    let v = versions(&first);
    assert!(v.iter().any(|(k, _)| k == "main/BP") && v.iter().any(|(k, _)| k == "main/RP"));
    assert_eq!(first["packs"][0]["bumped"], false);
    let lock = std::fs::read(p.path().join("beacon.lock.json")).unwrap();
    let again = pack(p.path(), &["--json"]);
    assert_eq!(report(&again)["packs"], first["packs"]);
    assert_eq!(
        std::fs::read(p.path().join("beacon.lock.json")).unwrap(),
        lock
    );
    // a change bumps, `--bump minor` bumps unchanged packs
    write(p.path(), "src/m/functions/hi.mcfunction", "say bye\n");
    let r = report(&pack(p.path(), &["--json"]));
    assert!(
        versions(&r)
            .iter()
            .any(|(k, v)| k == "main/BP" && *v == serde_json::json!([1, 0, 1]))
    );
    let r = report(&pack(
        p.path(),
        &["--json", "--bump", "minor", "--pack", "main/RP"],
    ));
    assert!(
        versions(&r)
            .iter()
            .any(|(k, v)| k == "main/RP" && *v == serde_json::json!([1, 1, 0]))
    );
}

#[test]
fn manual_policy_stale_exits_1_and_bad_version_exits_2() {
    let p = project();
    assert_eq!(pack(p.path(), &[]).status.code(), Some(0));
    let c = CONFIG.replace("\"targets\"", "\"versionPolicy\": \"manual\", \"targets\"");
    write(p.path(), "config.json", &c);
    write(p.path(), "src/m/functions/hi.mcfunction", "say bye\n");
    let lock = std::fs::read(p.path().join("beacon.lock.json")).unwrap();
    let o = pack(p.path(), &[]);
    assert_eq!(o.status.code(), Some(1), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("BCN0073"));
    assert_eq!(
        std::fs::read(p.path().join("beacon.lock.json")).unwrap(),
        lock
    );
    assert_eq!(
        pack(p.path(), &["--set-version", "1.x"]).status.code(),
        Some(2)
    );
    assert_eq!(
        pack(p.path(), &["--set-version", "1.0.2"]).status.code(),
        Some(0)
    );
}

#[test]
fn unknown_pack_id_exits_2_and_lists_valid_ids() {
    let p = project();
    let o = pack(p.path(), &["--pack", "nope/BP"]);
    assert_eq!(o.status.code(), Some(2), "{o:?}");
    assert!(String::from_utf8_lossy(&o.stderr).contains("main/BP"));
    assert!(!p.path().join("out").exists());
    assert_eq!(
        pack(p.path(), &["--pack", "main/BP", "--pack", "main/RP"])
            .status
            .code(),
        Some(0)
    );
}
