//! `beacon hook pre-tool-use|stop` with the real binary (8.2).

use serde_json::{Value, json};
use std::fs;
use std::io::Write;
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

fn run(home: &Path, cwd: &Path, args: &[&str], stdin: &str) -> Output {
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
    c.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
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
    let o = run(
        home.path(),
        ws.path(),
        &["init", "--yes", "--name", "demo", "demo"],
        "",
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    (home, ws, dir)
}

fn hook(home: &Path, cwd: &Path, event: &str, input: &str) -> (Value, Output) {
    let o = run(home, cwd, &["hook", event], input);
    assert_eq!(code(&o), 0, "hooks never exit non-zero: {}", err(&o));
    let v = serde_json::from_slice(&o.stdout).expect("stdout is JSON");
    (v, o)
}

fn pre(home: &Path, cwd: &Path, tool: &str, tool_input: Value) -> Value {
    let ev = json!({ "hook_event_name": "PreToolUse", "tool_name": tool,
        "tool_input": tool_input, "cwd": cwd });
    hook(home, cwd, "pre-tool-use", &ev.to_string()).0
}

fn denied(v: &Value) -> bool {
    let h = &v["hookSpecificOutput"];
    h["permissionDecision"] == "deny" && !h["permissionDecisionReason"].as_str().unwrap().is_empty()
}

fn edit(path: &str, old: &str, new: &str) -> Value {
    json!({ "file_path": path, "old_string": old, "new_string": new })
}

#[test]
fn generated_files_are_denied_in_every_path_spelling() {
    let (home, ws, dir) = project();
    let d = dir.to_string_lossy().into_owned();
    let upper = d.to_uppercase();
    let other = ws.path().to_string_lossy().into_owned();
    let paths = [
        // lock
        format!("{d}/beacon.lock.json"),
        "beacon.lock.json".into(),
        "src/../beacon.lock.json".into(),
        format!("{d}/src/main/../../beacon.lock.json"),
        format!("{upper}/BEACON.LOCK.JSON"),
        "./Beacon.Lock.Json".into(),
        // manifests: packs/*, .beacon/out, dev pack folders (anywhere)
        format!("{d}/packs/BP/manifest.json"),
        "packs/RP/manifest.json".into(),
        "src/../packs/BP/../BP/manifest.json".into(),
        format!("{upper}/PACKS/bp/MANIFEST.JSON"),
        ".beacon/out/main/BP/manifest.json".into(),
        ".BEACON/Out/x/manifest.json".into(),
        format!("{other}/com.mojang/development_behavior_packs/demo/manifest.json"),
        format!("{other}/Com.Mojang/Development_Resource_Packs/demo/Manifest.json"),
    ];
    for tool in ["Edit", "Write", "MultiEdit", "NotebookEdit"] {
        for p in &paths {
            let input = match tool {
                "Write" => json!({ "file_path": p, "content": "{}" }),
                "MultiEdit" => {
                    json!({ "file_path": p, "edits": [{"old_string":"a","new_string":"b"}] })
                }
                "NotebookEdit" => json!({ "notebook_path": p, "new_source": "x" }),
                _ => edit(p, "a", "b"),
            };
            let v = pre(home.path(), &dir, tool, input);
            assert!(denied(&v), "{tool} {p}: {v}");
        }
    }
}

#[test]
fn agents_md_is_denied_only_inside_the_generated_region() {
    let (home, _ws, dir) = project();
    let text = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let inside = "`beacon fix`: apply safe fixes";
    assert!(text.contains(inside));
    let d = dir.to_string_lossy().into_owned();
    for p in [
        "AGENTS.md".to_string(),
        format!("{d}/AGENTS.md"),
        "src/../agents.md".into(),
        format!("{}/Agents.MD", d.to_uppercase()),
    ] {
        let t = |o: &str, n: &str| pre(home.path(), &dir, "Edit", edit(&p, o, n));
        assert!(denied(&t(inside, "x")), "{p}");
        assert!(denied(&t("beacon:generated:start -->\n## Beacon", "")));
        assert!(denied(&t(
            "# demo",
            "# demo\n<!-- beacon:generated:end -->"
        )));
        assert!(!denied(&t("# demo", "# My demo")), "{p}");
        let multi = |a: &str| {
            let i = json!({ "file_path": p, "edits": [
                {"old_string":"# demo","new_string":"# x"}, {"old_string":a,"new_string":"y"}] });
            pre(home.path(), &dir, "MultiEdit", i)
        };
        assert!(denied(&multi(inside)), "{p}");
        assert!(!denied(&multi("# x")), "{p}");
    }
    // Write: allowed when the region is byte-identical, denied when it changes
    let write = |c: &str| {
        let i = json!({ "file_path": "AGENTS.md", "content": c });
        pre(home.path(), &dir, "Write", i)
    };
    assert!(!denied(&write(&format!("{text}\nMy notes\n"))));
    assert!(denied(&write(&text.replace(inside, "mine"))));
    assert!(denied(&write("nothing")));
    let nb = pre(
        home.path(),
        &dir,
        "NotebookEdit",
        json!({ "notebook_path": "AGENTS.md" }),
    );
    assert!(denied(&nb));
    // CRLF files: the model's \n edits still hit the region
    fs::write(dir.join("AGENTS.md"), text.replace('\n', "\r\n")).unwrap();
    let e = |o: &str, n: &str| pre(home.path(), &dir, "Edit", edit("AGENTS.md", o, n));
    assert!(denied(&e(inside, "x")));
    assert!(!denied(&e("# demo", "# d")));
}

#[test]
fn normal_files_and_other_tools_are_allowed() {
    let (home, ws, dir) = project();
    let d = dir.to_string_lossy().into_owned();
    for (tool, p) in [
        ("Edit", "src/main/scripts/main.ts".to_string()),
        ("Write", format!("{d}/src/main/texts/en_US.lang")),
        ("Edit", "config.json".into()),
        ("Edit", "src/../src/main/functions/hello.mcfunction".into()),
        ("Edit", "notes/beacon.lock.json.md".into()),
        ("Edit", "vendor/BP/manifest.json".into()),
        (
            "Edit",
            format!("{}/elsewhere/beacon.lock.json", ws.path().display()),
        ),
        ("Edit", format!("{d}-sibling/packs/BP/manifest.json")),
    ] {
        let v = pre(home.path(), &dir, tool, edit(&p, "a", "b"));
        assert_eq!(
            v,
            json!({}),
            "{tool} {p}: no opinion, never an explicit allow"
        );
    }
    let bash = json!({ "command": "echo > beacon.lock.json" });
    assert_eq!(pre(home.path(), &dir, "Bash", bash), json!({}));
}

#[test]
fn malformed_input_fails_open_with_a_note() {
    let (home, _ws, dir) = project();
    let bad = [
        "",
        "not json",
        "{",
        "[]",
        r#"{"tool_name":"Edit"}"#,
        r#"{"tool_name":"Edit","cwd":".","tool_input":{}}"#,
    ];
    for b in bad {
        let (v, o) = hook(home.path(), &dir, "pre-tool-use", b);
        assert_eq!(v, json!({}), "{b:?}");
        assert!(!err(&o).is_empty(), "{b:?}: note on stderr");
        let (v, _) = hook(home.path(), &dir, "stop", b);
        assert_eq!(v, json!({}), "{b:?}");
    }
}

#[test]
fn stop_hook_blocks_on_check_errors_and_allows_otherwise() {
    let (home, ws, dir) = project();
    let ev = |cwd: &Path, active: bool| {
        json!({ "hook_event_name": "Stop", "cwd": cwd, "stop_hook_active": active }).to_string()
    };
    if esbuild().is_some() {
        let (v, _) = hook(home.path(), &dir, "stop", &ev(&dir, false));
        assert_eq!(v, json!({}), "clean project");
    }
    // a broken module.json is a check error
    fs::write(dir.join("src/main/module.json"), "{").unwrap();
    assert_eq!(code(&run(home.path(), &dir, &["check"], "")), 1);
    let (v, _) = hook(home.path(), &dir, "stop", &ev(&dir, false));
    assert_eq!(v["decision"], "block", "{v}");
    assert!(v["reason"].as_str().unwrap().contains("BCN"), "{v}");
    // from a subfolder of the project too
    let (v, _) = hook(home.path(), &dir, "stop", &ev(&dir.join("src/main"), false));
    assert_eq!(v["decision"], "block", "{v}");
    // loop prevention: already continuing because of a stop hook
    let (v, _) = hook(home.path(), &dir, "stop", &ev(&dir, true));
    assert_eq!(v, json!({}));
    // outside any project nothing runs
    let empty = ws.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let (v, _) = hook(home.path(), &empty, "stop", &ev(&empty, false));
    assert_eq!(v, json!({}));
}
