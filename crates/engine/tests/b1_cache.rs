//! Generator cache keys (5.2): a cached result is reused only while everything the
//! generator observed is unchanged.

mod b1_common;
use b1_common::*;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const GEN: &str = "src/m/g.gen.ts";

/// Builds once (1 run), applies `edit`, builds again with the same session: did it rerun?
fn reran(src: &str, files: &[(&str, &str)], edit: impl FnOnce(&Path)) -> bool {
    let mut all = vec![(GEN, src)];
    all.extend_from_slice(files);
    let p = project(&all);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = session(opts(p.path(), Some("dev")), p.path(), &runs);
    let o = s.build();
    assert!(errors(&o).is_empty(), "{:?}", o.diagnostics);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    edit(p.path());
    let o = s.build();
    assert!(errors(&o).is_empty(), "{:?}", o.diagnostics);
    assert_eq!(o.generators_run, runs.load(Ordering::SeqCst) - 1);
    runs.load(Ordering::SeqCst) == 2
}

#[test]
fn edit_read_asset_reruns() {
    assert!(reran(
        "read src/m/sounds/a.ogg",
        &[("src/m/sounds/a.ogg", "OggS1")],
        |r| write(r, "src/m/sounds/a.ogg", "OggS-changed")
    ));
}

#[test]
fn new_file_in_listed_dir_reruns() {
    assert!(reran(
        "list src/m/sounds",
        &[("src/m/sounds/a.ogg", "OggS")],
        |r| write(r, "src/m/sounds/b.ogg", "OggS")
    ));
}

#[test]
fn deleted_file_in_listed_dir_reruns() {
    assert!(reran(
        "list src/m/sounds",
        &[
            ("src/m/sounds/a.ogg", "OggS"),
            ("src/m/sounds/b.ogg", "OggS")
        ],
        |r| std::fs::remove_file(r.join("src/m/sounds/b.ogg")).unwrap()
    ));
}

#[test]
fn created_negative_reruns() {
    assert!(reran("read src/m/opt.data.json", &[], |r| write(
        r,
        "src/m/opt.data.json",
        "{}"
    )));
}

#[test]
fn edited_generator_reruns() {
    assert!(reran("list src/m", &[], |r| write(
        r,
        GEN,
        "list src/m\nread src/m/x"
    )));
}

#[test]
fn unrelated_edit_does_not_rerun() {
    assert!(!reran(
        "read src/m/sounds/a.ogg\nlist src/m/sounds",
        &[("src/m/sounds/a.ogg", "OggS")],
        |r| {
            write(r, "src/m/items/sword.png", "PNG-changed");
            write(r, "src/m/items/axe.item.json", "{}");
        }
    ));
}

#[test]
fn uncacheable_always_reruns() {
    assert!(reran("uncacheable", &[], |_| {}));
}

#[test]
fn define_change_reruns() {
    assert!(reran("define FLAG", &[], |r| write(
        r,
        "config.json",
        &CONFIG.replace(r#""FLAG": 1"#, r#""FLAG": 2"#)
    )));
}

#[test]
fn disk_cache_survives_sessions_and_tolerates_corruption() {
    let p = project(&[(GEN, "list src/m")]);
    let runs = Arc::new(AtomicUsize::new(0));
    let count = || runs.load(Ordering::SeqCst);
    session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert_eq!(count(), 1);
    // a new session (new process) reuses the disk cache
    let o = session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert_eq!((count(), o.generators_run), (1, 0));
    // another target = another ctx = another key
    session(opts(p.path(), Some("addon")), p.path(), &runs).build();
    assert_eq!(count(), 2);
    session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert_eq!(count(), 2);
    // corrupt entries are a miss, never an error
    let dir = p.path().join(".beacon/cache");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 2);
    for f in files {
        std::fs::write(f.path(), "{ not json").unwrap();
    }
    let o = session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert_eq!(count(), 3);
    assert!(errors(&o).is_empty(), "{:?}", o.diagnostics);
    // and the rewritten entry is valid again
    session(opts(p.path(), Some("dev")), p.path(), &runs).build();
    assert_eq!(count(), 3);
}
