mod a2_common;
use a2_common::cfg;
use beacon_engine::config::LOCK_FILE;
use beacon_engine::diag::Code;
use beacon_engine::lock::{Lockfile, dev_uuid};
use beacon_engine::model::{Edition, Pack};

fn outs(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn creates_is_stable_and_never_removes() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(
        r#"{"targets":{"a":{"edition":"bedrock","artifact":"dev"},"b":{"edition":"education","artifact":"dev"}}}"#,
    );
    let l = Lockfile::ensure(d.path(), &c, &outs(&["main", "lib"]), false).unwrap();
    let bed = l
        .pack_uuid("main", Pack::Behavior, Edition::Bedrock)
        .unwrap();
    assert_ne!(
        bed,
        l.pack_uuid("main", Pack::Behavior, Edition::Education)
            .unwrap()
    );
    assert_ne!(
        bed,
        l.module_uuid("main", Pack::Behavior, Edition::Bedrock)
            .unwrap()
    );
    assert_eq!(bed.len(), 36);
    assert_eq!(&bed[14..15], "4");
    let path = d.path().join(LOCK_FILE);
    let before = (
        std::fs::read(&path).unwrap(),
        std::fs::metadata(&path).unwrap().modified().unwrap(),
    );
    std::thread::sleep(std::time::Duration::from_millis(30));
    // stable: same result, no rewrite (also under frozen)
    assert_eq!(
        Lockfile::ensure(d.path(), &c, &outs(&["main", "lib"]), true).unwrap(),
        l
    );
    // output disappearing keeps its entry
    let l2 = Lockfile::ensure(d.path(), &c, &outs(&["main"]), false).unwrap();
    assert_eq!(l2, l);
    assert_eq!(std::fs::read(&path).unwrap(), before.0);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before.1
    );
    assert!(!d.path().join("beacon.lock.json.tmp").exists());
}

#[test]
fn frozen_and_invalid() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg("{}");
    let e = Lockfile::ensure(d.path(), &c, &outs(&["main"]), true).unwrap_err();
    assert_eq!(e.code, Code::LockfileFrozen);
    assert!(!d.path().join(LOCK_FILE).exists());
    Lockfile::ensure(d.path(), &c, &outs(&["main"]), false).unwrap();
    let e = Lockfile::ensure(d.path(), &c, &outs(&["main", "new"]), true).unwrap_err();
    assert_eq!(e.code, Code::LockfileFrozen);
    std::fs::write(d.path().join(LOCK_FILE), "{nope").unwrap();
    let e = Lockfile::ensure(d.path(), &c, &outs(&["main"]), false).unwrap_err();
    assert_eq!(e.code, Code::LockfileInvalid);
}

#[test]
fn shared_edition_uuids() {
    let d = tempfile::tempdir().unwrap();
    let c = cfg(
        r#"{"sharedEditionUuids":true,"targets":{"a":{"edition":"bedrock","artifact":"dev"},"b":{"edition":"education","artifact":"dev"}}}"#,
    );
    let l = Lockfile::ensure(d.path(), &c, &outs(&["main"]), false).unwrap();
    for p in [Pack::Behavior, Pack::Resource] {
        assert_eq!(
            l.pack_uuid("main", p, Edition::Bedrock).unwrap(),
            l.pack_uuid("main", p, Edition::Education).unwrap()
        );
        assert_eq!(
            l.module_uuid("main", p, Edition::Bedrock).unwrap(),
            l.module_uuid("main", p, Edition::Education).unwrap()
        );
    }
    assert!(l.outputs["main"].editions.is_empty());
}

#[test]
fn dev_uuid_deterministic_per_worktree() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let rel = "11111111-1111-4111-8111-111111111111";
    let u = dev_uuid(rel, a.path());
    assert_eq!(u, dev_uuid(rel, a.path()));
    assert_ne!(u, dev_uuid(rel, b.path()));
    assert_ne!(
        u,
        dev_uuid("22222222-2222-4222-8222-222222222222", a.path())
    );
    assert_ne!(u, rel);
    assert_eq!(&u[14..15], "4");
    assert!("89ab".contains(&u[19..20]));
}
