//! Managed dev world (4.3): seeded once, only the pack lists rewritten; two dev targets.

mod b1_common;
mod c1_common;
use b1_common::{errors, has, opts, session, write};
use beacon_engine::build::{BuildOptions, BuildOutcome};
use beacon_engine::diag::Code;
use c1_common::fixture_bytes;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::SystemTime;

const TWO_OUTPUTS: &str = r#"[
  { "id": "game", "include": ["src/game/**"] },
  { "id": "lib", "include": ["src/lib/**"] }
]"#;

fn config(outputs: &str, world: &str, targets: &str) -> String {
    format!(
        r#"{{
  "type": "minecraftBedrock", "name": "c3 dev", "targetVersion": "1.21.100", "namespace": "demo",
  "beacon": {{ "outputs": {outputs}, "targets": {{ {targets} }} }}
}}{world}"#
    )
}

fn dev(world: &str) -> String {
    format!(r#""dev": {{ "edition": "bedrock", "artifact": "dev"{world} }}"#)
}

fn project(outputs: &str, targets: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "config.json", &config(outputs, "", targets));
    for m in ["game", "lib"]
        .into_iter()
        .filter(|m| m == &"game" || outputs.contains(&format!("{m}/")))
    {
        write(
            r,
            &format!("src/{m}/items/{m}.item.json"),
            &format!(
                r#"{{"format_version":"1.21.100","minecraft:item":{{"description":{{"identifier":"demo:{m}"}},"components":{{}}}}}}"#
            ),
        );
    }
    write(r, "src/game/g.gen.ts", "");
    std::fs::create_dir_all(r.join("worlds/main/db")).unwrap();
    std::fs::write(r.join("worlds/main/level.dat"), fixture_bytes()).unwrap();
    std::fs::write(r.join("worlds/main/db/000001.ldb"), b"LDB\0data").unwrap();
    std::fs::write(r.join("worlds/main/levelname.txt"), "Source").unwrap();
    d
}

fn run(root: &Path, o: BuildOptions) -> BuildOutcome {
    session(o, root, &Arc::new(AtomicUsize::new(0))).build()
}

fn deploy(root: &Path, target: &str, out: &Path) -> BuildOutcome {
    let mut o = opts(root, Some(target));
    o.out = Some(out.to_path_buf());
    run(root, o)
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn worlds(out: &Path) -> Vec<PathBuf> {
    names(&out.join("minecraftWorlds"))
        .into_iter()
        .map(|n| out.join("minecraftWorlds").join(n))
        .collect()
}

fn json(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// path -> (bytes, mtime) of every file below `dir`.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let m = std::fs::metadata(&p).unwrap().modified().unwrap();
                out.insert(p.clone(), (std::fs::read(&p).unwrap(), m));
            }
        }
    }
    out
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e["pack_id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn seeded_once_lists_rewritten_world_data_never_touched() {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(r#", "world": "worlds/main""#),
    );
    let out = tempfile::tempdir().unwrap();
    let source = snapshot(&p.path().join("worlds"));
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);

    let w = worlds(out.path());
    assert_eq!(w.len(), 1, "{w:?}");
    let name = w[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("c3 dev-") && name.len() == "c3 dev-".len() + 8,
        "{name}"
    );
    assert_eq!(
        std::fs::read(w[0].join("db/000001.ldb")).unwrap(),
        b"LDB\0data"
    );
    let level = std::fs::read(w[0].join("level.dat")).unwrap();
    assert_ne!(level, fixture_bytes(), "edition transform applied");
    assert!(
        level.windows(5).any(|x| x == b"c3 de"),
        "LevelName is the project name"
    );
    // dev UUIDs: the pack list matches the deployed (dev) manifests
    let bp = json(&w[0].join("world_behavior_packs.json"));
    let dev_bp = names(&out.path().join("development_behavior_packs"));
    assert_eq!(dev_bp.len(), 1);
    let m = json(
        &out.path()
            .join("development_behavior_packs")
            .join(&dev_bp[0])
            .join("manifest.json"),
    );
    assert_eq!(ids(&bp), [m["header"]["uuid"].as_str().unwrap()]);
    assert!(
        names(out.path())
            .iter()
            .all(|n| !n.ends_with(".beacon-seeding"))
    );
    assert!(
        names(&out.path().join("minecraftWorlds"))
            .iter()
            .all(|n| !n.ends_with(".beacon-seeding"))
    );

    // the game and the player change the world; a new output joins the project
    std::fs::write(w[0].join("db/SENTINEL"), b"keep").unwrap();
    std::fs::write(w[0].join("level.dat"), b"PLAYER PROGRESS").unwrap();
    write(
        p.path(),
        "src/lib/items/lib.item.json",
        r#"{"format_version":"1.21.100","minecraft:item":{"description":{"identifier":"demo:lib"},"components":{}}}"#,
    );
    write(
        p.path(),
        "config.json",
        &config(TWO_OUTPUTS, "", &dev(r#", "world": "worlds/main""#)),
    );
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(worlds(out.path()), w, "seeded once, not again");
    assert_eq!(std::fs::read(w[0].join("db/SENTINEL")).unwrap(), b"keep");
    assert_eq!(
        std::fs::read(w[0].join("level.dat")).unwrap(),
        b"PLAYER PROGRESS"
    );
    assert_eq!(
        std::fs::read(w[0].join("db/000001.ldb")).unwrap(),
        b"LDB\0data"
    );
    let bp2 = json(&w[0].join("world_behavior_packs.json"));
    assert_eq!(ids(&bp2).len(), 2, "lists follow the outputs: {bp2}");
    assert_eq!(ids(&bp2)[0], ids(&bp)[0], "game stays first");
    // the pack lists are the only files the build owns in the world
    let ledger = json(&w[0].join(".beacon-emit.json"));
    let mut owned: Vec<&String> = ledger.as_object().unwrap().keys().collect();
    owned.sort();
    assert_eq!(
        owned,
        ["world_behavior_packs.json", "world_resource_packs.json"]
    );
    // the project world was only read
    assert_eq!(snapshot(&p.path().join("worlds")), source);
}

#[test]
fn two_worktrees_two_world_folders() {
    let t = r#"{ "id": "game", "include": ["src/game/**"] }"#;
    let a = project(&format!("[{t}]"), &dev(r#", "world": "worlds/main""#));
    let b = project(&format!("[{t}]"), &dev(r#", "world": "worlds/main""#));
    let out = tempfile::tempdir().unwrap();
    for p in [&a, &b] {
        let r = deploy(p.path(), "dev", out.path());
        assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    }
    assert_eq!(worlds(out.path()).len(), 2);
    assert_eq!(
        names(&out.path().join("development_behavior_packs")).len(),
        2
    );
}

#[test]
fn missing_world_is_world_not_found() {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(r#", "world": "worlds/nope""#),
    );
    let out = tempfile::tempdir().unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(has(&r, Code::WorldNotFound), "{:?}", r.diagnostics);
    assert!(names(out.path()).is_empty(), "nothing deployed");
}

#[test]
fn world_false_deploys_packs_only() {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(r#", "world": false"#),
    );
    let out = tempfile::tempdir().unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(names(out.path()), ["development_behavior_packs"]);
}

/// No `world` on the dev target: a generated flat creative world named after the project.
#[test]
fn default_world_is_generated_and_only_lists_are_rewritten() {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(""),
    );
    let out = tempfile::tempdir().unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let w = worlds(out.path());
    assert_eq!(w.len(), 1, "{w:?}");
    let name = w[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("c3 dev-") && name.len() == "c3 dev-".len() + 8,
        "{name}"
    );
    assert_eq!(
        std::fs::read_to_string(w[0].join("levelname.txt")).unwrap(),
        "c3 dev"
    );
    let level = std::fs::read(w[0].join("level.dat")).unwrap();
    let (_, root) = beacon_engine::nbt::parse_level_dat(&level).unwrap();
    let get = |k| beacon_engine::nbt::get(&root, k).and_then(|t| t.as_i64());
    assert_eq!(get("GameType"), Some(1), "creative");
    assert_eq!(get("Generator"), Some(2), "flat");
    assert_eq!(get("commandsEnabled"), Some(1), "cheats on");
    let bp = json(&w[0].join("world_behavior_packs.json"));
    let dev_manifest = names(&out.path().join("development_behavior_packs"));
    let manifest = json(
        &out.path()
            .join("development_behavior_packs")
            .join(&dev_manifest[0])
            .join("manifest.json"),
    );
    assert_eq!(ids(&bp), [manifest["header"]["uuid"].as_str().unwrap()]);
    assert_eq!(bp[0]["version"], manifest["header"]["version"]);

    // second deploy: world data and level.dat untouched, only the lists are rewritten
    let before = snapshot(&w[0]);
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let after = snapshot(&w[0]);
    for (path, (bytes, _)) in &before {
        assert_eq!(&after[path].0, bytes, "{path:?}");
    }
    assert_eq!(before.len(), after.len());
    assert_eq!(std::fs::read(w[0].join("level.dat")).unwrap(), level);

    // deterministic: a fresh deploy of the same project gives the same level.dat
    let out2 = tempfile::tempdir().unwrap();
    deploy(p.path(), "dev", out2.path());
    let w2 = worlds(out2.path());
    assert_eq!(std::fs::read(w2[0].join("level.dat")).unwrap(), level);
}

#[test]
fn default_world_never_touches_a_foreign_folder() {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(""),
    );
    let out = tempfile::tempdir().unwrap();
    deploy(p.path(), "dev", out.path());
    let w = worlds(out.path());
    // the player's world now: no ledger
    std::fs::remove_file(w[0].join(".beacon-emit.json")).unwrap();
    std::fs::write(w[0].join("world_behavior_packs.json"), b"[\"mine\"]").unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(!errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        std::fs::read(w[0].join("world_behavior_packs.json")).unwrap(),
        b"[\"mine\"]"
    );
}

#[test]
fn two_dev_targets_do_not_clobber_each_other() {
    let targets = format!(
        r#"{}, "beta": {{ "edition": "bedrock", "artifact": "dev", "world": "worlds/main", "define": {{ "BETA_MODE": true }} }}"#,
        dev(r#", "world": "worlds/main""#)
    );
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &targets,
    );
    let out = tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let mut o = opts(p.path(), None);
        o.all = true;
        o.out = Some(out.path().to_path_buf());
        let r = run(p.path(), o);
        assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
        let bp = out.path().join("development_behavior_packs");
        let dirs = names(&bp);
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        for (target, dir) in [("beta", &dirs[0]), ("dev", &dirs[1])] {
            assert!(dir.starts_with(&format!("c3 dev-{target}-")), "{dir}");
            let f =
                std::fs::read_to_string(bp.join(dir).join("functions/game/g.mcfunction")).unwrap();
            assert!(f.starts_with(&format!("say {target} ")), "{dir}: {f}");
        }
        assert_eq!(worlds(out.path()).len(), 2);
    }
}

fn seeded() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let p = project(
        r#"[{ "id": "game", "include": ["src/game/**"] }]"#,
        &dev(r#", "world": "worlds/main""#),
    );
    let out = tempfile::tempdir().unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let w = worlds(out.path()).remove(0);
    (p, out, w)
}

#[test]
fn foreign_world_folder_is_never_written() {
    for corrupt in [false, true] {
        let (p, out, w) = seeded();
        let list = w.join("world_behavior_packs.json");
        std::fs::write(&list, b"[\"users own\"]").unwrap();
        if corrupt {
            std::fs::write(w.join(".beacon-emit.json"), b"{not json").unwrap();
        } else {
            std::fs::remove_file(w.join(".beacon-emit.json")).unwrap();
        }
        let r = deploy(p.path(), "dev", out.path());
        assert!(
            !errors(&r).is_empty(),
            "corrupt={corrupt}: {:?}",
            r.diagnostics
        );
        assert_eq!(std::fs::read(&list).unwrap(), b"[\"users own\"]");
    }
}

#[test]
fn user_folder_named_like_staging_survives() {
    let (p, out, w) = seeded();
    let staging = w.with_file_name(format!(
        "{}.beacon-seeding",
        w.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(staging.join("db")).unwrap();
    std::fs::write(staging.join("db/mine"), b"precious").unwrap();
    std::fs::remove_dir_all(&w).unwrap(); // forces a re-seed
    let r = deploy(p.path(), "dev", out.path());
    assert!(!errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(std::fs::read(staging.join("db/mine")).unwrap(), b"precious");
    assert!(!w.exists());
}

#[test]
fn stale_staging_folder_of_ours_is_replaced() {
    let (p, out, w) = seeded();
    let staging = w.with_file_name(format!(
        "{}.beacon-seeding",
        w.file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join(".beacon-staging"), b"").unwrap();
    std::fs::write(staging.join("junk"), b"x").unwrap();
    std::fs::remove_dir_all(&w).unwrap();
    let r = deploy(p.path(), "dev", out.path());
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(w.join("level.dat").is_file() && !staging.exists());
    assert!(!w.join(".beacon-staging").exists());
}
