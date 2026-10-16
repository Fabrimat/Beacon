//! B2a: `Index::update` / `Index::rescan` equal a full build; `emit_known` skips unchanged
//! outputs without touching the disk.

mod a2_common;
use a2_common::{FakeVfs, rp};
use beacon_engine::emit::emit_known;
use beacon_engine::model::Body;
use beacon_engine::path::{RelPath, Vfs};
use beacon_engine::vfs::Index;
use std::fs;
use std::path::{Path, PathBuf};

/// Same files, hashes and directory listings.
fn same(a: &Index, b: &Index, dirs: &[&str]) {
    assert_eq!(a.files(), b.files());
    for f in a.files() {
        assert_eq!(a.hash(f).unwrap(), b.hash(f).unwrap(), "{f}");
    }
    assert_eq!(a.list(None), b.list(None));
    for d in dirs {
        let d = rp(d);
        assert_eq!(a.exists(&d), b.exists(&d), "{d}");
        assert_eq!(a.list(Some(&d)), b.list(Some(&d)), "{d}");
    }
}

#[test]
fn update_equals_full_build_after_random_ops() {
    const DIRS: [&str; 6] = ["a", "a/b", "c", "c/d", ".beacon", "x/y"];
    for seed in 1..=6u64 {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n as u64) as usize
        };
        let (mut idx, _) = Index::build(root, &[], None);
        for round in 0..25 {
            let mut changed: Vec<PathBuf> = vec![];
            for _ in 0..1 + next(4) {
                let dir = root.join(DIRS[next(DIRS.len())]);
                let file = dir.join(format!("f{}.json", next(4)));
                match next(5) {
                    0 | 1 => {
                        fs::create_dir_all(&dir).unwrap();
                        fs::write(&file, format!("{{\"r\":{round}{}}}", next(1000))).unwrap();
                        changed.push(file);
                    }
                    2 => {
                        let _ = fs::remove_file(&file);
                        changed.push(file);
                    }
                    3 => {
                        let to = dir.join(format!("g{}.json", next(4)));
                        if fs::rename(&file, &to).is_ok() {
                            changed.extend([file, to]);
                        }
                    }
                    _ => {
                        // rename or delete a whole directory
                        let to = root.join(DIRS[next(DIRS.len())]).join("moved");
                        if next(2) == 0 {
                            let _ = fs::remove_dir_all(&dir);
                            changed.push(dir);
                        } else if fs::create_dir_all(to.parent().unwrap()).is_ok()
                            && fs::rename(&dir, &to).is_ok()
                        {
                            changed.extend([dir, to]);
                        }
                    }
                }
            }
            idx.update(root, &[], &changed);
            let (full, _) = Index::build(root, &[], None);
            let mut dirs: Vec<&str> = DIRS.to_vec();
            dirs.extend(["a/moved", "c/moved", "x", "a/b/moved", "a/moved/moved"]);
            same(&idx, &full, &dirs);
        }
        // a stat-only rescan agrees too
        let (re, _) = Index::rescan(root, &[], &idx);
        let (full, _) = Index::build(root, &[], None);
        same(&re, &full, &DIRS);
    }
}

#[test]
fn update_skips_excluded_and_outside_paths() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path();
    fs::create_dir_all(root.join("packs/BP")).unwrap();
    fs::write(root.join("packs/BP/o.json"), "{}").unwrap();
    fs::write(root.join("a.json"), "{}").unwrap();
    let ex: Vec<RelPath> = vec![rp("packs/BP")];
    let (mut idx, _) = Index::build(root, &ex, None);
    fs::write(root.join("packs/BP/o2.json"), "{}").unwrap();
    fs::create_dir_all(root.join(".beacon")).unwrap();
    fs::write(root.join(".beacon/x"), "1").unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("z.json"), "{}").unwrap();
    idx.update(
        root,
        &ex,
        &[
            root.join("packs/BP/o2.json"),
            root.join("packs/BP"),
            root.join(".beacon"),
            outside.path().join("z.json"),
        ],
    );
    assert_eq!(idx.files(), [&rp("a.json")]);
}

fn files(v: &[(&str, &Body)]) -> Vec<(RelPath, Body)> {
    v.iter().map(|(p, b)| (rp(p), (*b).clone())).collect()
}

#[test]
fn emit_known_touches_only_changes() {
    let d = tempfile::tempdir().unwrap();
    let vfs = FakeVfs::with(&[]);
    let (a, b) = (Body::Text("a\n".into()), Body::Text("b\n".into()));
    let run = |dir: &Path, list: &[(RelPath, Body)], known| {
        let f: Vec<(RelPath, &Body)> = list.iter().map(|(p, b)| (p.clone(), b)).collect();
        emit_known(dir, &f, &vfs, known)
    };
    let one = files(&[("a.txt", &a), ("b.txt", &b)]);
    let (r, diags, l1) = run(d.path(), &one, None);
    assert!((r.written, diags.len()) == (2, 0), "{diags:?}");
    fs::write(d.path().join("foreign.txt"), "mine").unwrap();

    // unchanged: nothing written, and a hand-deleted output is NOT noticed (documented)
    fs::remove_file(d.path().join("b.txt")).unwrap();
    let (r, _, l2) = run(d.path(), &one, Some(&l1));
    assert_eq!((r.written, r.unchanged, r.deleted), (0, 2, 0));
    assert!(!d.path().join("b.txt").exists());
    // ... and is restored by a full emit (no known ledger)
    let (r, _, _) = run(d.path(), &one, None);
    assert_eq!((r.written, r.unchanged), (1, 1));

    // changed + added + removed
    let a2 = Body::Text("a2\n".into());
    let two = files(&[("a.txt", &a2), ("c.txt", &b)]);
    let (r, diags, l3) = run(d.path(), &two, Some(&l2));
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!((r.written, r.deleted), (2, 1));
    assert!(!d.path().join("b.txt").exists());
    assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "a2\n");
    assert_eq!(
        fs::read_to_string(d.path().join("foreign.txt")).unwrap(),
        "mine"
    );
    let on_disk: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(d.path().join(".beacon-emit.json")).unwrap()).unwrap();
    assert_eq!(on_disk, l3);
}

/// Same size and mtime, different bytes: `update` and a strict build must see the new hash.
#[test]
fn same_stat_rewrite_is_rehashed() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("a.txt");
    fs::write(&f, "aaaa").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let (mut idx, _) = Index::build(d.path(), &[], None);
    let rewrite = |body: &str| {
        fs::write(&f, body).unwrap();
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    };
    rewrite("bbbb");
    idx.update(d.path(), &[], std::slice::from_ref(&f));
    let k = rp("a.txt");
    assert_eq!(idx.hash(&k).unwrap(), *blake3::hash(b"bbbb").as_bytes());
    assert_eq!(idx.read(&k).unwrap(), b"bbbb");

    // the strict rebuild used by the session's retry (no prev) agrees
    rewrite("cccc");
    let (strict, _) = Index::build(d.path(), &[], None);
    assert_eq!(strict.read(&k).unwrap(), b"cccc");
}
