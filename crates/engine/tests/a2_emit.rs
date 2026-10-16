mod a2_common;
use a2_common::{FakeVfs, rp};
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::emit::{EmitReport, LEDGER_FILE, emit};
use beacon_engine::model::Body;
use beacon_engine::path::RelPath;
use serde_json::json;

fn run(
    dir: &std::path::Path,
    files: &[(&str, Body)],
    vfs: &FakeVfs,
) -> (EmitReport, Vec<Diagnostic>) {
    let f: Vec<(RelPath, &Body)> = files.iter().map(|(p, b)| (rp(p), b)).collect();
    emit(dir, &f, vfs)
}

#[test]
fn foreign_stale_unchanged_passthrough() {
    let d = tempfile::tempdir().unwrap();
    let bin: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let vfs = FakeVfs::with(&[("art/a.png", &bin)]);
    let first = [
        ("manifest.json", Body::Json(json!({"b": 1, "a": 2}))),
        ("texts/en_US.lang", Body::Text("a=b\n".into())),
        ("textures/deep/a.png", Body::Passthrough(rp("art/a.png"))),
        ("items/x.json", Body::Json(json!({}))),
    ];
    let (r, diags) = run(d.path(), &first, &vfs);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!((r.written, r.unchanged, r.deleted), (4, 0, 0));
    assert_eq!(
        std::fs::read(d.path().join("textures/deep/a.png")).unwrap(),
        bin
    );
    assert_eq!(
        std::fs::read_to_string(d.path().join("manifest.json")).unwrap(),
        "{\n  \"a\": 2,\n  \"b\": 1\n}\n"
    );
    std::fs::write(d.path().join("foreign.txt"), "mine").unwrap();
    std::fs::write(d.path().join("items/foreign.json"), "{}").unwrap();

    let m = d.path().join("manifest.json");
    let t0 = std::fs::metadata(&m).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(40));
    // rebuild dropping the deep png (dir pruned) and items/x.json (dir kept: foreign file)
    let second = [first[0].clone(), first[1].clone()];
    let (r, diags) = run(d.path(), &second, &vfs);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!((r.written, r.unchanged, r.deleted), (0, 2, 2));
    assert_eq!(
        std::fs::metadata(&m).unwrap().modified().unwrap(),
        t0,
        "unchanged file rewritten"
    );
    assert!(!d.path().join("textures").exists());
    assert!(!d.path().join("items/x.json").exists());
    assert!(d.path().join("items/foreign.json").exists());
    assert_eq!(
        std::fs::read_to_string(d.path().join("foreign.txt")).unwrap(),
        "mine"
    );
    let ledger = std::fs::read_to_string(d.path().join(LEDGER_FILE)).unwrap();
    assert!(ledger.contains("manifest.json") && !ledger.contains("items/x.json"));

    // changed content replaces atomically, no temp left
    let third = [
        ("manifest.json", Body::Json(json!({"c": 3}))),
        second[1].clone(),
    ];
    let (r, _) = run(d.path(), &third, &vfs);
    assert_eq!((r.written, r.unchanged), (1, 1));
    let names: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|n| !n.ends_with(".beacon-tmp")),
        "{names:?}"
    );
}

#[test]
fn missing_or_corrupt_ledger_deletes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let vfs = FakeVfs::default();
    std::fs::write(d.path().join("old.json"), "{}").unwrap();
    let files = [("new.json", Body::Json(json!(1)))];
    let (r, diags) = run(d.path(), &files, &vfs);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, Code::LedgerMissing);
    assert_eq!(r.deleted, 0);
    assert!(d.path().join("old.json").exists());
    // fresh ledger written: next run is quiet and unchanged
    let (r, diags) = run(d.path(), &files, &vfs);
    assert!(diags.is_empty());
    assert_eq!(r.unchanged, 1);
    // corrupt ledger
    std::fs::write(d.path().join(LEDGER_FILE), "garbage").unwrap();
    let (_, diags) = run(d.path(), &[], &vfs);
    assert_eq!(diags[0].code, Code::LedgerMissing);
    assert!(d.path().join("new.json").exists() && d.path().join("old.json").exists());
}

#[test]
fn empty_dir_is_not_a_warning() {
    let d = tempfile::tempdir().unwrap();
    let (_, diags) = run(
        &d.path().join("fresh/pack"),
        &[("a.json", Body::Json(json!(1)))],
        &FakeVfs::default(),
    );
    assert!(diags.is_empty());
}

#[test]
fn missing_passthrough_source_is_emit_failed() {
    let d = tempfile::tempdir().unwrap();
    let (r, diags) = run(
        d.path(),
        &[("a.png", Body::Passthrough(rp("nope.png")))],
        &FakeVfs::default(),
    );
    assert_eq!(r.written, 0);
    assert_eq!(diags[0].code, Code::EmitFailed);
}

#[test]
fn tampered_ledger_cannot_delete_outside_dir() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("pack");
    let outside_dir = root.path().join("deep");
    std::fs::create_dir_all(&outside_dir).unwrap();
    let victim = root.path().join("victim.txt");
    let victim2 = outside_dir.join("v2.txt");
    let abs = root.path().join("abs.txt");
    for f in [&victim, &victim2, &abs] {
        std::fs::write(f, "keep").unwrap();
    }
    std::fs::create_dir_all(dir.join("a")).unwrap();
    let h = blake3::hash(b"x").to_hex().to_string();
    let ledger = json!({
        "../victim.txt": h,
        "a/../../deep/v2.txt": h,
        abs.to_str().unwrap(): h,
        r"..\victim.txt": h,
        "a/stale.txt": h,
    });
    std::fs::write(dir.join(LEDGER_FILE), ledger.to_string()).unwrap();
    std::fs::write(dir.join("a/stale.txt"), "x").unwrap();
    let (r, diags) = run(&dir, &[], &FakeVfs::default());
    assert_eq!(r.deleted, 1);
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.code == Code::LedgerMissing)
            .count(),
        4
    );
    assert!(
        diags[0].message.contains("../victim.txt")
            || diags.iter().any(|d| d.message.contains("victim"))
    );
    for f in [
        &victim,
        &victim2,
        &abs,
        &outside_dir,
        &root.path().to_path_buf(),
    ] {
        assert!(f.exists(), "{f:?}");
    }
    assert!(
        !dir.join("a").exists(),
        "legit stale file still deleted and pruned"
    );
    let new = std::fs::read_to_string(dir.join(LEDGER_FILE)).unwrap();
    assert!(!new.contains("victim") && !new.contains("abs.txt"));
}
