//! `beacon rename` and `beacon symbols` on `fixtures/rename` (phase 4 exit criterion 1).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn beacon(home: &Path, dir: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"))
        .arg("--project")
        .arg(dir)
        .args(args)
        .env("BEACON_HOME", home)
        .env("BEACON_ESBUILD", "no-esbuild-here")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(c.stdin.take().unwrap());
    c.wait_with_output().unwrap()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Every file below `dir`: (relative path, bytes), sorted.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// (home, temp dir, project copy of `fixtures/rename`).
fn copy() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = ws.path().join("rename");
    for (rel, bytes) in tree(&fixtures().join("rename")) {
        let p = dir.join(&rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    (home, ws, dir)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn rename_car_to_truck_matches_expected_and_checks_clean() {
    let (home, _ws, dir) = copy();
    let o = beacon(home.path(), &dir, &["rename", "demo:car", "demo:truck"]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(text(&o.stdout).contains("renamed demo:car to demo:truck: 14 edits in 8 files"));
    let (got, want) = (tree(&dir), tree(&fixtures().join("rename.expected")));
    assert_eq!(
        got.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        want.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    for ((p, a), (_, b)) in got.iter().zip(&want) {
        assert!(a == b, "{p} differs:\n{}\n---\n{}", text(a), text(b));
    }
    // no unresolved reference afterwards, restricted or trusted (generators run)
    for args in [&["check", "--json"][..], &["check", "--trust", "--json"]] {
        let o = beacon(home.path(), &dir, args);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stdout));
        assert!(!text(&o.stdout).contains("BCN0140"), "{}", text(&o.stdout));
    }
    // and the table now knows the truck only
    let o = beacon(
        home.path(),
        &dir,
        &["--json", "symbols", "--find", "demo:car"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(
        v["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["kind"] != "entity"),
        "{v}"
    );
}

#[test]
fn dry_run_prints_edits_writes_nothing_and_json_is_deterministic() {
    let (home, _ws, dir) = copy();
    let before = tree(&dir);
    let args = ["--json", "rename", "demo:car", "demo:truck", "--dry-run"];
    let a = beacon(home.path(), &dir, &args);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    let b = beacon(home.path(), &dir, &args);
    assert_eq!(a.stdout, b.stdout);
    assert_eq!(tree(&dir), before);
    let v: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    assert_eq!(
        (&v["schema"], &v["dryRun"], &v["applied"]),
        (&1.into(), &true.into(), &false.into())
    );
    let edits = v["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 14);
    assert!(
        edits
            .iter()
            .all(|e| e["replacement"] == "demo:truck" || e["replacement"] == "truck")
    );
    // text form: one `path:line:col: old -> new` line per edit
    let o = beacon(
        home.path(),
        &dir,
        &["rename", "demo:car", "demo:truck", "--dry-run"],
    );
    let out = text(&o.stdout);
    assert!(
        out.contains("src/world/functions/spawn.mcfunction:2:8: demo:car -> demo:truck"),
        "{out}"
    );
    assert!(
        out.contains("src/vehicles/scripts/lib/ids.ts:6:35: car -> truck"),
        "{out}"
    );
    // the template's loop body is one source line: one edit per literal, not per iteration
    assert!(
        out.contains("src/world/functions/convoy.mcfunction:3:24: demo:car -> demo:truck"),
        "{out}"
    );
    assert!(out.contains("nothing written"), "{out}");
    assert_eq!(tree(&dir), before);

    // block + item renamed together; `--kind block` is ambiguous
    let o = beacon(
        home.path(),
        &dir,
        &["--json", "rename", "demo:lamp", "demo:light", "--dry-run"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["edits"].as_array().unwrap().len(), 5, "{v}");
    let o = beacon(
        home.path(),
        &dir,
        &["rename", "demo:lamp", "demo:light", "--kind", "block"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("BCN0143"), "{}", text(&o.stderr));
    assert_eq!(tree(&dir), before);
}

#[test]
fn blocked_rename_writes_nothing() {
    let (home, _ws, dir) = copy();
    let before = tree(&dir);
    for (from, to) in [
        ("demo:horn", "demo:trumpet"), // referenced from the static output
        ("demo:car", "demo:lamp"),     // already defined
        ("demo:car", "Demo:Truck"),    // invalid
        ("minecraft:pig", "demo:pig"),
    ] {
        let o = beacon(home.path(), &dir, &["--json", "rename", from, to]);
        assert_eq!(o.status.code(), Some(1), "{from} -> {to}");
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["applied"], false);
        assert_eq!(v["diagnostics"][0]["code"], "BCN0143", "{v}");
    }
    let o = beacon(home.path(), &dir, &["rename", "demo:horn", "demo:trumpet"]);
    assert!(
        text(&o.stderr).contains("vendor/BP/functions/vendor/horn.mcfunction"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(tree(&dir), before);
    // a bad --kind is a usage error
    let o = beacon(
        home.path(),
        &dir,
        &["rename", "demo:car", "demo:x", "--kind", "mob"],
    );
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn symbols_json_is_schema_1_and_deterministic() {
    let home = tempfile::tempdir().unwrap();
    let dir = fixtures().join("rename");
    let a = beacon(home.path(), &dir, &["--json", "symbols"]);
    assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
    assert_eq!(
        a.stdout,
        beacon(home.path(), &dir, &["--json", "symbols"]).stdout
    );
    let v: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    assert_eq!(v["schema"], 1);
    let car = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "entity" && s["name"] == "demo:car")
        .unwrap();
    assert_eq!(car["references"], 13);
    assert_eq!(
        car["definitions"][0],
        serde_json::json!({ "path": "src/vehicles/entities/car.entity.json", "line": 6, "column": 22 })
    );
    let o = beacon(
        home.path(),
        &dir,
        &["--json", "symbols", "--find", "lamp", "--kind", "block"],
    );
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let names: Vec<_> = v["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["name"])
        .collect();
    assert_eq!(names, ["demo:lamp"]);
    let o = beacon(home.path(), &dir, &["symbols", "--find", "demo:horn"]);
    assert!(
        text(&o.stdout).contains("src/world/items/horn.item.json:4"),
        "{}",
        text(&o.stdout)
    );
}
