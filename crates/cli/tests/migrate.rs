//! `beacon migrate` (5.6, phase 3 B1) on the committed fixtures: round trips against the real
//! legacy output (`--oracle`), dry run, determinism, `--out` validation, trust, to-port list,
//! first release after migration.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/migrate")
}

/// `BEACON_ESBUILD`, else the test install in `target/esbuild-test/` (see cli.rs); `None` =
/// skip the parts that bundle scripts.
fn esbuild() -> Option<PathBuf> {
    let env = std::env::var_os("BEACON_ESBUILD").map(PathBuf::from);
    let (dir, exe) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => ("win32-x64", "esbuild.exe"),
        ("windows", "aarch64") => ("win32-arm64", "esbuild.exe"),
        ("macos", "aarch64") => ("darwin-arm64", "bin/esbuild"),
        ("macos", "x86_64") => ("darwin-x64", "bin/esbuild"),
        ("linux", "x86_64") => ("linux-x64", "bin/esbuild"),
        _ => ("", ""),
    };
    let local = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/esbuild-test/node_modules/@esbuild")
        .join(dir)
        .join(exe);
    env.into_iter().chain([local]).find(|p| p.is_file())
}

fn beacon(home: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"));
    c.args(args)
        .env("BEACON_HOME", home)
        .env(
            "BEACON_ESBUILD",
            esbuild().unwrap_or("no-esbuild-here".into()),
        )
        .stdin(Stdio::null());
    c.output().unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}\n{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    })
}

fn mmc(t: &Path, out: &Path, extra: &[&str]) -> Output {
    let dir = fixtures().join("modular_mc");
    let mut a = vec![
        "migrate",
        s(&dir),
        "--out",
        s(out),
        "--trust",
        "--json",
        "--env",
        "FIXTURE_EDITION=bedrock",
    ];
    a.extend_from_slice(extra);
    beacon(&t.join("home"), &a)
}

fn st(t: &Path, out: &Path, extra: &[&str]) -> Output {
    let dir = fixtures().join("system_template");
    let ev = fixtures().join("system_template.expected");
    let mut a = vec![
        "migrate",
        s(&dir),
        "--out",
        s(out),
        "--trust",
        "--json",
        "--evaluated",
        s(&ev),
    ];
    a.extend_from_slice(extra);
    beacon(&t.join("home"), &a)
}

fn codes(o: &Output) -> Vec<String> {
    let v = json(o);
    v["runs"][0]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["ruleId"].as_str().map(String::from))
        .collect()
}

fn to_port(plan: &Value) -> Vec<(String, String)> {
    plan["report"]["toPort"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["kind"].as_str().unwrap().to_string(),
                t["path"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[test]
fn modular_mc_round_trip_matches_the_real_output() {
    if esbuild().is_none() {
        eprintln!("SKIP: no esbuild (set BEACON_ESBUILD)");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    let exp = fixtures().join("modular_mc.expected");
    let o = mmc(t.path(), &out, &["--oracle", s(&exp)]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let plan = json(&o);
    let rep = &plan["report"]["oracle"];
    assert_eq!(rep["diffs"], serde_json::json!([]), "{rep}");
    // every expected file (manifests without identities), the bundle by module order
    assert_eq!(rep["compared"], 16, "{rep}");
    assert!(out.join("src/_scripts/scripts/main.ts").is_file());
    assert!(!out.join(".beacon/migrate-oracle").exists());
    assert!(
        std::fs::read_to_string(out.join("MIGRATION.md"))
            .unwrap()
            .contains("16 files compared")
    );
    // a plain build of the migrated project needs no other tool
    let o = beacon(
        &t.path().join("home"),
        &["build", "--project", s(&out), "--trust"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(out.join("packs/BP/scripts/main.js").is_file());
}

#[test]
fn system_template_round_trip_matches_the_real_output() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    let exp = fixtures().join("system_template.expected");
    let o = st(t.path(), &out, &["--oracle", s(&exp)]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let rep = &json(&o)["report"]["oracle"];
    assert_eq!(rep["diffs"], serde_json::json!([]), "{rep}");
    assert_eq!(rep["compared"], 17, "{rep}");
}

#[test]
fn to_port_lists_every_item() {
    let t = tempfile::tempdir().unwrap();
    let o = mmc(t.path(), &t.path().join("a"), &["--dry-run"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let d = "packs/data/modular_mc";
    assert_eq!(
        to_port(&json(&o)),
        [
            ("generatorCandidate".into(), format!("{d}/a_core/_map.ts")),
            ("envBranch".into(), format!("{d}/b_extra/_map.ts")),
        ]
    );
    let o = st(t.path(), &t.path().join("b"), &["--dry-run"]);
    assert_eq!(
        to_port(&json(&o)),
        [
            (
                "generatorCandidate".to_string(),
                "data/system_template/golem/_map.py".to_string()
            ),
            (
                "subfunctionSource".to_string(),
                "data/system_template/golem/gated.mcfunction".to_string()
            )
        ]
    );
}

fn listing(root: &Path) -> Vec<String> {
    let mut v = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                todo.push(e.path());
            }
            v.push(e.path().display().to_string());
        }
    }
    v.sort();
    v
}

#[test]
fn dry_run_writes_nothing_and_the_plan_is_deterministic() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(t.path().join("home")).unwrap();
    let before = listing(t.path());
    let a = mmc(t.path(), &out, &["--dry-run"]);
    let b = mmc(t.path(), &out, &["--dry-run"]);
    assert_eq!(
        a.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&a.stderr)
    );
    assert_eq!(listing(t.path()), before);
    assert!(!a.stdout.is_empty());
    assert_eq!(a.stdout, b.stdout);
    let c = st(t.path(), &out, &["--dry-run"]);
    let d = st(t.path(), &out, &["--dry-run"]);
    assert_eq!(c.stdout, d.stdout);
    assert_eq!(listing(t.path()), before);
}

#[test]
fn bad_out_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let fx = fixtures().join("modular_mc");
    for out in [fx.clone(), fx.join("packs/new_out")] {
        let o = mmc(t.path(), &out, &[]);
        assert_eq!(o.status.code(), Some(1));
        assert_eq!(codes(&o), ["BCN0105"], "{out:?}");
    }
    assert!(!fx.join("packs/new_out").exists());
    // the oracle may not live in the comparison build dir (it is wiped first)
    let m = t.path().join("o");
    let inner = m.join(".beacon/migrate-oracle/x");
    let o = mmc(t.path(), &m, &["--oracle", s(&inner)]);
    assert_eq!(codes(&o), ["BCN0105"]);
    assert!(!m.exists());
    // non-empty without --force
    let full = t.path().join("full");
    std::fs::create_dir_all(&full).unwrap();
    std::fs::write(full.join("keep.txt"), "x").unwrap();
    let o = st(t.path(), &full, &[]);
    assert_eq!(codes(&o), ["BCN0105"]);
    assert_eq!(listing(&full).len(), 1);
    let o = st(t.path(), &full, &["--force"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(full.join("keep.txt").is_file() && full.join("config.json").is_file());
}

#[test]
fn untrusted_is_refused() {
    let t = tempfile::tempdir().unwrap();
    let o = beacon(
        &t.path().join("home"),
        &[
            "migrate",
            s(&fixtures().join("modular_mc")),
            "--out",
            s(&t.path().join("m")),
        ],
    );
    assert_eq!(o.status.code(), Some(3));
    assert!(!t.path().join("m").exists());
}

#[test]
fn first_pack_after_migration_keeps_the_imported_version() {
    let t = tempfile::tempdir().unwrap();
    let out = t.path().join("m");
    let o = st(t.path(), &out, &[]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let o = beacon(
        &t.path().join("home"),
        &["pack", "--project", s(&out), "--trust", "--json"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let r = json(&o);
    for p in r["packs"].as_array().unwrap() {
        assert_eq!(p["version"], serde_json::json!([1, 0, 0]), "{p}");
        assert_eq!(p["bumped"], false, "{p}");
    }
    assert_eq!(r["packs"].as_array().unwrap().len(), 2);
}

#[test]
fn oracle_profile_selects_the_compared_target() {
    let t = tempfile::tempdir().unwrap();
    let exp = fixtures().join("system_template.expected");
    let o = st(
        t.path(),
        &t.path().join("a"),
        &["--oracle", s(&exp), "--oracle-profile", "default"],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(json(&o)["report"]["oracle"]["diffs"], serde_json::json!([]));
    let o = st(
        t.path(),
        &t.path().join("b"),
        &["--oracle", s(&exp), "--oracle-profile", "nope"],
    );
    assert_eq!(o.status.code(), Some(1));
    let plan = json(&o);
    assert!(plan["changes"].as_array().unwrap().iter().any(|c| {
        c["note"]["code"] == "BCN0104"
            && c["note"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("\"nope\""))
    }));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (f, t) = (e.path(), to.join(e.file_name()));
        if f.is_dir() {
            copy_dir(&f, &t);
        } else {
            std::fs::copy(&f, &t).unwrap();
        }
    }
}

/// The archive branch of the oracle build: a profile mapped to an `.mcaddon` (one archive,
/// pack folders) and to `.mcpack`s (one archive per pack) compares the packs it holds.
#[test]
fn oracle_profile_builds_archive_targets() {
    let t = tempfile::tempdir().unwrap();
    let proj = t.path().join("st");
    copy_dir(&fixtures().join("system_template"), &proj);
    let cfg = std::fs::read_to_string(proj.join("config.json")).unwrap();
    let mut v: Value = serde_json::from_str(&cfg).unwrap();
    for (name, out) in [("addon", "x.mcaddon"), ("packs", "x.mcpack")] {
        v["regolith"]["profiles"][name] = serde_json::json!({
            "export": {"target": "local"},
            "filters": [{"profile": "default"},
                        {"filter": "pack_anything", "settings": {"output": out}}]
        });
    }
    std::fs::write(proj.join("config.json"), v.to_string()).unwrap();
    let exp = fixtures().join("system_template.expected");
    for (i, prof) in ["addon", "packs"].into_iter().enumerate() {
        let out = t.path().join(format!("m{i}"));
        let o = beacon(
            &t.path().join("home"),
            &[
                "migrate",
                s(&proj),
                "--out",
                s(&out),
                "--trust",
                "--json",
                "--evaluated",
                s(&exp),
                "--oracle",
                s(&exp),
                "--oracle-profile",
                prof,
            ],
        );
        assert_eq!(
            o.status.code(),
            Some(0),
            "{prof}: {}",
            String::from_utf8_lossy(&o.stdout)
        );
        let plan = json(&o);
        let rep = &plan["report"]["oracle"];
        assert_eq!(rep["diffs"], serde_json::json!([]), "{prof}: {rep}");
        assert_eq!(rep["compared"], 17, "{prof}: {rep}");
        let note = plan["changes"].as_array().unwrap().iter().any(|c| {
            c["note"]["message"]
                .as_str()
                .is_some_and(|m| m.contains(&format!("target `{prof}`")))
        });
        assert!(note, "{prof}");
    }
}
