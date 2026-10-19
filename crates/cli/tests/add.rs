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

fn beacon(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    run(home, cwd, args, "")
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

/// `check` always; `build --trust` when esbuild is available (the template has a script).
fn passes(home: &Path, dir: &Path) {
    let c = beacon(home, dir, &["check"]);
    if esbuild().is_some() {
        assert_eq!(code(&c), 0, "{}", err(&c));
        let b = beacon(home, dir, &["build", "--trust", "--target", "addon"]);
        assert_eq!(code(&b), 0, "{}", err(&b));
    } else {
        assert_ne!(code(&c), 3, "{}", err(&c));
    }
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap()
}

#[test]
fn each_kind_scaffolds_then_check_and_build_pass() {
    let (home, _ws, dir) = project();
    for (kind, name, file) in [
        ("entity", "robo_cat", "entities/robo_cat.entity.json"),
        ("entity", "robo_cat", "entities/robo_cat.client_entity.json"),
        ("item", "ruby", "items/ruby.item.json"),
        ("block", "ruby_ore", "blocks/ruby_ore.block.json"),
    ] {
        if !dir.join("src/main").join(file).exists() {
            let o = beacon(home.path(), &dir, &["add", kind, name]);
            assert_eq!(code(&o), 0, "{}", err(&o));
        }
        assert!(read(&dir, &format!("src/main/{file}")).contains("demo:"));
    }
    let lang = read(&dir, "src/main/texts/en_US.lang");
    for l in [
        "demo.hello=Hello from demo",
        "entity.demo:robo_cat.name=Robo Cat",
        "item.demo:ruby=Ruby",
        "tile.demo:ruby_ore.name=Ruby Ore",
    ] {
        assert!(lang.lines().any(|x| x == l), "{l} in {lang}");
    }
    passes(home.path(), &dir);
}

#[test]
fn module_kind_creates_folder_and_module_flag_targets_it() {
    let (home, _ws, dir) = project();
    let o = beacon(home.path(), &dir, &["add", "module", "extra"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(dir.join("src/extra").is_dir());
    let again = beacon(home.path(), &dir, &["add", "module", "extra"]);
    assert_eq!(code(&again), 2);
    let o = beacon(
        home.path(),
        &dir,
        &["add", "item", "gem", "--module", "extra"],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(dir.join("src/extra/items/gem.item.json").is_file());
    assert!(!dir.join("src/main/items").exists());
    passes(home.path(), &dir);
}

#[test]
fn never_overwrites_and_rejects_bad_names() {
    let (home, _ws, dir) = project();
    assert_eq!(
        code(&beacon(home.path(), &dir, &["add", "item", "ruby"])),
        0
    );
    let p = dir.join("src/main/items/ruby.item.json");
    fs::write(&p, "{\"mine\":true}").unwrap();
    let lang = read(&dir, "src/main/texts/en_US.lang");
    let o = beacon(home.path(), &dir, &["add", "item", "ruby"]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert_eq!(fs::read_to_string(&p).unwrap(), "{\"mine\":true}");
    assert_eq!(read(&dir, "src/main/texts/en_US.lang"), lang);
    // entity: the client file exists, so the BP file must not appear either
    let c = dir.join("src/main/entities/cat.client_entity.json");
    fs::create_dir_all(c.parent().unwrap()).unwrap();
    fs::write(&c, "x").unwrap();
    assert_eq!(
        code(&beacon(home.path(), &dir, &["add", "entity", "cat"])),
        2
    );
    assert!(!dir.join("src/main/entities/cat.entity.json").exists());
    for bad in ["Ruby", "a-b", "a b", "../x", "a:b", ""] {
        let o = beacon(home.path(), &dir, &["add", "item", bad]);
        assert_eq!(code(&o), 2, "{bad}: {}", err(&o));
    }
    let o = beacon(home.path(), &dir, &["add", "item", "x", "--module", "nope"]);
    assert_eq!(code(&o), 2);
}

#[test]
fn pack_roots_modules_are_refused_or_skipped() {
    let (home, _ws, dir) = project();
    let m = dir.join("src/aaa");
    fs::create_dir_all(&m).unwrap();
    fs::write(m.join("module.json"), r#"{"packRoots":{"BP":"bp"}}"#).unwrap();
    // explicit: refused
    let o = beacon(home.path(), &dir, &["add", "item", "x", "--module", "aaa"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("packRoots"), "{}", err(&o));
    assert!(!m.join("items").exists());
    // default: the first module is aaa (packRoots), so main is used and said so
    let o = beacon(home.path(), &dir, &["add", "item", "x"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(err(&o).contains("aaa"), "{}", err(&o));
    assert!(dir.join("src/main/items/x.item.json").is_file());
    // forced
    let o = beacon(
        home.path(),
        &dir,
        &["add", "item", "y", "--module", "aaa", "--force-convention"],
    );
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(m.join("items/y.item.json").is_file());
}
