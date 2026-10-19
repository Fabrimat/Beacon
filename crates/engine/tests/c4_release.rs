//! Release builds (4.2.1): content hashes, version decisions, lock records.

mod b1_common;
mod c3_common;
use b1_common::{errors, has, opts, session, write};
use beacon_engine::build::{BuildOutcome, Bump, ReleaseOptions};
use beacon_engine::diag::Code;
use beacon_engine::event::PackVersion;
use beacon_engine::model::Edition;
use c3_common::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn run(p: &Path, target: Option<&str>, rel: Option<ReleaseOptions>, frozen: bool) -> BuildOutcome {
    let mut o = opts(p, target);
    o.all = target.is_none(); // no target = every target
    o.release = rel;
    o.frozen = frozen;
    session(o, p, &Arc::new(AtomicUsize::new(0))).build()
}

fn pack(p: &Path, target: &str) -> BuildOutcome {
    run(p, Some(target), Some(ReleaseOptions::default()), false)
}

fn rel(bump: Option<Bump>, set: Option<[u32; 3]>, only: Option<&str>) -> Option<ReleaseOptions> {
    Some(ReleaseOptions {
        bump,
        set_version: set,
        only: only.map(String::from).into_iter().collect(),
    })
}

fn ver<'a>(r: &'a BuildOutcome, key: &str) -> &'a PackVersion {
    r.report
        .packs
        .iter()
        .find(|v| v.pack == key)
        .unwrap_or_else(|| panic!("{key} in {:?}", r.report.packs))
}

fn v(r: &BuildOutcome, key: &str) -> [u32; 3] {
    ver(r, key).version
}

fn lock_bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p.join("beacon.lock.json")).unwrap()
}

fn ok(r: &BuildOutcome) {
    assert!(errors(r).is_empty(), "{:?}", r.diagnostics);
}

fn edit_rp(p: &Path) {
    write(p, "src/game/items/gem.png", "PNG2");
}

fn edit_bp(p: &Path) {
    write(p, "src/game/functions/start.mcfunction", "say changed\n");
}

fn set_lock(p: &Path, f: impl FnOnce(&mut Value)) {
    let mut l: Value = serde_json::from_slice(&lock_bytes(p)).unwrap();
    f(&mut l);
    std::fs::write(
        p.join("beacon.lock.json"),
        serde_json::to_vec_pretty(&l).unwrap(),
    )
    .unwrap();
}

fn manual(p: &Path) {
    let c = std::fs::read_to_string(p.join("config.json")).unwrap();
    let c = c.replace("\"sources\"", "\"versionPolicy\": \"manual\", \"sources\"");
    write(p, "config.json", &c);
}

fn manifest_version(z: &Zip, name: &str) -> Value {
    serde_json::from_slice::<Value>(&z[name]).unwrap()["header"]["version"].clone()
}

#[test]
fn rp_uuid_is_not_content() {
    let p = multipack();
    let h = ver(&pack(p.path(), "addon"), "game/RP").hash.clone();
    set_lock(p.path(), |l| {
        l["outputs"]["game"]["editions"]["bedrock"]["RP"]["header"] =
            "11111111-1111-4111-8111-111111111111".into();
    });
    let r = pack(p.path(), "addon");
    ok(&r);
    assert_eq!(ver(&r, "game/RP").hash, h);
    assert_eq!(v(&r, "game/RP"), [1, 0, 0]);
}

#[test]
fn second_pack_changes_nothing() {
    let p = multipack();
    let first = pack(p.path(), "template");
    ok(&first);
    assert!(first.report.packs.iter().all(|x| !x.bumped));
    let l = lock_bytes(p.path());
    let second = pack(p.path(), "template");
    ok(&second);
    assert_eq!(lock_bytes(p.path()), l);
    assert!(
        second
            .report
            .packs
            .iter()
            .all(|x| !x.bumped && x.version == [1, 0, 0])
    );
}

#[test]
fn rp_change_bumps_rp_bp_and_template() {
    let p = multipack();
    ok(&pack(p.path(), "template"));
    edit_rp(p.path());
    let r = pack(p.path(), "template");
    ok(&r);
    for k in ["game/RP", "game/BP", "world:worlds/main"] {
        assert_eq!((v(&r, k), ver(&r, k).bumped), ([1, 0, 1], true), "{k}");
    }
    assert_eq!(v(&r, "lib/RP"), [1, 0, 0]);
    assert_eq!(v(&r, "lib/BP"), [1, 0, 0]);
    let l = lock(p.path());
    assert_eq!(
        l["outputs"]["game"]["editions"]["bedrock"]["RP"]["release"]["version"],
        json!([1, 0, 1])
    );
}

#[test]
fn bp_change_leaves_rp() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    edit_bp(p.path());
    let r = pack(p.path(), "addon");
    ok(&r);
    assert_eq!(v(&r, "game/RP"), [1, 0, 0]);
    assert_eq!(v(&r, "game/BP"), [1, 0, 1]);
}

#[test]
fn bump_all_and_named() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    let r = run(
        p.path(),
        Some("addon"),
        rel(Some(Bump::Minor), None, None),
        false,
    );
    ok(&r);
    assert!(
        r.report
            .packs
            .iter()
            .all(|x| x.version == [1, 1, 0] && x.bumped)
    );
    // only the named pack; its BP follows through the dependency (patch)
    let r = run(
        p.path(),
        Some("addon"),
        rel(Some(Bump::Major), None, Some("game/RP")),
        false,
    );
    ok(&r);
    assert_eq!(v(&r, "game/RP"), [2, 0, 0]);
    assert_eq!(v(&r, "game/BP"), [1, 1, 1]);
    assert_eq!(v(&r, "lib/RP"), [1, 1, 0]);
}

#[test]
fn bump_unchanged_explicit_pack() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    let r = run(
        p.path(),
        Some("addon"),
        rel(Some(Bump::Minor), None, Some("lib/BP")),
        false,
    );
    ok(&r);
    assert_eq!(
        (v(&r, "lib/BP"), ver(&r, "lib/BP").bumped),
        ([1, 1, 0], true)
    );
    for k in ["game/RP", "game/BP", "lib/RP"] {
        assert_eq!(v(&r, k), [1, 0, 0], "{k}");
    }
}

#[test]
fn manual_policy_stale_blocks_everything() {
    let p = multipack();
    manual(p.path());
    ok(&pack(p.path(), "addon"));
    edit_bp(p.path());
    let l = lock_bytes(p.path());
    std::fs::remove_dir_all(p.path().join("out")).unwrap();
    let r = pack(p.path(), "addon");
    assert!(has(&r, Code::PackVersionStale), "{:?}", r.diagnostics);
    assert!(r.report.artifacts.is_empty() && r.report.packs.is_empty());
    assert!(!p.path().join("out").exists());
    assert_eq!(lock_bytes(p.path()), l);
    // the fix the diagnostic names
    let r = run(
        p.path(),
        Some("addon"),
        rel(None, Some([1, 0, 5]), Some("game/BP")),
        false,
    );
    ok(&r);
    assert_eq!(v(&r, "game/BP"), [1, 0, 5]);
}

#[test]
fn set_version_is_monotonic() {
    let p = multipack();
    manual(p.path());
    ok(&run(
        p.path(),
        Some("addon"),
        rel(None, Some([2, 0, 0]), None),
        false,
    ));
    let l = lock_bytes(p.path());
    let r = run(
        p.path(),
        Some("addon"),
        rel(None, Some([1, 9, 9]), Some("game/RP")),
        false,
    );
    assert!(
        has(&r, Code::PackVersionNotMonotonic),
        "{:?}",
        r.diagnostics
    );
    assert_eq!(lock_bytes(p.path()), l);
}

#[test]
fn frozen_blocks_a_needed_bump() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    edit_rp(p.path());
    let l = lock_bytes(p.path());
    let r = run(
        p.path(),
        Some("addon"),
        Some(ReleaseOptions::default()),
        true,
    );
    assert!(has(&r, Code::LockfileFrozen), "{:?}", r.diagnostics);
    assert!(r.report.artifacts.is_empty());
    assert_eq!(lock_bytes(p.path()), l);
}

#[test]
fn build_reports_behind_and_uses_last_release() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    edit_bp(p.path());
    let l = lock_bytes(p.path());
    let r = build(p.path(), "addon", "b");
    ok(&r);
    assert!(has(&r, Code::PackReleaseBehind));
    assert!(r.report.packs.is_empty());
    assert_eq!(lock_bytes(p.path()), l);
    let (_, z) = artifact(&r);
    assert_eq!(
        manifest_version(&z, "beacon-multipack game-BP/manifest.json"),
        json!([1, 0, 0])
    );
    // released again: no warning
    ok(&pack(p.path(), "addon"));
    assert!(!has(
        &build(p.path(), "addon", "c"),
        Code::PackReleaseBehind
    ));
}

#[test]
fn dev_builds_never_touch_versions() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    edit_rp(p.path());
    let l = lock_bytes(p.path());
    let r = build(p.path(), "dev", "d");
    ok(&r);
    assert!(!has(&r, Code::PackReleaseBehind));
    assert_eq!(lock_bytes(p.path()), l);
    // a release over every target decides nothing for the dev targets
    let r = run(p.path(), None, Some(ReleaseOptions::default()), false);
    ok(&r);
    assert_eq!(v(&r, "game/RP"), [1, 0, 1]);
}

#[test]
fn static_outputs_are_never_bumped() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    write(
        p.path(),
        "vendor/pack/BP/functions/vendor/hello.mcfunction",
        "say changed\n",
    );
    let r = pack(p.path(), "addon");
    ok(&r);
    assert!(r.report.packs.iter().all(|x| !x.pack.starts_with("vendor")));
    let (_, z) = artifact(&r);
    // the vendor manifest is passed through untouched, version and all
    let src = std::fs::read(p.path().join("vendor/pack/BP/manifest.json")).unwrap();
    assert_eq!(z["beacon-multipack vendor-BP/manifest.json"], src);
    assert!(lock(p.path())["outputs"].get("vendor").is_none());
}

#[test]
fn editions_version_independently() {
    let p = multipack();
    ok(&run(p.path(), None, Some(ReleaseOptions::default()), false));
    let c = std::fs::read_to_string(p.path().join("config.json")).unwrap();
    write(
        p.path(),
        "config.json",
        &c.replace("\"1.21.50\"", "\"1.21.60\""),
    );
    let r = run(p.path(), None, Some(ReleaseOptions::default()), false);
    ok(&r);
    for x in &r.report.packs {
        assert_eq!(
            x.bumped,
            x.edition == Edition::Education && !x.pack.starts_with("world:"),
            "{x:?}"
        );
    }
    let l = lock(p.path());
    let ed = &l["outputs"]["game"]["editions"];
    assert_eq!(ed["bedrock"]["BP"]["release"]["version"], json!([1, 0, 0]));
    assert_eq!(
        ed["education"]["BP"]["release"]["version"],
        json!([1, 0, 1])
    );
}

#[test]
fn manifests_match_world_pack_lists() {
    let p = multipack();
    ok(&pack(p.path(), "template"));
    edit_rp(p.path());
    let (_, z) = artifact(&pack(p.path(), "template"));
    let j = |n: &str| serde_json::from_slice::<Value>(&z[n]).unwrap();
    let mut seen = 0;
    for (list, top) in [
        ("world_behavior_packs.json", "behavior_packs"),
        ("world_resource_packs.json", "resource_packs"),
    ] {
        for e in j(list).as_array().unwrap() {
            let m = z
                .keys()
                .filter(|k| k.starts_with(top) && k.ends_with("/manifest.json"))
                .filter_map(|k| serde_json::from_slice::<Value>(&z[k]).ok()) // static packs are JSONC
                .find(|m| m["header"]["uuid"] == e["pack_id"]);
            let Some(m) = m else { continue };
            assert_eq!(m["header"]["version"], e["version"], "{}", e["pack_id"]);
            assert_eq!(m["modules"][0]["version"], e["version"]);
            seen += 1;
        }
    }
    assert!(seen >= 4, "{seen}");
    let rp = j("resource_packs/beacon-multipack game-RP/manifest.json");
    let bp = j("behavior_packs/beacon-multipack game-BP/manifest.json");
    assert_eq!(bp["dependencies"][0]["version"], rp["header"]["version"]);
    assert_eq!(rp["header"]["version"], json!([1, 0, 1]));
    // the template's own manifest carries its settled version
    assert_eq!(manifest_version(&z, "manifest.json"), json!([1, 0, 1]));
    let w = &lock(p.path())["worlds"]["worlds/main"]["editions"]["bedrock"];
    assert_eq!(w["release"]["version"], json!([1, 0, 1]));
}

#[test]
fn skin_pack_is_versioned() {
    let p = multipack();
    let r = pack(p.path(), "skins");
    ok(&r);
    assert_eq!(v(&r, "skins:skins"), [1, 0, 0]);
    write(
        p.path(),
        "skins/texts/en_US.lang",
        "skin.beacon_multipack.steve_blue=Blue2\n",
    );
    let r = pack(p.path(), "skins");
    ok(&r);
    assert_eq!(v(&r, "skins:skins"), [1, 0, 1]);
    let (_, z) = artifact(&r);
    assert_eq!(manifest_version(&z, "manifest.json"), json!([1, 0, 1]));
}

#[test]
fn unknown_pack_id_is_an_error_listing_valid_ids() {
    let p = multipack();
    ok(&pack(p.path(), "template"));
    edit_rp(p.path());
    let l = lock_bytes(p.path());
    std::fs::remove_dir_all(p.path().join("out")).unwrap();
    let r = run(
        p.path(),
        Some("template"),
        rel(Some(Bump::Minor), None, Some("game/XP")),
        false,
    );
    let msg = &errors(&r)[0].message;
    assert!(
        msg.contains("game/XP") && msg.contains("game/RP") && msg.contains("world:worlds/main")
    );
    assert!(r.report.artifacts.is_empty() && !p.path().join("out").exists());
    assert_eq!(lock_bytes(p.path()), l);
    // a bare output id and a repeated pack are fine
    let mut o = rel(Some(Bump::Minor), None, Some("lib"));
    o.as_mut().unwrap().only.push("game/RP".into());
    let r = run(p.path(), Some("template"), o, false);
    ok(&r);
    assert_eq!(v(&r, "lib/BP"), [1, 1, 0]);
    assert_eq!(v(&r, "game/RP"), [1, 1, 0]);
}

#[test]
fn targets_with_different_content_for_one_entry_are_an_error() {
    let p = multipack();
    let c = std::fs::read_to_string(p.path().join("config.json")).unwrap();
    let extra =
        r#""addon2": { "edition": "bedrock", "artifact": "mcaddon", "targetVersion": "1.21.60" },"#;
    write(
        p.path(),
        "config.json",
        &c.replace("\"addon\": {", &format!("{extra} \"addon\": {{")),
    );
    let r = run(p.path(), None, Some(ReleaseOptions::default()), false);
    let e = errors(&r);
    assert!(
        e[0].message.contains("different content"),
        "{:?}",
        r.diagnostics
    );
    assert!(r.report.packs.is_empty());
    assert!(
        !p.path().join("beacon.lock.json").exists()
            || !String::from_utf8(lock_bytes(p.path()))
                .unwrap()
                .contains("\"release\"")
    );
}

fn shared_editions(p: &Path) {
    let c = std::fs::read_to_string(p.join("config.json")).unwrap();
    let extra = r#""addon-edu": { "edition": "education", "artifact": "mcaddon", "targetVersion": "1.21.50" },"#;
    let c = c
        .replace("\"addon\": {", &format!("{extra} \"addon\": {{"))
        .replace("\"sources\"", "\"sharedEditionUuids\": true, \"sources\"");
    write(p, "config.json", &c);
}

#[test]
fn shared_uuids_keep_one_release_per_edition() {
    let p = multipack();
    shared_editions(p.path());
    for t in ["addon", "addon-edu"] {
        ok(&pack(p.path(), t));
    }
    let l = lock_bytes(p.path());
    for t in ["addon", "addon-edu", "addon", "addon-edu"] {
        let r = pack(p.path(), t);
        ok(&r);
        assert!(
            r.report.packs.iter().all(|x| !x.bumped),
            "{t}: {:?}",
            r.report.packs
        );
        assert_eq!(lock_bytes(p.path()), l, "{t}");
    }
    // the two editions' packs hash differently, and each is recorded under its edition
    let g = &lock(p.path())["outputs"]["game"]["shared"]["BP"];
    assert!(
        g.get("release").is_none()
            && g["releases"]["bedrock"]["hash"] != g["releases"]["education"]["hash"]
    );
}

#[test]
fn shared_uuids_all_targets_are_stable() {
    let p = multipack();
    shared_editions(p.path());
    let all = |p: &Path| run(p, None, Some(ReleaseOptions::default()), false);
    ok(&all(p.path()));
    let l = lock_bytes(p.path());
    let r = all(p.path());
    ok(&r);
    assert!(r.report.packs.iter().all(|x| !x.bumped));
    assert_eq!(lock_bytes(p.path()), l);
}

#[test]
fn dev_targets_are_not_part_of_a_release() {
    let p = multipack();
    ok(&pack(p.path(), "addon"));
    let l = lock_bytes(p.path());
    std::fs::remove_dir_all(p.path().join("out")).unwrap();
    let r = pack(p.path(), "dev");
    assert!(errors(&r)[0].message.contains("no release targets"));
    assert!(r.report.artifacts.is_empty() && !p.path().join("out").exists());
    assert_eq!(lock_bytes(p.path()), l);
    // `--all` releases the archives and writes nothing for the dev targets
    let r = run(p.path(), None, Some(ReleaseOptions::default()), false);
    ok(&r);
    let names: Vec<String> = std::fs::read_dir(p.path().join("out"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| n.starts_with("beacon-multipack-")),
        "{names:?}"
    );
}

fn with_deps(p: &Path, mode: &str) {
    let c = std::fs::read_to_string(p.join("config.json")).unwrap();
    let c = c.replace(
        "\"packMeta\": {",
        &format!("\"packMeta\": {{\"packDependencies\": \"{mode}\","),
    );
    assert!(c.contains("packDependencies"));
    write(p, "config.json", &c);
}

fn game_manifest(z: &Zip, pk: &str) -> Value {
    serde_json::from_slice(&z[&format!("beacon-multipack game-{pk}/manifest.json")]).unwrap()
}

#[test]
fn rp_bp_release_keeps_rp_dependency_on_the_bp_version() {
    let p = multipack();
    with_deps(p.path(), "rp-bp");
    ok(&pack(p.path(), "addon"));
    let l = lock_bytes(p.path());

    edit_bp(p.path());
    let r = pack(p.path(), "addon");
    ok(&r);
    assert_eq!(
        (v(&r, "game/BP"), ver(&r, "game/BP").bumped),
        ([1, 0, 1], true)
    );
    // the RP manifest changed with the BP version, so the RP bumps too
    assert_eq!(
        (v(&r, "game/RP"), ver(&r, "game/RP").bumped),
        ([1, 0, 1], true)
    );
    let (_, z) = artifact(&r);
    let (bp, rp) = (game_manifest(&z, "BP"), game_manifest(&z, "RP"));
    assert_eq!(bp["header"]["version"], json!([1, 0, 1]));
    assert_eq!(rp["header"]["version"], json!([1, 0, 1]));
    assert_eq!(rp["dependencies"][0]["uuid"], bp["header"]["uuid"]);
    assert_eq!(rp["dependencies"][0]["version"], bp["header"]["version"]);
    assert_ne!(lock_bytes(p.path()), l);

    let l = lock_bytes(p.path());
    let r = pack(p.path(), "addon");
    ok(&r);
    assert!(r.report.packs.iter().all(|x| !x.bumped));
    assert_eq!(lock_bytes(p.path()), l);
}

#[test]
fn mutual_release_bumps_both_packs_once_and_is_stable() {
    let p = multipack();
    with_deps(p.path(), "mutual");
    ok(&pack(p.path(), "addon"));
    let mut expect = [1, 0, 0];
    for edit in [edit_bp, edit_rp] {
        expect[2] += 1;
        edit(p.path());
        let r = pack(p.path(), "addon");
        ok(&r);
        for k in ["game/BP", "game/RP"] {
            assert_eq!((v(&r, k), ver(&r, k).bumped), (expect, true), "{k}");
        }
        let (_, z) = artifact(&r);
        let (bp, rp) = (game_manifest(&z, "BP"), game_manifest(&z, "RP"));
        assert_eq!(rp["dependencies"][0]["uuid"], bp["header"]["uuid"]);
        assert_eq!(rp["dependencies"][0]["version"], json!(expect));
        assert_eq!(bp["dependencies"][0]["uuid"], rp["header"]["uuid"]);
        assert_eq!(bp["dependencies"][0]["version"], json!(expect));
        assert_eq!(bp["header"]["version"], json!(expect));
        assert_eq!(rp["header"]["version"], json!(expect));
        // no change: nothing bumps, the lock is byte-identical
        let l = lock_bytes(p.path());
        let r = pack(p.path(), "addon");
        ok(&r);
        assert!(r.report.packs.iter().all(|x| !x.bumped));
        assert_eq!(lock_bytes(p.path()), l);
    }
}
