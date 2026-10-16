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
