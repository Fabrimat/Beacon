use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

fn cmd(home: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"));
    c.args(args)
        .env("BEACON_HOME", home)
        .env_remove("BEACON_COM_MOJANG")
        .stdin(Stdio::null());
    if let Some(e) = esbuild() {
        c.env("BEACON_ESBUILD", e);
    } else {
        c.env("BEACON_ESBUILD", "no-esbuild-here");
    }
    c
}

fn beacon(home: &Path, args: &[&str]) -> Output {
    cmd(home, args).output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// esbuild installed once into `target/esbuild-test/` (same as crates/bundler/tests); `None`
/// (tests skip their script parts) without npm.
fn esbuild() -> Option<&'static Path> {
    static EXE: OnceLock<Option<PathBuf>> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/esbuild-test");
        let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("windows", "x86_64") => "win32-x64/esbuild.exe",
            ("windows", "aarch64") => "win32-arm64/esbuild.exe",
            ("macos", "aarch64") => "darwin-arm64/bin/esbuild",
            ("macos", "x86_64") => "darwin-x64/bin/esbuild",
            ("linux", "x86_64") => "linux-x64/bin/esbuild",
            ("linux", "aarch64") => "linux-arm64/bin/esbuild",
            _ => return None,
        };
        let exe = dir.join("node_modules/@esbuild").join(platform);
        if !exe.is_file() {
            std::fs::create_dir_all(&dir).ok()?;
            let mut c = if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.args(["/C", "npm"]);
                c
            } else {
                Command::new("npm")
            };
            let ok = c
                .args(["i", "--no-audit", "--no-fund", "--prefix"])
                .arg(&dir)
                .arg("esbuild@0.28.2")
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                eprintln!("SKIP (scripts): npm unavailable or esbuild install failed");
            }
        }
        exe.canonicalize().ok()
    })
    .as_deref()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

const CONFIG: &str =
    r#"{"type":"minecraftBedrock","name":"t","targetVersion":"1.21.100","namespace":"demo"}"#;

fn project(with_gen: bool) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "config.json", CONFIG);
    write(d.path(), "src/m/functions/a.mcfunction", "say a\n");
    if with_gen {
        write(
            d.path(),
            "src/m/g.gen.ts",
            "export default generate(() => [file(\"BP\", \"functions/m/gen.mcfunction\", { text: \"say gen\\n\" })]);\n",
        );
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
    assert_eq!(
        names,
        [
            "build",
            "watch",
            "check",
            "explain",
            "trust",
            "pack",
            "init",
            "fix",
            "mcp",
            "migrate",
            "add",
            "rename-output",
            "hook"
        ]
    );
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
    let gen_out = p.path().join("out/main/BP/functions/m/gen.mcfunction");
    let o = build(h.path(), p.path(), &[]);
    assert_eq!(code(&o), 3, "{}", stderr(&o));
    assert!(!gen_out.exists());
    let o = build(h.path(), p.path(), &["--trust"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(std::fs::read_to_string(&gen_out).unwrap(), "say gen\n");
    assert!(
        p.path()
            .join("out/main/BP/functions/m/a.mcfunction")
            .is_file()
    );
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
fn check_is_restricted_and_reports_sarif() {
    let h = tempfile::tempdir().unwrap();
    let p = project(true);
    write(p.path(), "src/m/notes.txt", "x");
    let o = beacon(
        h.path(),
        &["check", "--json", "--project", p.path().to_str().unwrap()],
    );
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let s: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let results = s["runs"][0]["results"].as_array().unwrap();
    let rule = |id: &str| results.iter().find(|r| r["ruleId"] == id).cloned();
    assert_eq!(rule("BCN0050").expect("untrusted note")["level"], "note");
    assert_eq!(rule("BCN0010").expect("routing ran")["level"], "warning");
    assert!(
        !p.path().join("beacon.lock.json").exists(),
        "check writes nothing"
    );
    // a collision is an error: exit 1
    write(p.path(), "src/n/functions/x.mcfunction", "say x\n");
    write(p.path(), "src/n/items/a.item.json", "{}");
    write(p.path(), "src/m/items/a.item.json", "{}");
    let o = beacon(
        h.path(),
        &["check", "--project", p.path().to_str().unwrap()],
    );
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("BCN0011"), "{}", stderr(&o));
}

/// Restricted mode never runs the esbuild sidecar (5.5), not even `esbuild --version` to
/// locate it: a project-shipped or env-given binary must not execute for an untrusted project.
#[test]
fn untrusted_never_runs_the_bundler() {
    let h = tempfile::tempdir().unwrap();
    let p = project(true);
    write(
        p.path(),
        "src/m/scripts/main.ts",
        "export {};
",
    );
    let tools = tempfile::tempdir().unwrap();
    let marker = tools.path().join("ran");
    let fake = if cfg!(windows) {
        let f = tools.path().join("esbuild.cmd");
        std::fs::write(
            &f,
            format!(
                "@echo off
echo x> \"{}\"
echo 0.0.0
",
                marker.display()
            ),
        )
        .unwrap();
        f
    } else {
        let f = tools.path().join("esbuild");
        std::fs::write(
            &f,
            format!(
                "#!/bin/sh
echo x > '{}'
echo 0.0.0
",
                marker.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f
    };
    let ps = p.path().to_str().unwrap();
    let out = p.path().join("out");
    let run = |args: &[&str]| {
        cmd(h.path(), args)
            .env("BEACON_ESBUILD", &fake)
            .output()
            .unwrap()
    };
    let o = run(&["build", "--project", ps, "--out", out.to_str().unwrap()]);
    assert_eq!(code(&o), 3, "{}", stderr(&o));
    let o = run(&["check", "--project", ps]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(!marker.exists(), "the bundler ran for an untrusted project");
    // even with --yes and no BEACON_ESBUILD an untrusted build never downloads
    let o = cmd(h.path(), &["build", "--yes", "--project", ps])
        .env_remove("BEACON_ESBUILD")
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(code(&o), 3, "{}", stderr(&o));
    assert!(
        !h.path().join("tools").exists(),
        "untrusted build touched the tools dir"
    );
    // control: trusted, it is located (and run)
    run(&[
        "build",
        "--trust",
        "--project",
        ps,
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        marker.exists(),
        "control: the fake esbuild should run when trusted"
    );
}

/// A junction (Windows) or symlink (unix) pointing outside the project, and one forming a
/// cycle, must be neither followed nor copied.
#[test]
fn links_are_not_followed() {
    let h = tempfile::tempdir().unwrap();
    let p = project(false);
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "x.gen.ts", "1");
    write(outside.path(), "secret.mcfunction", "say secret");
    let (out_link, loop_link) = (p.path().join("src/m/out"), p.path().join("src/m/loop"));
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
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).contains("BCN0030"), "{}", stderr(&o));
    let bp = p.path().join("out/main/BP/functions/m");
    assert!(bp.join("a.mcfunction").is_file());
    assert!(!bp.join("out").exists() && !bp.join("loop").exists());
}

/// Copies `fixtures/minimal` (sources only) into a temp dir.
fn minimal() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let name = e.file_name();
            if ["packs", "dist", ".beacon"].contains(&name.to_str().unwrap()) {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(&name));
            } else {
                std::fs::copy(e.path(), to.join(&name)).unwrap();
            }
        }
    }
    let d = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal"),
        d.path(),
    );
    d
}

fn zip_json(z: &mut zip::ZipArchive<std::fs::File>, name: &str) -> serde_json::Value {
    let f = z.by_name(name).unwrap_or_else(|_| panic!("{name} missing"));
    serde_json::from_reader(f).unwrap()
}

#[test]
fn minimal_fixture_end_to_end() {
    if esbuild().is_none() {
        eprintln!("SKIP: no esbuild; the fixture has scripts");
        return;
    }
    let h = tempfile::tempdir().unwrap();
    let p = minimal();
    let ps = p.path().to_str().unwrap();
    let lock = std::fs::read(p.path().join("beacon.lock.json")).unwrap();

    let o = beacon(
        h.path(),
        &[
            "--project",
            ps,
            "build",
            "--trust",
            "--target",
            "addon",
            "--json",
            "--frozen",
        ],
    );
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stdout));
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let artifact = r["artifacts"][0].as_str().unwrap();
    assert!(
        artifact.ends_with("beacon-minimal-addon.mcaddon"),
        "{artifact}"
    );
    assert_eq!(
        std::fs::read(p.path().join("beacon.lock.json")).unwrap(),
        lock,
        "committed lockfile is stable"
    );

    let mut z = zip::ZipArchive::new(std::fs::File::open(artifact).unwrap()).unwrap();
    let mut names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().unwrap().into_owned())
        .collect();
    names.sort();
    let want = [
        "beacon-minimal BP/entities/ghost.json",
        "beacon-minimal BP/functions/demo/hello.mcfunction",
        "beacon-minimal BP/items/ruby.json",
        "beacon-minimal BP/manifest.json",
        "beacon-minimal BP/pack_icon.png",
        "beacon-minimal BP/scripts/main.js",
        "beacon-minimal BP/texts/en_US.lang",
        "beacon-minimal BP/texts/languages.json",
        "beacon-minimal RP/entity/ghost.json",
        "beacon-minimal RP/manifest.json",
        "beacon-minimal RP/models/entity/ghost.geo.json",
        "beacon-minimal RP/pack_icon.png",
        "beacon-minimal RP/sounds/demo/beep.ogg",
        "beacon-minimal RP/sounds/sound_definitions.json",
        "beacon-minimal RP/texts/en_US.lang",
        "beacon-minimal RP/texts/languages.json",
        "beacon-minimal RP/textures/entity/ghost.png",
        "beacon-minimal RP/textures/item_texture.json",
        "beacon-minimal RP/textures/items/ruby.png",
    ];
    assert_eq!(names, want);

    let bp = zip_json(&mut z, "beacon-minimal BP/manifest.json");
    let rp = zip_json(&mut z, "beacon-minimal RP/manifest.json");
    let deps = bp["dependencies"].as_array().unwrap();
    assert!(
        deps.iter().any(|d| d["uuid"] == rp["header"]["uuid"]),
        "BP depends on RP"
    );
    assert!(deps.iter().any(|d| d["module_name"] == "@minecraft/server"));
    assert!(
        bp["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["type"] == "script" && m["entry"] == "scripts/main.js")
    );
    let sounds = zip_json(&mut z, "beacon-minimal RP/sounds/sound_definitions.json");
    assert_eq!(
        sounds["sound_definitions"]["minimal:beep"]["sounds"][0],
        "sounds/demo/beep"
    );
    assert_eq!(
        zip_json(&mut z, "beacon-minimal RP/texts/languages.json"),
        serde_json::json!(["en_US"])
    );
    let mut js = String::new();
    std::io::Read::read_to_string(
        &mut z.by_name("beacon-minimal BP/scripts/main.js").unwrap(),
        &mut js,
    )
    .unwrap();
    assert!(
        js.contains("worldLoad") && js.contains("\"@minecraft/server\""),
        "{js}"
    );

    // the packs build projects into packs/BP + packs/RP
    let o = beacon(h.path(), &["--project", ps, "build", "--trust"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    for f in [
        "packs/BP/manifest.json",
        "packs/BP/scripts/main.js.map",
        "packs/RP/textures/items/ruby.png",
    ] {
        assert!(p.path().join(f).is_file(), "{f}");
    }
    let o = beacon(h.path(), &["--project", ps, "check", "--trust"]);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
}

#[test]
fn watch_rebuilds_on_change() {
    let h = tempfile::tempdir().unwrap();
    let p = project(false);
    let out = p.path().join("out");
    let mut child = cmd(
        h.path(),
        &[
            "--json",
            "--project",
            p.path().to_str().unwrap(),
            "watch",
            "--out",
            out.to_str().unwrap(),
        ],
    )
    .stdout(Stdio::piped())
    .spawn()
    .unwrap();
    let (tx, rx) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for l in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    let next = || -> serde_json::Value {
        let l = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("watch event");
        serde_json::from_str(&l).unwrap()
    };
    let ready = next();
    assert_eq!(
        (ready["event"].as_str(), ready["generation"].as_u64()),
        (Some("ready"), Some(1))
    );
    let target = out.join("main/BP/functions/m/a.mcfunction");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "say a\n");
    write(p.path(), "src/m/functions/a.mcfunction", "say changed\n");
    let built = loop {
        let e = next();
        if e["event"] == "built" && std::fs::read_to_string(&target).unwrap() == "say changed\n" {
            break e;
        }
    };
    assert_eq!(built["errors"], 0);
    let _ = child.kill();
    let _ = child.wait();
}

/// `cargo corpus --size medium` builds with `--trust` and no error diagnostics (only
/// `BundlerMissing` without esbuild).
#[test]
fn corpus_medium_builds() {
    let h = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let corpus = dir.path().join("c");
    let st = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()))
        .args([
            "run",
            "-q",
            "-p",
            "beacon-corpus",
            "--",
            "--size",
            "medium",
            "--out",
        ])
        .arg(&corpus)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
    let o = beacon(
        h.path(),
        &[
            "--project",
            corpus.to_str().unwrap(),
            "build",
            "--trust",
            "--json",
        ],
    );
    let r: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let errors: Vec<_> = r["sarif"]["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["level"] == "error")
        .map(|r| r["ruleId"].as_str().unwrap().to_owned())
        .collect();
    if esbuild().is_some() {
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(code(&o), 0);
        assert!(corpus.join("packs/BP/scripts/main.js").is_file());
    } else {
        assert!(errors.iter().all(|e| e == "BCN0060"), "{errors:?}");
    }
    assert!(corpus.join("packs/BP/manifest.json").is_file());
    assert!(corpus.join(".beacon/out/lib/RP/manifest.json").is_file());
}

/// Trusted, scripts present, no esbuild anywhere, no TTY and no `--yes`: the download is not
/// allowed (BCN0065, exit USAGE) and nothing is written.
#[test]
fn download_needs_confirm_without_yes() {
    let h = tempfile::tempdir().unwrap();
    let p = project(false);
    write(
        p.path(),
        "src/m/scripts/main.ts",
        "export {};
",
    );
    let ps = p.path().to_str().unwrap();
    let o = cmd(h.path(), &["build", "--trust", "--project", ps])
        .env_remove("BEACON_ESBUILD")
        .env("PATH", "")
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{}", stderr(&o));
    assert!(
        stderr(&o).contains("BCN0065") && stderr(&o).contains("--yes"),
        "{}",
        stderr(&o)
    );
    assert!(!h.path().join("tools").exists());
}

/// `BEACON_DOWNLOAD=yes` consents to downloads only: it must never confirm `trust`.
#[test]
fn download_env_does_not_confirm_trust() {
    let h = tempfile::tempdir().unwrap();
    let p = project(true);
    let ps = p.path().to_str().unwrap();
    let o = cmd(h.path(), &["trust", "--project", ps])
        .env("BEACON_DOWNLOAD", "yes")
        .output()
        .unwrap();
    assert_eq!(code(&o), 2, "{}", stderr(&o));
    assert!(!h.path().join("trust.json").exists());
}
