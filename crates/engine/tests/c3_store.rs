//! Marketplace submission zip.

mod b1_common;
mod c3_common;
use b1_common::has;
use beacon_engine::diag::Code;
use c3_common::*;

fn under(z: &Zip, prefix: &str) -> Zip {
    z.iter()
        .filter_map(|(k, v)| Some((k.strip_prefix(prefix)?.into(), v.clone())))
        .collect()
}

#[test]
fn layout_and_template_equals_standalone() {
    let p = multipack();
    let (path, z) = artifact(&build(p.path(), "store", "o"));
    assert!(path.ends_with("beacon-multipack-store.zip"));
    let (_, t) = artifact(&build(p.path(), "template", "t"));
    // same assembly; the store target shares the world's lock identity, so even the manifest matches
    assert_eq!(under(&z, "Content/world_template/"), t);
    let rest: Vec<&str> = z
        .keys()
        .map(String::as_str)
        .filter(|k| !k.starts_with("Content/world_template/"))
        .collect();
    assert_eq!(
        rest,
        ["Marketing Art/banner.png", "Store Art/thumbnail.png"]
    );
    let art = p.path().join("art");
    assert_eq!(
        z["Store Art/thumbnail.png"],
        std::fs::read(art.join("Store Art/thumbnail.png")).unwrap()
    );
}

#[test]
fn skin_pack_goes_under_content() {
    let p = multipack();
    let cfg = std::fs::read_to_string(p.path().join("config.json")).unwrap();
    let cfg = cfg.replace(r#""art": "art""#, r#""art": "art", "skins": "skins""#);
    std::fs::write(p.path().join("config.json"), cfg).unwrap();
    let (_, z) = artifact(&build(p.path(), "store", "o"));
    let (_, s) = artifact(&build(p.path(), "skins", "s"));
    assert_eq!(under(&z, "Content/skin_pack/"), s);
}

#[test]
fn missing_art_warns_but_writes() {
    let p = multipack();
    std::fs::remove_dir_all(p.path().join("art/Marketing Art")).unwrap();
    let r = build(p.path(), "store", "o");
    assert!(has(&r, Code::ArtMissing));
    let (_, z) = artifact(&r);
    assert!(z.contains_key("Store Art/thumbnail.png"));
    assert!(!z.keys().any(|k| k.starts_with("Marketing Art")));
}

#[test]
fn deterministic() {
    let p = multipack();
    let (a, b) = (build(p.path(), "store", "a"), build(p.path(), "store", "b"));
    assert_eq!(
        std::fs::read(artifact(&a).0).unwrap(),
        std::fs::read(artifact(&b).0).unwrap()
    );
}
