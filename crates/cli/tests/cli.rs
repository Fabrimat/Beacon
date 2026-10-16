use std::path::Path;
use std::process::{Command, Output, Stdio};

fn beacon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(args)
        .env("BEACON_HOME", home)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn project(with_gen: bool) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/a.json"), "{}").unwrap();
    if with_gen {
        std::fs::write(d.path().join("src/m.gen.ts"), "export default 1").unwrap();
    }
    d
}

fn build(home: &Path, p: &Path, extra: &[&str]) -> Output {
    let out = p.join("out");
    let mut a = vec![
        "build",
        "--project",
        p.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ];
    a.extend(extra);
    beacon(home, &a)
}

#[test]
fn help_json_lists_every_command() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["--help", "--json"]);
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["schema"], 1);
    let names: Vec<_> = v["command"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["build", "watch", "check", "explain", "trust"]);
    let build = &v["command"]["commands"][0];
    assert!(
        build["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["long"] == "frozen")
    );
}

#[test]
fn explain() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["explain", "BCN0011"]);
    assert_eq!(code(&o), 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("same destination"));
    assert_eq!(code(&beacon(h.path(), &["explain", "BCN9999"])), 2);
}

#[test]
fn trust_gate() {
    let h = tempfile::tempdir().unwrap();
    let p = project(true);
    assert_eq!(code(&build(h.path(), p.path(), &[])), 3);
    assert_eq!(code(&build(h.path(), p.path(), &["--trust"])), 0);
    assert!(p.path().join("out/a.json").is_file());
    // --trust does not persist.
    assert_eq!(code(&build(h.path(), p.path(), &[])), 3);
    // Without a TTY, `trust` needs --yes.
    let ps = p.path().to_str().unwrap();
    assert_eq!(code(&beacon(h.path(), &["trust", "--project", ps])), 2);
    assert_eq!(
        code(&beacon(h.path(), &["trust", "--yes", "--project", ps])),
        0
    );
    assert_eq!(code(&build(h.path(), p.path(), &[])), 0);
}

#[test]
fn no_code_needs_no_trust_and_check_notes() {
    let h = tempfile::tempdir().unwrap();
    let p = project(false);
    let o = build(h.path(), p.path(), &["--json"]);
    assert_eq!(code(&o), 0);
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(r["emitted"], 1);
    let g = project(true);
    let o = beacon(
        h.path(),
        &["check", "--json", "--project", g.path().to_str().unwrap()],
    );
    assert_eq!(code(&o), 0);
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(s["runs"][0]["results"][0]["ruleId"], "BCN0050");
    assert_eq!(s["runs"][0]["results"][0]["level"], "note");
}

/// A junction (Windows) or symlink (unix) pointing outside the project, and one forming a
/// cycle, must be neither followed nor copied.
#[test]
fn links_are_not_followed() {
    let h = tempfile::tempdir().unwrap();
    let p = project(false);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.gen.ts"), "1").unwrap();
    let (out_link, loop_link) = (p.path().join("src/out"), p.path().join("src/loop"));
    #[cfg(windows)]
    let ok = [
        (&out_link, outside.path()),
        (&loop_link, &p.path().join("src")),
    ]
    .iter()
    .all(|(l, t)| {
        use std::os::windows::process::CommandExt;
        Command::new("cmd")
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                l.display(),
                t.display()
            ))
            .output()
            .unwrap()
            .status
            .success()
    });
    #[cfg(unix)]
    let ok = std::os::unix::fs::symlink(outside.path(), &out_link).is_ok()
        && std::os::unix::fs::symlink(p.path().join("src"), &loop_link).is_ok();
    assert!(ok, "could not create links");
    // No gen file reachable without following links: no trust needed, no stack overflow.
    let o = build(h.path(), p.path(), &[]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!p.path().join("out/out").exists() && !p.path().join("out/loop").exists());
}
