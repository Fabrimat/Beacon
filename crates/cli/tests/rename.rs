use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn esbuild() -> Option<PathBuf> {
    let p = std::env::var_os("BEACON_ESBUILD")
        .map(PathBuf::from)
        .or_else(|| {
            let d = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/esbuild-test/node_modules/@esbuild");
            [
                "win32-x64/esbuild.exe",
                "darwin-arm64/bin/esbuild",
                "linux-x64/bin/esbuild",
            ]
            .iter()
            .map(|s| d.join(s))
            .find(|p| p.is_file())
        });
    p.filter(|p| p.is_file())
}

fn beacon(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(args)
        .current_dir(cwd)
        .env("BEACON_HOME", home)
        .env_remove("BEACON_COM_MOJANG")
        .env(
            "BEACON_ESBUILD",
            esbuild().unwrap_or_else(|| "no-esbuild-here".into()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(c.stdin.take().unwrap());
    c.wait_with_output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// (home, workspace, project dir) with a fresh project made by `init --yes`.
fn project() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = ws.path().join("demo");
    let o = beacon(
        home.path(),
        ws.path(),
        &["init", "--yes", "--name", "demo", "demo"],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    (home, ws, dir)
}

fn set_outputs(dir: &Path, outs: &str) {
    let p = dir.join("config.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    v["beacon"]["outputs"] = serde_json::from_str(outs).unwrap();
    v["beacon"]["primaryOutput"] = "main".into();
    v["beacon"]["targets"]["addon"]["outputs"] = serde_json::json!(["main"]);
    fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

fn lock(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(dir.join("beacon.lock.json")).unwrap()).unwrap()
}

#[test]
fn rename_updates_config_modules_and_lock_keeping_identities() {
    if esbuild().is_none() {
        return;
    }
    let (home, _ws, dir) = project();
    set_outputs(
        &dir,
        r#"[{"id":"main","include":["src/main/**"]},{"id":"lib","include":["src/lib/**"],"dependsOn":["main"]}]"#,
    );
    fs::create_dir_all(dir.join("src/lib")).unwrap();
    fs::write(dir.join("src/lib/module.json"), "{ \"output\": \"main\" }").unwrap();
    let b = beacon(
        home.path(),
        &dir,
        &["build", "--trust", "--target", "addon"],
    );
    assert_eq!(code(&b), 0, "{}", err(&b));
    let before = lock(&dir);
    let o = beacon(home.path(), &dir, &["rename-output", "main", "core"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let out = String::from_utf8_lossy(&o.stdout);
    for f in [
        "config.json",
        "beacon.lock.json",
        "src/lib/module.json",
        "AGENTS.md",
    ] {
        assert!(out.contains(f), "{f} in {out}");
    }
    let cfg = fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(!cfg.contains("\"main\""), "{cfg}");
    let v: serde_json::Value = serde_json::from_str(&cfg).unwrap();
    assert_eq!(v["beacon"]["outputs"][0]["id"], "core");
    assert_eq!(v["beacon"]["outputs"][1]["dependsOn"][0], "core");
    assert_eq!(v["beacon"]["primaryOutput"], "core");
    assert_eq!(v["beacon"]["targets"]["addon"]["outputs"][0], "core");
    assert_eq!(v["name"], "demo");
    assert_eq!(
        fs::read_to_string(dir.join("src/lib/module.json")).unwrap(),
        "{ \"output\": \"core\" }"
    );
    let after = lock(&dir);
    assert_eq!(after["outputs"]["core"], before["outputs"]["main"]);
    assert!(after["outputs"].get("main").is_none());
    assert_eq!(after["outputs"]["lib"], before["outputs"]["lib"]);
    // the project still builds against the same lock (nothing re-minted)
    let c = beacon(home.path(), &dir, &["check"]);
    assert_eq!(code(&c), 0, "{}", err(&c));
    let args = ["build", "--trust", "--target", "addon", "--frozen"];
    let b = beacon(home.path(), &dir, &args);
    assert_eq!(code(&b), 0, "{}", err(&b));
    assert_eq!(lock(&dir), after);
}

#[test]
fn collision_unknown_and_bad_ids_write_nothing() {
    let (home, _ws, dir) = project();
    set_outputs(
        &dir,
        r#"[{"id":"main","include":["src/main/**"]},{"id":"lib","include":["src/lib/**"]}]"#,
    );
    let snap = |d: &Path| ["config.json", "beacon.lock.json"].map(|f| fs::read(d.join(f)).unwrap());
    let before = snap(&dir);
    for args in [
        ["rename-output", "main", "lib"],
        ["rename-output", "nope", "x"],
        ["rename-output", "main", "bad name"],
        ["rename-output", "main", "main"],
    ] {
        let o = beacon(home.path(), &dir, &args);
        assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
        assert_eq!(snap(&dir), before, "{args:?}");
    }
    // a lock that already holds the target id: error, nothing written
    let mut l = lock(&dir);
    l["outputs"]["other"] = l["outputs"]["main"].clone();
    fs::write(dir.join("beacon.lock.json"), l.to_string()).unwrap();
    let before = snap(&dir);
    let o = beacon(home.path(), &dir, &["rename-output", "main", "other"]);
    assert_ne!(code(&o), 0);
    assert_eq!(snap(&dir), before);
}
