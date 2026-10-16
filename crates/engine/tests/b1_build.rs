//! Build orchestration: determinism, ledger safety, lockfile, trust, bundling.

mod b1_common;
use b1_common::*;
use beacon_engine::build::BuildOptions;
use beacon_engine::diag::{Code, Severity};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                m.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    m
}

const SCRIPTED: &[(&str, &str)] = &[
    ("src/m/g.gen.ts", "list src/m"),
    ("src/m/scripts/main.ts", "export {};"),
];

#[test]
fn builds_are_deterministic() {
    let p = project(SCRIPTED);
    let runs = Arc::new(AtomicUsize::new(0));
    let build = |target: &str, out: &str| {
        let mut o = opts(p.path(), Some(target));
        o.out = Some(p.path().join(out));
        let r = session(o, p.path(), &runs).build();
        assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
        r
    };
    build("dev", "a");
    build("dev", "b");
    let (a, b) = (tree(&p.path().join("a")), tree(&p.path().join("b")));
    assert_eq!(a, b);
    let bp = a
        .keys()
        .find(|k| k.starts_with("development_behavior_packs/") && k.ends_with("manifest.json"))
        .expect("dev BP manifest");
    assert!(bp.contains("b1 test-"), "{bp}");
    assert!(a.keys().any(|k| k.ends_with(" BP/scripts/main.js")));
    assert!(a.keys().any(|k| k.ends_with(" BP/scripts/main.js.map")));
    assert!(a.keys().any(|k| k.ends_with(" RP/texts/languages.json")));

    let r1 = build("addon", "c");
    let r2 = build("addon", "d");
    assert_eq!(r1.report.artifacts.len(), 1);
    let read =
        |r: &beacon_engine::build::BuildOutcome| std::fs::read(&r.report.artifacts[0]).unwrap();
    assert_eq!(read(&r1), read(&r2));
    assert!(r1.report.artifacts[0].ends_with("b1 test-addon.mcaddon"));

    // mcpack: one archive per pack
    let r = build("pack", "e");
    let names: Vec<_> = r
        .report
        .artifacts
        .iter()
        .map(|a| {
            Path::new(a)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(names, ["b1 test-pack-BP.mcpack", "b1 test-pack-RP.mcpack"]);
}

#[test]
fn dev_and_release_uuids_differ() {
    let p = project(&[]);
    let runs = Arc::new(AtomicUsize::new(0));
    session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    session(opts(p.path(), None), p.path(), &runs).build();
    let manifest = |dir: &Path| -> serde_json::Value {
        let t = tree(dir);
        let (_, m) = t
            .iter()
            .find(|(k, _)| k.ends_with("RP/manifest.json"))
            .expect("RP manifest");
        serde_json::from_slice(m).unwrap()
    };
    let dev = manifest(&p.path().join("out/development_resource_packs"));
    let release = manifest(&p.path().join("out/main"));
    let lock: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.path().join("beacon.lock.json")).unwrap()).unwrap();
    let uuid = &lock["outputs"]["main"]["editions"]["bedrock"]["RP"]["header"];
    assert_eq!(&release["header"]["uuid"], uuid);
    assert_ne!(&dev["header"]["uuid"], uuid);
}

#[test]
fn foreign_files_survive_and_stale_files_go() {
    let p = project(&[("src/m/functions/a.mcfunction", "say a")]);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = session(opts(p.path(), Some("dev")), p.path(), &runs);
    s.build();
    let bp = std::fs::read_dir(p.path().join("out/development_behavior_packs"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let func = bp.join("functions/m/a.mcfunction");
    assert!(func.is_file());
    std::fs::write(bp.join("foreign.txt"), "mine").unwrap();
    std::fs::remove_file(p.path().join("src/m/functions/a.mcfunction")).unwrap();
    let o = s.build();
    assert!(errors(&o).is_empty(), "{:?}", o.diagnostics);
    assert!(!func.exists(), "stale output deleted");
    assert_eq!(
        std::fs::read_to_string(bp.join("foreign.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn lockfile_created_and_frozen() {
    let p = project(&[]);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(p.path(), None);
    o.frozen = true;
    let r = session(o, p.path(), &runs).build();
    assert!(has(&r, Code::LockfileFrozen), "{:?}", r.diagnostics);
    assert!(!p.path().join("beacon.lock.json").exists());
    assert!(
        !p.path().join("out").exists(),
        "nothing emitted without a lockfile"
    );

    let r = session(opts(p.path(), None), p.path(), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let lock = std::fs::read(p.path().join("beacon.lock.json")).unwrap();
    assert!(p.path().join("out/main/BP/manifest.json").is_file());
    // the bundler is recorded only when scripts are bundled
    assert!(!String::from_utf8_lossy(&lock).contains("fake 1"));

    // a stable lockfile passes --frozen
    let mut o = opts(p.path(), None);
    o.frozen = true;
    let r = session(o, p.path(), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        std::fs::read(p.path().join("beacon.lock.json")).unwrap(),
        lock
    );
}

#[test]
fn bundler_version_is_recorded() {
    let p = project(SCRIPTED);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), None), p.path(), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let lock = std::fs::read_to_string(p.path().join("beacon.lock.json")).unwrap();
    assert!(lock.contains("\"bundler\": \"fake 1\""), "{lock}");
    let m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.path().join("out/main/BP/manifest.json")).unwrap())
            .unwrap();
    assert!(
        m["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["entry"] == "scripts/main.js")
    );
}

#[test]
fn missing_bundler_is_a_diagnostic() {
    let p = project(SCRIPTED);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = beacon_engine::build::Session::new(
        p.path(),
        opts(p.path(), None),
        Box::new(FakeHost(runs.clone())),
        None,
    );
    let r = s.build();
    assert!(has(&r, Code::BundlerMissing), "{:?}", r.diagnostics);
    assert_eq!(errors(&r).len(), 1, "{:?}", r.diagnostics);
}

#[test]
fn untrusted_build_refuses_and_check_still_validates() {
    let p = project(&[("src/m/g.gen.ts", "list src/m"), ("src/m/notes.txt", "x")]);
    let runs = Arc::new(AtomicUsize::new(0));
    let untrusted = BuildOptions {
        out: Some(p.path().join("out")),
        ..Default::default()
    };
    let r = session(untrusted.clone(), p.path(), &runs).build();
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == Code::Untrusted)
        .expect("untrusted");
    assert_eq!(d.severity, Severity::Error);
    assert!(!p.path().join("out").exists());
    assert!(!p.path().join("beacon.lock.json").exists());

    let r = session(untrusted, p.path(), &runs).check();
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == Code::Untrusted)
        .expect("untrusted");
    assert_eq!(d.severity, Severity::Note);
    assert!(
        has(&r, Code::UnknownSuffix),
        "routing still runs: {:?}",
        r.diagnostics
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0, "no generator ran");
    assert!(
        !p.path().join("beacon.lock.json").exists(),
        "check writes nothing"
    );

    // no code: no trust needed
    let q = project(&[]);
    let r = session(BuildOptions::default(), q.path(), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
}

#[test]
fn unknown_target_and_config_errors_do_not_panic() {
    let p = project(&[]);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), Some("nope")), p.path(), &runs).build();
    assert!(has(&r, Code::ConfigField));
    write(p.path(), "config.json", "{");
    let r = session(opts(p.path(), None), p.path(), &runs).build();
    assert!(has(&r, Code::ConfigInvalid));
}

#[test]
fn generations_count_up() {
    let p = project(&[]);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = session(opts(p.path(), None), p.path(), &runs);
    assert_eq!(s.build().generation, 1);
    let r = s.build();
    assert_eq!(
        (r.generation, r.report.emitted),
        (2, 0),
        "unchanged files are not rewritten"
    );
    assert!(r.inputs.contains(&rp("src/m/items/sword.png")));
}

/// A bundle that imported something outside the root is rebuilt every time.
#[test]
fn bundles_with_external_inputs_are_not_reused() {
    use beacon_engine::host::{BundleRequest, BundleResult, Bundler};
    struct External(Arc<AtomicUsize>);
    impl Bundler for External {
        fn bundle(&self, req: &BundleRequest) -> BundleResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            let mut r = FakeBundler.bundle(req);
            r.external_inputs = true;
            r
        }
        fn version(&self) -> Option<String> {
            None
        }
    }
    let p = project(SCRIPTED);
    let (runs, bundles) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut s = beacon_engine::build::Session::new(
        p.path(),
        opts(p.path(), None),
        Box::new(FakeHost(runs.clone())),
        Some(Box::new(External(bundles.clone()))),
    );
    s.build();
    s.build();
    assert_eq!(bundles.load(Ordering::SeqCst), 2);
}

#[test]
fn per_target_min_engine_and_stub_artifacts() {
    let p = project(&[]);
    write(
        p.path(),
        "config.json",
        r#"{"type":"minecraftBedrock","name":"b1 test","targetVersion":"1.21.100","namespace":"demo",
          "beacon":{"targets":{
            "a":{"edition":"bedrock","artifact":"dev"},
            "b":{"edition":"bedrock","artifact":"dev","targetVersion":"1.21.50"},
            "w":{"edition":"bedrock","artifact":"mcworld","world":"worlds/main"}}}}"#,
    );
    let runs = Arc::new(AtomicUsize::new(0));
    let min_engine = |t: &str| {
        let r = session(opts(p.path(), Some(t)), p.path(), &runs).build();
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let packs = p.path().join("out/development_behavior_packs");
        let dir = std::fs::read_dir(packs).unwrap().next().unwrap().unwrap();
        let m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json")).unwrap())
                .unwrap();
        m["header"]["min_engine_version"].clone()
    };
    assert_eq!(min_engine("a"), serde_json::json!([1, 21, 100]));
    assert_eq!(min_engine("b"), serde_json::json!([1, 21, 50]));

    let r = session(opts(p.path(), Some("w")), p.path(), &runs).build();
    // mcworld is built now; this project has no world folder
    assert!(r.diagnostics.iter().any(|d| d.code == Code::WorldNotFound));

    write(
        p.path(),
        "config.json",
        r#"{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"demo",
          "beacon":{"targets":{"z":{"edition":"bedrock","artifact":"zip"}}}}"#,
    );
    let r = session(opts(p.path(), None), p.path(), &runs).build();
    assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    assert_eq!(r.diagnostics[0].code, Code::ConfigField);
}
