//! Debug interop (6.6): the debug mirror's map binds to the real `.ts` files, and the launch
//! configuration carries the dev script module UUID. The fake bundler reports maps the way the
//! real one does (`sources` relative to the project root); the real esbuild is checked in
//! `crates/bundler/tests/esbuild.rs`.

mod b1_common;
use b1_common::*;
use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::config::LOCK_FILE;
use beacon_engine::debug::{self, DEFAULT_PORT, Mode};
use beacon_engine::host::{BundleRequest, BundleResult, Bundler};
use beacon_engine::lock::{Lockfile, dev_uuid};
use beacon_engine::path::RelPath;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const MAP: &str = r#"{"version":3,"sources":["src/m/scripts/main.ts","src/m/lib/util.ts"],"sourcesContent":["a","b"],"mappings":"AAAA","names":[]}"#;

struct MapBundler;

impl Bundler for MapBundler {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        let mut r = FakeBundler.bundle(req);
        r.map = req.sourcemap.then(|| MAP.into());
        r
    }
    fn version(&self) -> Option<String> {
        Some("map 1".into())
    }
}

fn scripted(root: &Path) {
    write(root, "src/m/scripts/main.ts", "import './../lib/util';");
    write(root, "src/m/lib/util.ts", "export {};");
}

fn build(root: &Path, target: &str) -> beacon_engine::build::BuildOutcome {
    let runs = Arc::new(AtomicUsize::new(0));
    let o = BuildOptions {
        target: Some(target.into()),
        ..opts(root, Some(target))
    };
    let r = Session::new(
        root,
        o,
        Box::new(FakeHost(runs)),
        Some(Box::new(MapBundler)),
    )
    .build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    r
}

fn check_mirror(root: &Path) {
    let map_path = root.join(".beacon/debug/dev/main/scripts/main.js.map");
    let map: Value = serde_json::from_str(&std::fs::read_to_string(&map_path).unwrap()).unwrap();
    assert_eq!(map["file"], "scripts/main.js");
    assert_eq!(map["sourcesContent"].as_array().unwrap().len(), 2);
    let sources = map["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    for s in sources {
        let s = s.as_str().unwrap();
        assert!(!s.contains(':'), "{s}");
        let real = map_path.parent().unwrap().join(s);
        assert!(real.is_file(), "{s} does not resolve to a file: {real:?}");
        assert!(s.ends_with(".ts"), "{s}");
    }
}

#[test]
fn mirror_binds_to_ts_sources() {
    let p = project(&[]);
    scripted(p.path());
    build(p.path(), "dev");
    check_mirror(p.path());
    // never inside the packs
    assert!(!p.path().join("out").join(".beacon").exists());
    let mut n = 0;
    let mut stack = vec![p.path().join("out")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().ends_with("main.js.map") {
                n += 1;
            }
        }
    }
    assert!(n >= 1, "the pack keeps its own map");
}

/// A project on another drive than the system TEMP (here: the repo's `target/`, which is `D:`
/// on the dev machine; the same drive on CI, where the property holds trivially).
#[test]
fn mirror_binds_across_drives() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    std::fs::create_dir_all(&base).unwrap();
    let t = tempfile::tempdir_in(&base).unwrap();
    let root = t.path().canonicalize().unwrap();
    for (p, b) in [
        ("config.json", CONFIG),
        ("src/m/scripts/main.ts", "export {};"),
        ("src/m/lib/util.ts", "export {};"),
    ] {
        write(&root, p, b);
    }
    build(&root, "dev");
    check_mirror(&root);
}

#[test]
fn mirror_only_for_dev_builds() {
    let p = project(&[]);
    scripted(p.path());
    build(p.path(), "addon");
    assert!(!p.path().join(".beacon/debug").exists());
}

#[test]
fn rewrite_map_forms() {
    let root = Path::new("D:/work/proj");
    let mirror = RelPath::parse(".beacon/debug/dev/main/scripts").unwrap();
    let map = r#"{"sources":["file:///D:/work/proj/src/a.ts","D:\\work\\proj\\src\\b.ts","src/c.ts","./src/d.ts","E:/other/e.ts","https://x/y.ts"]}"#;
    let out = debug::rewrite_map(map, root, &mirror, "scripts/main.js").unwrap();
    let v: Value = serde_json::from_str(&out).unwrap();
    let up = "../../../../../";
    assert_eq!(
        v["sources"],
        serde_json::json!([
            format!("{up}src/a.ts"),
            format!("{up}src/b.ts"),
            format!("{up}src/c.ts"),
            format!("{up}src/d.ts"),
            "E:/other/e.ts",
            "https://x/y.ts"
        ])
    );
    assert_eq!(v["file"], "scripts/main.js");
    assert!(debug::rewrite_map("nope", root, &mirror, "x").is_err());
}

#[test]
fn launch_config_resolves_dev_uuid() {
    let p = project(&[]);
    let cfg = beacon_engine::route::load(p.path()).unwrap();
    let lock = Lockfile::ensure(p.path(), &cfg, &["main".into()], &[], &[], false).unwrap();
    assert!(p.path().join(LOCK_FILE).is_file());
    let mut c = debug::launch_config(p.path(), &cfg, &lock, "dev", "main", Mode::Connect).unwrap();
    let release = lock.script_module_uuid("main").unwrap();
    assert_eq!(c.target_module_uuid, dev_uuid(&release, p.path()));
    assert_ne!(c.target_module_uuid, release);
    assert_eq!(c.port, DEFAULT_PORT);
    let mirror = "${workspaceFolder}/.beacon/debug/dev/main";
    assert_eq!(c.local_root, mirror);
    let v = serde_json::to_value(&c).unwrap();
    for k in [
        "type",
        "request",
        "name",
        "mode",
        "host",
        "port",
        "localRoot",
        "sourceMapRoot",
        "generatedSourceRoot",
        "targetModuleUuid",
    ] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(
        (v["type"].as_str(), v["mode"].as_str()),
        (Some("minecraft-js"), Some("connect"))
    );
    c.absolutize(p.path());
    assert!(!c.source_map_root.contains("${"), "{}", c.source_map_root);
    assert!(c.source_map_root.ends_with("/.beacon/debug/dev/main"));
    let l = debug::launch_config(p.path(), &cfg, &lock, "dev", "main", Mode::Listen).unwrap();
    assert!(serde_json::to_value(&l).unwrap().get("host").is_none());
}

#[test]
fn launch_config_rejects_stale_names() {
    let p = project(&[]);
    let cfg = beacon_engine::route::load(p.path()).unwrap();
    let lock = Lockfile::ensure(p.path(), &cfg, &["main".into()], &[], &[], false).unwrap();
    for (t, o) in [("nope", "main"), ("addon", "main"), ("dev", "nope")] {
        let e = debug::launch_config(p.path(), &cfg, &lock, t, o, Mode::Connect).unwrap_err();
        assert_eq!(
            e.code,
            beacon_engine::diag::Code::LaunchConfigStale,
            "{t}/{o}"
        );
    }
}
