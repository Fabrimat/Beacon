use beacon_engine::diag::Code;
use beacon_engine::path::{RelPath, Vfs};
use beacon_engine::vfs::Index;
use std::fs;
use std::io::ErrorKind;

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    fs::create_dir_all(r.join("src/a")).unwrap();
    fs::write(r.join("src/a/x.json"), "{}").unwrap();
    fs::write(r.join("config.json"), "{}").unwrap();
    for ex in [".git", ".beacon", "node_modules", "packs/BP", "other"] {
        fs::create_dir_all(r.join(ex)).unwrap();
        fs::write(r.join(ex).join("f.txt"), "x").unwrap();
    }
    fs::create_dir_all(r.join("src/node_modules")).unwrap();
    fs::write(r.join("src/node_modules/f.txt"), "x").unwrap();
    d
}

#[test]
fn excludes_and_listing() {
    let d = project();
    let (idx, diags) = Index::build(d.path(), &[rp("packs/BP"), rp("other")], None);
    assert!(diags.is_empty(), "{diags:?}");
    let files: Vec<_> = idx.files().iter().map(|p| p.as_str()).collect();
    assert_eq!(files, ["config.json", "src/a/x.json"]);
    assert_eq!(idx.list(None), ["config.json", "src"]);
    assert_eq!(idx.list(Some(&rp("src"))), ["a"]);
    assert!(idx.list(Some(&rp("nope"))).is_empty());
    assert_eq!(idx.read(&rp("src/a/x.json")).unwrap(), b"{}");
    let mut out = Vec::new();
    assert_eq!(idx.stream(&rp("src/a/x.json"), &mut out).unwrap(), 2);
    assert_eq!(
        idx.hash(&rp("src/a/x.json")).unwrap(),
        *blake3::hash(b"{}").as_bytes()
    );
    // unindexed -> NotFound, even though it exists on disk
    for p in ["other/f.txt", ".git/f.txt", "src/node_modules/f.txt"] {
        assert_eq!(
            idx.read(&rp(p)).unwrap_err().kind(),
            ErrorKind::NotFound,
            "{p}"
        );
        assert!(!idx.exists(&rp(p)));
    }
    // without extra excludes the pack dirs are indexed
    let (idx, _) = Index::build(d.path(), &[], None);
    assert!(idx.exists(&rp("packs/BP/f.txt")));
}

#[test]
fn fast_path_reuses_hash_changed_file_rehashes() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    let (k, old) = (rp("a.txt"), *blake3::hash(b"aaaa").as_bytes());
    fs::write(&f, "aaaa").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let (i1, _) = Index::build(d.path(), &[], None);
    assert_eq!(i1.hash(&k).unwrap(), old);

    // same size + mtime + file id, different bytes: the fast path trusts the index
    fs::write(&f, "bbbb").unwrap();
    fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let (i2, _) = Index::build(d.path(), &[], Some(&i1));
    assert_eq!(
        i2.hash(&k).unwrap(),
        old,
        "hash must be reused without reading"
    );
    let (full, _) = Index::build(d.path(), &[], None);
    assert_eq!(full.hash(&k).unwrap(), *blake3::hash(b"bbbb").as_bytes());

    // different size: rehashed
    fs::write(&f, "cc").unwrap();
    let (i3, _) = Index::build(d.path(), &[], Some(&i1));
    assert_eq!(i3.hash(&k).unwrap(), *blake3::hash(b"cc").as_bytes());
    // changed after indexing: an error that is not NotFound
    assert_eq!(i1.read(&k).unwrap_err().kind(), ErrorKind::InvalidData);
    assert_eq!(
        i1.stream(&k, &mut Vec::new()).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    // same size and mtime but different bytes (stat matches): the hash check catches it
    fs::write(&f, "aaab").unwrap();
    fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(i1.read(&k).unwrap_err().kind(), ErrorKind::InvalidData);
    assert_eq!(
        i1.stream(&k, &mut Vec::new()).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn nfd_names_are_keyed_nfc() {
    let d = tempfile::tempdir().unwrap();
    let nfd = "cafe\u{301}.txt";
    if fs::write(d.path().join(nfd), "hi").is_err() {
        return;
    }
    let (idx, _) = Index::build(d.path(), &[], None);
    assert_eq!(idx.files()[0].as_str(), "caf\u{e9}.txt");
    assert_eq!(idx.read(&rp(nfd)).unwrap(), b"hi");
}

#[cfg(windows)]
#[test]
fn junction_is_skipped_and_unreadable() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "s").unwrap();
    let root = d.path().join("proj");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/ok.txt"), "ok").unwrap();
    let link = root.join("src").join("link");
    let st = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(st.status.success(), "{st:?}");
    assert!(link.join("secret.txt").exists());

    let (idx, diags) = Index::build(&root, &[], None);
    assert_eq!(idx.files().len(), 1);
    assert!(
        diags.iter().any(|d| d.code == Code::ReparsePointSkipped),
        "{diags:?}"
    );
    let e = idx.read(&rp("src/link/secret.txt")).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::NotFound);
    assert!(!idx.exists(&rp("src/link")));
}

#[cfg(windows)]
#[test]
fn indexed_file_replaced_by_link_is_changed_not_notfound() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "s").unwrap();
    fs::write(d.path().join("a.txt"), "a").unwrap();
    let (idx, _) = Index::build(d.path(), &[], None);
    fs::remove_file(d.path().join("a.txt")).unwrap();
    // file symlinks need privilege or developer mode: skip silently if unavailable
    if std::os::windows::fs::symlink_file(outside.path().join("secret.txt"), d.path().join("a.txt"))
        .is_ok()
    {
        assert_eq!(
            idx.read(&rp("a.txt")).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_is_skipped_and_unreadable() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "s").unwrap();
    let root = d.path().join("proj");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/ok.txt"), "ok").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("src/link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("src/f.txt")).unwrap();

    let (idx, diags) = Index::build(&root, &[], None);
    assert_eq!(idx.files().len(), 1);
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.code == Code::ReparsePointSkipped)
            .count(),
        2,
        "{diags:?}"
    );
    assert_eq!(
        idx.read(&rp("src/link/secret.txt")).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        idx.read(&rp("src/f.txt")).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn indexed_file_removed_is_changed_not_notfound() {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join("a.txt"), "a").unwrap();
    let (idx, _) = Index::build(d.path(), &[], None);
    fs::remove_file(d.path().join("a.txt")).unwrap();
    assert_eq!(
        idx.read(&rp("a.txt")).unwrap_err().kind(),
        ErrorKind::InvalidData
    );
}

#[test]
fn excludes_are_case_insensitive() {
    let d = tempfile::tempdir().unwrap();
    for p in [".GIT", "Node_Modules", "Packs/bp"] {
        fs::create_dir_all(d.path().join(p)).unwrap();
        fs::write(d.path().join(p).join("f.txt"), "x").unwrap();
    }
    let (idx, _) = Index::build(d.path(), &[rp("packs/BP")], None);
    // ALWAYS_EXCLUDED is case-insensitive everywhere; extra excludes only on windows/macos
    let n = idx.files().len();
    assert_eq!(
        n,
        if cfg!(any(windows, target_os = "macos")) {
            0
        } else {
            1
        }
    );
}
