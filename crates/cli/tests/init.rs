//! `beacon init`, generated AGENTS.md and `beacon fix` with the real binary (9.0, 8.3, 6.3).

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
    Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(args)
        .current_dir(cwd)
        .env("BEACON_HOME", home)
        .env_remove("BEACON_COM_MOJANG")
        .env(
            "BEACON_ESBUILD",
            esbuild().unwrap_or_else(|| "no-esbuild-here".into()),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn files(root: &Path) -> Vec<String> {
    fn walk(d: &Path, root: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                let rel = p.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut v = Vec::new();
    walk(root, root, &mut v);
    v.sort();
    v
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

const EXPECTED: [&str; 10] = [
    ".gitignore",
    "AGENTS.md",
    "CLAUDE.md",
    "beacon.lock.json",
    "config.json",
    "pack_icon.png",
    "package.json",
    "src/main/functions/hello.mcfunction",
    "src/main/scripts/main.ts",
    "src/main/texts/en_US.lang",
];

#[test]
fn init_yes_creates_exact_files_trusted_and_builds() {
    let (home, _ws, dir) = project();
    assert_eq!(files(&dir), EXPECTED);
    let gi = fs::read_to_string(dir.join(".gitignore")).unwrap();
    for p in ["/.beacon/", "/dist/", "/node_modules/"] {
        assert!(gi.contains(p), "{gi}");
    }
    let claude = fs::read_to_string(dir.join("CLAUDE.md")).unwrap();
    assert_eq!(claude, "@AGENTS.md\n");
    let trust = fs::read_to_string(home.path().join("trust.json")).unwrap();
    assert!(trust.contains("demo"), "{trust}");
    // trusted without --trust: never exit 3
    let c = beacon(home.path(), &dir, &["check"]);
    assert_ne!(code(&c), 3, "{}", err(&c));
    if esbuild().is_some() {
        assert_eq!(code(&c), 0, "{}", err(&c));
        let b = beacon(
            home.path(),
            &dir,
            &["build", "--trust", "--target", "addon"],
        );
        assert_eq!(code(&b), 0, "{}", err(&b));
        assert!(dir.join("dist").is_dir());
    }
}

#[test]
fn nonempty_dir_refused_nothing_written() {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(ws.path().join("keep.txt"), "x").unwrap();
    let o = beacon(home.path(), ws.path(), &["init", "--yes", "."]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(files(ws.path()), ["keep.txt"]);
}

#[test]
fn from_json_works_and_flags_override() {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let j = ws.path().join("init.json");
    fs::write(
        &j,
        r#"{"name":"From Json","namespace":"fj","authors":["Ann"],"editions":["bedrock","education"],"artifacts":["mcpack"],"agentFiles":false}"#,
    )
    .unwrap();
    let dir = ws.path().join("p");
    let o = beacon(
        home.path(),
        ws.path(),
        &[
            "init",
            "--from",
            j.to_str().unwrap(),
            "--namespace",
            "flag",
            "p",
        ],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    let cfg = fs::read(dir.join("config.json")).unwrap();
    let cfg: serde_json::Value = serde_json::from_slice(&cfg).unwrap();
    assert_eq!(cfg["namespace"], "flag");
    assert_eq!(cfg["name"], "From Json");
    assert_eq!(cfg["authors"][0], "Ann");
    let t = cfg["beacon"]["targets"].as_object().unwrap();
    assert_eq!(t.keys().collect::<Vec<_>>(), ["pack", "pack-edu"]);
    assert!(!dir.join("AGENTS.md").exists());
    let bad = ws.path().join("bad.json");
    fs::write(&bad, r#"{"nope":1}"#).unwrap();
    let o = beacon(
        home.path(),
        ws.path(),
        &["init", "--from", bad.to_str().unwrap(), "q"],
    );
    assert_eq!(code(&o), 2);
    assert!(!ws.path().join("q").exists());
}

#[test]
fn invalid_namespace_and_template_are_usage_errors() {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for extra in [
        ["--namespace", "minecraft"],
        ["--namespace", "Bad-NS"],
        ["--template", "https://x/y.git"],
    ] {
        let mut a = vec!["init", "--yes"];
        a.extend(extra);
        a.push("p");
        let o = beacon(home.path(), ws.path(), &a);
        assert_eq!(code(&o), 2, "{extra:?}");
        assert!(err(&o).contains("BCN0002"), "{}", err(&o));
        assert!(!ws.path().join("p").exists());
    }
}

#[test]
fn describe_json_parses() {
    let (home, ws) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let o = beacon(home.path(), ws.path(), &["init", "--describe", "--json"]);
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["schema"], 1);
    let q = v["questions"].as_array().unwrap();
    let f: Vec<_> = q.iter().map(|x| x["field"].as_str().unwrap()).collect();
    assert_eq!(
        f,
        [
            "folder",
            "name",
            "namespace",
            "authors",
            "targetVersion",
            "editions",
            "artifacts",
            "template",
            "gitInit",
            "agentFiles"
        ]
    );
    assert_eq!(q[4]["default"], "1.26.50");
}

fn region(text: &str) -> &str {
    let a = text.find("<!-- beacon:generated:start -->").unwrap();
    let e = "<!-- beacon:generated:end -->";
    &text[a..text.find(e).unwrap() + e.len()]
}

#[test]
fn agents_region_is_deterministic() {
    let (_h, _w, a) = project();
    let (_h2, _w2, b) = project();
    let x = fs::read_to_string(a.join("AGENTS.md")).unwrap();
    let y = fs::read_to_string(b.join("AGENTS.md")).unwrap();
    assert_eq!(x, y);
    assert!(region(&x).contains("`addon` (bedrock mcaddon)") && region(&x).contains("`demo`"));
}

#[test]
fn hand_edit_is_stale_and_fix_restores_outside_untouched() {
    let (home, _ws, dir) = project();
    let md = dir.join("AGENTS.md");
    let orig = fs::read_to_string(&md).unwrap();
    // text outside the markers, with CRLF and odd characters, must survive byte for byte
    let (pre, post) = (
        "my notes \u{e9}\r\n\r\n",
        "\r\n\ttail without newline \u{1f600}",
    );
    let edited = format!(
        "{pre}{}{post}",
        region(&orig).replace("Namespace", "NAMESPACE")
    );
    fs::write(&md, &edited).unwrap();
    let c = beacon(home.path(), &dir, &["check"]);
    assert_eq!(code(&c), 1, "{}", err(&c));
    assert!(err(&c).contains("BCN0090"), "{}", err(&c));
    let j = beacon(home.path(), &dir, &["check", "--json"]);
    let sarif = String::from_utf8_lossy(&j.stdout);
    assert!(
        sarif.contains("BCN0090") && sarif.contains("fixes"),
        "{sarif}"
    );
    let f = beacon(home.path(), &dir, &["fix"]);
    assert_eq!(code(&f), 0, "{}", err(&f));
    let fixed = fs::read_to_string(&md).unwrap();
    assert!(fixed.starts_with(pre) && fixed.ends_with(post));
    assert_eq!(region(&fixed).replace("\r\n", "\n"), region(&orig));
    assert!(!err(&beacon(home.path(), &dir, &["check"])).contains("BCN0090"));
    // idempotent
    assert_eq!(code(&beacon(home.path(), &dir, &["fix"])), 0);
    assert_eq!(fs::read_to_string(&md).unwrap(), fixed);
    // a file without markers, or no file at all: not an error
    fs::write(&md, "my own notes\n").unwrap();
    assert!(!err(&beacon(home.path(), &dir, &["check"])).contains("BCN0090"));
    fs::remove_file(&md).unwrap();
    assert!(!err(&beacon(home.path(), &dir, &["check"])).contains("BCN0090"));
}

#[test]
fn fix_refuses_symlinked_agents_md() {
    let (home, _ws, dir) = project();
    let real = dir.join("real.md");
    let orig = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    let stale = orig.replace("Namespace", "NAMESPACE");
    fs::write(&real, &stale).unwrap();
    fs::remove_file(dir.join("AGENTS.md")).unwrap();
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&real, dir.join("AGENTS.md"));
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(&real, dir.join("AGENTS.md"));
    if made.is_err() {
        eprintln!("skipped: cannot create symlinks here");
        return;
    }
    let f = beacon(home.path(), &dir, &["fix"]);
    assert_ne!(code(&f), 0);
    assert!(err(&f).contains("symlink"), "{}", err(&f));
    assert_eq!(fs::read_to_string(&real).unwrap(), stale);
}
