//! `ensure` / `ApiIndex::load` against a seeded download cache (no network). One test: it sets
//! `BEACON_HOME`, which is process-global.

use beacon_api::{VersionPins, ensure_with};
use beacon_engine::api_index::{ApiIndex, cache_path};
use beacon_engine::diag::Code;
use beacon_fetch::{Confirm, Pinned};
use flate2::{Compression, write::GzEncoder};

fn tgz(pkg: &str, ver: &str, dts: &str) -> Vec<u8> {
    let mut b = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let manifest = format!(r#"{{"name":"{pkg}","version":"{ver}","license":"MIT"}}"#);
    for (name, data) in [("package.json", manifest.as_str()), ("index.d.ts", dts)] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, format!("package/{name}"), data.as_bytes())
            .unwrap();
    }
    b.into_inner().unwrap().finish().unwrap()
}

#[test]
fn ensure_builds_caches_and_load_reads_it() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this is the only test in this binary.
    unsafe { std::env::set_var("BEACON_HOME", home.path()) };
    let tools = tempfile::tempdir().unwrap();

    let body = tgz("@minecraft/server", "2.10.0", "export class World {}");
    let hash: &'static str = Box::leak(blake3::hash(&body).to_hex().to_string().into_boxed_str());
    let pin = Pinned {
        url: "https://registry.invalid/server-2.10.0.tgz",
        blake3: hash,
        size: body.len() as u64,
    };
    let vp = VersionPins {
        game_version: "9.9.9",
        packages: Box::leak(Box::new([("@minecraft/server", "2.10.0", pin)])),
    };

    // missing index: Err(ApiIndexMissing), a note
    let e = ApiIndex::load("9.9.9").unwrap_err();
    assert_eq!(e.code, Code::ApiIndexMissing);

    // not downloaded, not cached, not confirmed
    let e = ensure_with(&vp, tools.path(), Confirm::No).unwrap_err();
    assert_eq!(e.code, Code::DownloadNeedsConfirm);

    // the download cache already holds the pinned file: no network, even with Confirm::No
    std::fs::write(tools.path().join(hash), &body).unwrap();
    let path = ensure_with(&vp, tools.path(), Confirm::No).unwrap();
    assert_eq!(Some(path.clone()), cache_path("9.9.9"));
    assert!(path.starts_with(home.path()));
    let ix = ApiIndex::load("9.9.9").unwrap();
    assert!(
        ix.script_modules["@minecraft/server"][0]
            .exports
            .contains_key("World")
    );
    assert_eq!(
        ix.lookup("World", 5)[0].path,
        "@minecraft/server@2.10.0#World"
    );

    // a cached index is returned without touching the download cache
    std::fs::remove_file(tools.path().join(hash)).unwrap();
    assert_eq!(ensure_with(&vp, tools.path(), Confirm::No).unwrap(), path);

    // a tampered cache (wrong schema) is a miss again
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    v["schema"] = 0.into();
    std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
    assert_eq!(
        ApiIndex::load("9.9.9").unwrap_err().code,
        Code::ApiIndexMissing
    );

    // game versions are directory names: no traversal
    assert!(cache_path("../x").is_none());
}
