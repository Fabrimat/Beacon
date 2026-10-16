//! Config can never steer writes outside their base; `check` persists nothing; a build that
//! keeps seeing files change writes nothing.

mod b1_common;
use b1_common::*;
use beacon_engine::build::Session;
use beacon_engine::diag::Code;
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn config_error(beacon: &str, packs: &str) -> Vec<String> {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "config.json",
        &format!(
            r#"{{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"demo","packs":{packs},"beacon":{beacon}}}"#
        ),
    );
    match beacon_engine::route::load(d.path()) {
        Ok(_) => vec![],
        Err(ds) => ds
            .into_iter()
            .inspect(|d| assert_eq!(d.code, Code::ConfigField, "{d:?}"))
            .map(|d| d.message)
            .collect(),
    }
}

#[test]
fn path_like_output_ids_are_rejected() {
    for id in ["../../x", "a/b", "a\\\\b", "", "C:x", "a b", "."] {
        let e = config_error(&format!(r#"{{"outputs":[{{"id":"{id}"}}]}}"#), "{}");
        assert_eq!(e.len(), 1, "{id:?}: {e:?}");
    }
    assert!(config_error(r#"{"outputs":[{"id":"main-2_x"}]}"#, "{}").is_empty());
}

#[test]
fn path_like_target_names_are_rejected() {
    for t in ["../t", "a/b", "x.y", ""] {
        let e = config_error(
            &format!(r#"{{"targets":{{"{t}":{{"edition":"bedrock","artifact":"mcaddon"}}}}}}"#),
            "{}",
        );
        assert_eq!(e.len(), 1, "{t:?}: {e:?}");
    }
}

#[test]
fn escaping_pack_dirs_are_rejected() {
    for p in [
        "../out",
        "./../out",
        "/etc/x",
        "C:/x",
        "a/../../b",
        ".",
        "./",
    ] {
        for key in ["behaviorPack", "resourcePack"] {
            let e = config_error("{}", &format!(r#"{{"{key}":"{p}"}}"#));
            assert_eq!(e.len(), 1, "{key}={p:?}: {e:?}");
        }
    }
    assert!(
        config_error(
            "{}",
            r#"{"behaviorPack":"./packs/BP/","resourcePack":"packs/RP"}"#
        )
        .is_empty()
    );
}

#[test]
fn unknown_primary_output_is_rejected() {
    assert_eq!(config_error(r#"{"primaryOutput":"nope"}"#, "{}").len(), 1);
    assert!(config_error(r#"{"primaryOutput":"main"}"#, "{}").is_empty());
    assert_eq!(
        config_error(r#"{"outputs":[{"id":"a"}],"primaryOutput":"main"}"#, "{}").len(),
        1
    );
}

#[test]
fn escaping_config_writes_nothing() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("proj");
    write(&root, "src/m/functions/a.mcfunction", "say a");
    write(
        &root,
        "config.json",
        &CONFIG.replacen(
            r#""namespace": "demo","#,
            r#""namespace": "demo", "packs": {"behaviorPack": "../escaped"},"#,
            1,
        ),
    );
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(&root, None);
    o.out = None;
    let r = session(o, &root, &runs).build();
    assert!(has(&r, Code::ConfigField), "{:?}", r.diagnostics);
    assert!(!parent.path().join("escaped").exists());
}

#[test]
fn check_persists_nothing() {
    let p = project(&[("src/m/g.gen.ts", "list src/m")]);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), None), p.path(), &runs).check();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "trusted check runs generators"
    );
    assert!(!p.path().join(".beacon").exists(), "no cache written");
    assert!(!p.path().join("beacon.lock.json").exists());
    assert!(!p.path().join("out").exists());
}

/// Rewrites `path` on every run (or only the first) while the build is in flight, like an
/// editor saving mid-build. Uncacheable so it runs on every attempt.
struct Toucher {
    root: PathBuf,
    path: &'static str,
    once: bool,
    n: AtomicUsize,
}

impl GeneratorHost for Toucher {
    fn run(&self, g: &Generator, _: &GenCtx, vfs: &dyn Vfs) -> GenResult {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        if !self.once || n == 0 {
            let body = format!("k=v{}\n", "#".repeat(n + 1));
            std::fs::write(self.root.join(self.path), body).unwrap();
        }
        let mut r = GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        };
        r.inputs
            .files
            .insert(g.path.clone(), vfs.hash(&g.path).unwrap());
        r.inputs.uncacheable = true;
        r
    }
    fn version(&self) -> String {
        "toucher".into()
    }
}

fn toucher_build(root: &Path, once: bool) -> beacon_engine::build::BuildOutcome {
    let host = Toucher {
        root: root.into(),
        // read by the lang merge (base lang + module lang) before anything is written
        path: "src/m/texts/en_US.lang",
        once,
        n: AtomicUsize::new(0),
    };
    Session::new(root, opts(root, None), Box::new(host), None).build()
}

#[test]
fn change_during_build_retries_then_emits_a_clean_snapshot() {
    let p = project(&[("src/m/g.gen.ts", "x")]);
    let r = toucher_build(p.path(), true);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let lang = std::fs::read_to_string(p.path().join("out/main/RP/texts/en_US.lang")).unwrap();
    assert!(lang.contains("k=v#\n"), "{lang}");
}

#[test]
fn files_that_keep_changing_write_nothing() {
    let p = project(&[("src/m/g.gen.ts", "x")]);
    let r = toucher_build(p.path(), false);
    let e = errors(&r);
    assert!(
        e.iter()
            .any(|d| d.code == Code::EmitFailed && d.message.contains("kept changing")),
        "{:?}",
        r.diagnostics
    );
    assert!(!p.path().join("out").exists(), "nothing written");
}

#[test]
fn shared_or_nested_pack_dirs_are_rejected() {
    for (bp, rp) in [
        ("packs", "./packs/"),
        ("packs", "packs/RP"),
        ("packs/BP/x", "Packs/bp"),
    ] {
        let e = config_error(
            "{}",
            &format!(r#"{{"behaviorPack":"{bp}","resourcePack":"{rp}"}}"#),
        );
        assert_eq!(e.len(), 1, "{bp} {rp}: {e:?}");
    }
    assert!(
        config_error(
            "{}",
            r#"{"behaviorPack":"packs/BP","resourcePack":"packs/BPx"}"#
        )
        .is_empty()
    );
}

/// Directory link at `link` -> `target` (junction on Windows: no privilege needed).
fn dir_link(target: &Path, link: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let ok = std::process::Command::new("cmd")
            .raw_arg(format!(
                "/C mklink /J \"{}\" \"{}\"",
                link.display(),
                target.display()
            ))
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J failed");
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

fn with_packs(root: &Path) {
    write(
        root,
        "config.json",
        &CONFIG.replacen(
            r#""namespace": "demo","#,
            r#""namespace": "demo", "packs": {"behaviorPack": "./packs/BP", "resourcePack": "./packs/RP"},"#,
            1,
        ),
    );
}

fn count_files(d: &Path) -> usize {
    std::fs::read_dir(d).map_or(0, |r| r.count())
}

#[test]
fn linked_pack_dir_untrusted_refuses_trusted_warns() {
    let p = project(&[]);
    with_packs(p.path());
    let outside = tempfile::tempdir().unwrap();
    dir_link(outside.path(), &p.path().join("packs/BP"));
    let runs = Arc::new(AtomicUsize::new(0));

    let mut o = opts(p.path(), None);
    o.out = None;
    o.trusted = false;
    let r = session(o.clone(), p.path(), &runs).build();
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == Code::OutputIsLink)
        .expect("link diag");
    assert_eq!(d.severity, beacon_engine::diag::Severity::Error);
    assert_eq!(
        count_files(outside.path()),
        0,
        "nothing written through the link"
    );
    assert!(
        p.path().join("packs/RP/manifest.json").is_file(),
        "the plain RP dir is fine"
    );

    o.trusted = true;
    let r = session(o, p.path(), &runs).build();
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.code == Code::OutputIsLink)
        .expect("link warning");
    assert_eq!(d.severity, beacon_engine::diag::Severity::Warning);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(outside.path().join("manifest.json").is_file());
}

#[test]
fn linked_state_and_dist_dirs_are_refused_untrusted() {
    let p = project(&[]);
    let outside = tempfile::tempdir().unwrap();
    dir_link(outside.path(), &p.path().join(".beacon"));
    dir_link(outside.path(), &p.path().join("dist"));
    let runs = Arc::new(AtomicUsize::new(0));
    let untrusted = |t: Option<&str>| {
        let mut o = opts(p.path(), t);
        o.out = None;
        o.trusted = false;
        o
    };
    // packs build without `packs.*`: everything goes to .beacon/out
    let r = session(untrusted(None), p.path(), &runs).build();
    assert!(has(&r, Code::OutputIsLink), "{:?}", r.diagnostics);
    let r = session(untrusted(Some("addon")), p.path(), &runs).build();
    assert!(has(&r, Code::OutputIsLink), "{:?}", r.diagnostics);
    assert!(r.report.artifacts.is_empty());
    assert_eq!(count_files(outside.path()), 0);
}

#[test]
fn links_inside_a_pack_dir_are_never_written_through() {
    let p = project(&[]);
    with_packs(p.path());
    let outside = tempfile::tempdir().unwrap();
    dir_link(outside.path(), &p.path().join("packs/BP/items"));
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(p.path(), None);
    o.out = None;
    let r = session(o, p.path(), &runs).build();
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == Code::EmitFailed && d.message.contains("is a link")),
        "{:?}",
        r.diagnostics
    );
    assert_eq!(count_files(outside.path()), 0);
    assert!(p.path().join("packs/BP/manifest.json").is_file());
}

/// A dangling symlink at a destination the ledger does not own is not followed.
#[cfg(unix)]
#[test]
fn dangling_link_at_destination_is_not_followed() {
    let p = project(&[]);
    with_packs(p.path());
    let outside = tempfile::tempdir().unwrap();
    let victim = outside.path().join("victim");
    std::fs::create_dir_all(p.path().join("packs/BP/items")).unwrap();
    std::os::unix::fs::symlink(&victim, p.path().join("packs/BP/items/sword.json")).unwrap();
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(p.path(), None);
    o.out = None;
    let r = session(o, p.path(), &runs).build();
    assert!(has(&r, Code::EmitFailed), "{:?}", r.diagnostics);
    assert!(!victim.exists());
}
