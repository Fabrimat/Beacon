//! B2a: watch-style rebuilds (`Session::changed`) reuse generator results and bundles, and
//! only the changed outputs are written.

mod b1_common;
use b1_common::{FakeHost, opts, project};
use beacon_engine::build::Session;
use beacon_engine::host::{BundleRequest, BundleResult, Bundler};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting(Arc<AtomicUsize>);
impl Bundler for Counting {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        b1_common::FakeBundler.bundle(req)
    }
    fn version(&self) -> Option<String> {
        None
    }
}

#[test]
fn delta_builds_skip_generators_and_bundler() {
    let p = project(&[
        ("src/m/g.gen.ts", "read src/m/items/sword.item.json\n"),
        ("src/m/scripts/main.ts", "export {}\n"),
    ]);
    let (gens, bundles) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let mut s = Session::new(
        p.path(),
        opts(p.path(), None),
        Box::new(FakeHost(gens.clone())),
        Some(Box::new(Counting(bundles.clone()))),
    );
    let first = s.build();
    assert!(first.report.emitted > 0);
    assert_eq!(
        (gens.load(Ordering::SeqCst), bundles.load(Ordering::SeqCst)),
        (1, 1)
    );

    // an unrelated text edit: nothing reruns, one file written
    let lang = p.path().join("src/m/texts/en_US.lang");
    std::fs::write(&lang, "item.demo:sword=Blade\n").unwrap();
    s.changed([lang.clone()]);
    let r = s.build();
    assert_eq!(
        (gens.load(Ordering::SeqCst), bundles.load(Ordering::SeqCst)),
        (1, 1)
    );
    assert_eq!(r.report.emitted, 1, "{:?}", r.diagnostics);
    let out = std::fs::read_to_string(p.path().join("out/main/RP/texts/en_US.lang"))
        .or_else(|_| std::fs::read_to_string(p.path().join("out/main/BP/texts/en_US.lang")))
        .unwrap();
    assert!(out.contains("Blade"), "{out}");

    // nothing changed: nothing written
    s.changed([lang.clone()]);
    assert_eq!(s.build().report.emitted, 0);

    // a script input changed: the bundler runs once
    let ts = p.path().join("src/m/scripts/main.ts");
    std::fs::write(&ts, "export const a = 1;\n").unwrap();
    s.changed([ts]);
    s.build();
    assert_eq!(bundles.load(Ordering::SeqCst), 2);
    assert_eq!(gens.load(Ordering::SeqCst), 1);

    // a generator input changed: it reruns
    let sword = p.path().join("src/m/items/sword.item.json");
    let body = std::fs::read_to_string(&sword)
        .unwrap()
        .replace("sword", "blade");
    std::fs::write(&sword, body).unwrap();
    s.changed([sword]);
    s.build();
    assert_eq!(gens.load(Ordering::SeqCst), 2);
    assert_eq!(bundles.load(Ordering::SeqCst), 2);
}

#[test]
fn same_stat_rewrite_builds_cleanly() {
    let p = project(&[]);
    let mut s = Session::new(
        p.path(),
        opts(p.path(), None),
        Box::new(FakeHost(Arc::default())),
        None,
    );
    s.build();
    let f = p.path().join("src/m/items/sword.png");
    let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
    std::fs::write(&f, "PNH").unwrap(); // same size as "PNG"
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    s.changed([f]);
    let r = s.build();
    assert!(b1_common::errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.report.emitted, 1);
    let out = std::fs::read(p.path().join("out/main/BP/items/sword.png"))
        .or_else(|_| std::fs::read(p.path().join("out/main/RP/items/sword.png")))
        .or_else(|_| std::fs::read(p.path().join("out/main/RP/textures/sword.png")));
    if let Ok(o) = out {
        assert_eq!(o, b"PNH");
    }
}

/// The routing memo (`route_key`) must not outlive what it was routed from: each edit below
/// changes routing while the previous result would still look valid by path count or size.
#[test]
fn delta_builds_invalidate_the_route_memo() {
    let p = project(&[("src/m/module.json", r#"{"paths":{"**/*.png":"RP/aaaa"}}"#)]);
    let root = p.path();
    let mut s = Session::new(
        root,
        opts(root, None),
        Box::new(FakeHost(Arc::default())),
        None,
    );
    let out = |rel: &str| root.join("out").join(rel).is_file();
    let rebuild = |s: &mut Session, f: &std::path::Path| {
        s.changed([f.to_path_buf()]);
        let r = s.build();
        assert!(b1_common::errors(&r).is_empty(), "{:?}", r.diagnostics);
    };
    s.build();
    assert!(out("main/RP/aaaa/sword.png"));

    // module.json: same path, same size, other `paths` target
    let mj = root.join("src/m/module.json");
    std::fs::write(&mj, r#"{"paths":{"**/*.png":"RP/bbbb"}}"#).unwrap();
    rebuild(&mut s, &mj);
    assert!(out("main/RP/bbbb/sword.png"));
    assert!(!out("main/RP/aaaa/sword.png"));

    // a file add
    let shield = root.join("src/m/items/shield.png");
    std::fs::write(&shield, "PNG").unwrap();
    rebuild(&mut s, &shield);
    assert!(out("main/RP/bbbb/shield.png"));

    // a rename: same file count, different key
    let (old, new) = (
        root.join("src/m/items/sword.png"),
        root.join("src/m/items/blade.png"),
    );
    std::fs::rename(&old, &new).unwrap();
    s.changed([old, new]);
    let r = s.build();
    assert!(b1_common::errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(out("main/RP/bbbb/blade.png"));
    assert!(!out("main/RP/bbbb/sword.png"));

    // config.json: outputs (the module now routes to another output)
    let cfg = root.join("config.json");
    let body = std::fs::read_to_string(&cfg).unwrap().replacen(
        r#""beacon": {"#,
        r#""beacon": { "outputs": [{"id":"alt","include":["src/m/**"]}],"#,
        1,
    );
    std::fs::write(&cfg, body).unwrap();
    rebuild(&mut s, &cfg);
    assert!(out("alt/RP/bbbb/blade.png"), "output set change not routed");
}
