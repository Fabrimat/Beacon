//! Dev deploy into `BEACON_COM_MOJANG` (own test binary: it sets a process env var).

mod b1_common;
use b1_common::*;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

#[test]
fn dev_target_deploys_to_com_mojang() {
    let p = project(&[]);
    let mojang = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary; nothing else reads the env concurrently.
    unsafe { std::env::set_var("BEACON_COM_MOJANG", mojang.path()) };
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(p.path(), Some("dev"));
    o.out = None;
    let r = session(o, p.path(), &runs).build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let bp: Vec<_> = std::fs::read_dir(mojang.path().join("development_behavior_packs"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(bp.len(), 1);
    let (name, rest) = bp[0].split_once('-').unwrap();
    assert_eq!(name, "b1 test");
    assert_eq!(rest.len(), "0123abcd BP".len(), "{rest}");
    assert!(rest.ends_with(" BP"));
    let rp_dir = mojang
        .path()
        .join("development_resource_packs")
        .join(bp[0].replace(" BP", " RP"));
    assert!(rp_dir.join("manifest.json").is_file());
}
