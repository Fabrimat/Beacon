//! An imported release (empty hash, `Lockfile::import_manifests`) is adopted, never bumped.

mod b1_common;
mod c3_common;
use b1_common::{errors, has, opts, session, write};
use beacon_engine::build::{BuildOutcome, ReleaseOptions};
use beacon_engine::diag::Code;
use c3_common::*;
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn run(p: &Path, rel: Option<ReleaseOptions>) -> BuildOutcome {
    let mut o = opts(p, Some("addon"));
    o.release = rel;
    session(o, p, &Arc::new(AtomicUsize::new(0))).build()
}

/// Gives every pack of `game` an imported release: 1.4.2, no hash.
fn import(p: &Path) {
    assert!(errors(&run(p, Some(Default::default()))).is_empty());
    let mut l = lock(p);
    for pack in ["BP", "RP"] {
        l["outputs"]["game"]["editions"]["bedrock"][pack]["release"] =
            serde_json::json!({"version": [1, 4, 2], "hash": ""});
    }
    std::fs::write(
        p.join("beacon.lock.json"),
        serde_json::to_vec_pretty(&l).unwrap(),
    )
    .unwrap();
}

fn check(p: &Path) {
    // a plain build neither warns nor changes versions
    let r = run(p, None);
    assert!(
        errors(&r).is_empty() && !has(&r, Code::PackReleaseBehind),
        "{:?}",
        r.diagnostics
    );
    let r = run(p, Some(Default::default()));
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    for key in ["game/BP", "game/RP"] {
        let v = r.report.packs.iter().find(|v| v.pack == key).unwrap();
        assert_eq!((v.version, v.bumped), ([1, 4, 2], false), "{key}");
        let l: Value = lock(p);
        let h = &l["outputs"]["game"]["editions"]["bedrock"][&key[5..]]["release"];
        assert_eq!(h["version"], serde_json::json!([1, 4, 2]));
        assert_eq!(h["hash"], v.hash.as_str());
        assert!(!v.hash.is_empty());
    }
    // the hash is now real: a content change bumps as usual
    write(p, "src/game/functions/start.mcfunction", "say changed\n");
    let r = run(p, Some(Default::default()));
    assert_eq!(
        r.report
            .packs
            .iter()
            .find(|v| v.pack == "game/BP")
            .unwrap()
            .version,
        [1, 4, 3]
    );
}

#[test]
fn imported_release_is_adopted_not_bumped() {
    let p = multipack();
    import(p.path());
    check(p.path());
}

#[test]
fn imported_release_is_not_stale_under_manual_policy() {
    let p = multipack();
    let c = std::fs::read_to_string(p.path().join("config.json")).unwrap();
    write(
        p.path(),
        "config.json",
        &c.replace("\"sources\"", "\"versionPolicy\": \"manual\", \"sources\""),
    );
    import(p.path());
    let r = run(p.path(), Some(Default::default()));
    assert!(
        errors(&r).is_empty() && !has(&r, Code::PackVersionStale),
        "{:?}",
        r.diagnostics
    );
    assert_eq!(
        r.report
            .packs
            .iter()
            .find(|v| v.pack == "game/BP")
            .unwrap()
            .version,
        [1, 4, 2]
    );
}
