mod a2_common;
use a2_common::{FakeVfs, rp};
use beacon_engine::archive::write_zip;
use beacon_engine::model::Body;
use beacon_engine::path::RelPath;
use serde_json::json;
use std::io::Read;
use zip::{CompressionMethod, ZipArchive};

#[test]
fn deterministic_sorted_stored_vs_deflated() {
    let d = tempfile::tempdir().unwrap();
    let png: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
    let vfs = FakeVfs::with(&[
        ("a/t.png", &png),
        ("a/s.ogg", b"OggS"),
        ("a/b.json", b"{ // keep\n}"),
    ]);
    let bodies = [
        Body::Json(json!({"k": [1, 2]})),
        Body::Passthrough(rp("a/t.png")),
        Body::Passthrough(rp("a/s.ogg")),
        Body::Text("x=y\n".repeat(100)),
        Body::Passthrough(rp("a/b.json")),
    ];
    let names = [
        "manifest.json",
        "textures/t.png",
        "sounds/s.ogg",
        "texts/en_US.lang",
        "b.json",
    ];
    let entries: Vec<(RelPath, &Body)> =
        names.iter().zip(&bodies).map(|(n, b)| (rp(n), b)).collect();
    let (o1, o2) = (d.path().join("1.mcpack"), d.path().join("2.mcpack"));
    write_zip(&o1, &entries, &vfs).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut rev = entries.clone();
    rev.reverse();
    write_zip(&o2, &rev, &vfs).unwrap();
    assert_eq!(std::fs::read(&o1).unwrap(), std::fs::read(&o2).unwrap());

    let mut z = ZipArchive::new(std::fs::File::open(&o1).unwrap()).unwrap();
    let got: Vec<String> = z.file_names().map(|n| n.unwrap().into_owned()).collect();
    let mut sorted = got.clone();
    sorted.sort();
    assert_eq!(got, sorted);
    assert_eq!(got.len(), 5);
    for n in &got {
        let mut f = z.by_name(n).unwrap();
        let stored = n.ends_with(".png") || n.ends_with(".ogg");
        assert_eq!(f.compression() == CompressionMethod::Stored, stored, "{n}");
        assert_eq!(f.unix_mode(), Some(0o100644), "{n}");
        let t = f.last_modified().unwrap();
        assert_eq!((t.year(), t.month(), t.day()), (1980, 1, 1));
        if n == "textures/t.png" {
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            assert_eq!(b, png);
        }
    }
    assert!(z.comment().is_empty());
}
