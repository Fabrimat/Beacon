//! Skin pack artifact (`.mcpack`).

mod b1_common;
mod c3_common;
use b1_common::{errors, has, write};
use beacon_engine::diag::Code;
use c3_common::*;
use serde_json::{Value, json};

fn json_of(z: &Zip, n: &str) -> Value {
    serde_json::from_slice(&z[n]).unwrap()
}

#[test]
fn manifest_passthrough_and_derived_files() {
    let p = multipack();
    let r = build(p.path(), "skins", "o");
    let (path, z) = artifact(&r);
    assert!(path.ends_with("beacon-multipack-skins.mcpack"));
    let ids = &lock(p.path())["skins"]["skins"]["editions"]["bedrock"];
    assert_eq!(
        json_of(&z, "manifest.json"),
        json!({
            "format_version": 2,
            "header": {
                "name": "pack.name", "description": "pack.description",
                "uuid": ids["header"], "version": [1, 0, 0],
            },
            "modules": [{"type": "skin_pack", "uuid": ids["module"], "version": [1, 0, 0]}],
        })
    );
    let src = p.path().join("skins");
    assert_eq!(
        z["skins.json"],
        std::fs::read(src.join("skins.json")).unwrap()
    );
    assert_eq!(z["skin.png"], std::fs::read(src.join("skin.png")).unwrap());
    let lang = String::from_utf8(z["texts/en_US.lang"].clone()).unwrap();
    assert!(lang.starts_with(
        "skinpack.beacon_multipack=Beacon Multipack\nskin.beacon_multipack.steve_blue=Blue\n"
    ));
    assert!(lang.contains("pack.name=Beacon Multipack\n"));
    assert_eq!(json_of(&z, "texts/languages.json"), json!(["en_US"]));
    assert_eq!(z.len(), 5, "{:?}", z.keys());
}

#[test]
fn base_key_is_added_when_missing_and_languages_are_derived() {
    let p = multipack();
    write(
        p.path(),
        "skins/texts/en_US.lang",
        "skin.beacon_multipack.steve_blue=Blue",
    );
    write(p.path(), "skins/texts/de_DE.lang", "x=y\n");
    write(p.path(), "skins/texts/languages.json", r#"["zz_ZZ"]"#);
    let (_, z) = artifact(&build(p.path(), "skins", "o"));
    let lang = String::from_utf8(z["texts/en_US.lang"].clone()).unwrap();
    assert!(lang.starts_with(
        "skin.beacon_multipack.steve_blue=Blue\nskinpack.beacon_multipack=Beacon Multipack\n"
    ));
    assert_eq!(
        json_of(&z, "texts/languages.json"),
        json!(["de_DE", "en_US"])
    );
}

#[test]
fn deterministic() {
    let p = multipack();
    let (a, b) = (build(p.path(), "skins", "a"), build(p.path(), "skins", "b"));
    assert_eq!(
        std::fs::read(artifact(&a).0).unwrap(),
        std::fs::read(artifact(&b).0).unwrap()
    );
}

#[test]
fn invalid_skins_json_is_diagnosed() {
    let p = multipack();
    write(
        p.path(),
        "skins/skins.json",
        r#"{"localization_name":"x","skins":[{"texture":"nope.png"}]}"#,
    );
    let r = build(p.path(), "skins", "o");
    assert!(has(&r, Code::StaticPackInvalid) && r.report.artifacts.is_empty());
    assert!(errors(&r)[0].message.contains("nope.png"));
    write(p.path(), "skins/skins.json", r#"{"skins":[]}"#);
    let r = build(p.path(), "skins", "o");
    assert!(errors(&r)[0].message.contains("localization_name"));
    write(p.path(), "skins/skins.json", "{");
    assert!(has(&build(p.path(), "skins", "o"), Code::StaticPackInvalid));
}
