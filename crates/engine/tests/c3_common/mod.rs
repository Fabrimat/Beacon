#![allow(dead_code)]
//! Shared by the skin pack and marketplace tests: a copy of `fixtures/multipack`, builds and
//! zip reading.

use super::b1_common::{errors, opts, session};
use beacon_engine::build::BuildOutcome;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

pub fn multipack() -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let name = e.file_name();
            if ["dist", ".beacon"].contains(&name.to_str().unwrap()) {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(&name));
            } else {
                std::fs::copy(e.path(), to.join(&name)).unwrap();
            }
        }
    }
    let d = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/multipack"),
        d.path(),
    );
    d
}

pub fn build(p: &Path, target: &str, out: &str) -> BuildOutcome {
    let mut o = opts(p, Some(target));
    o.out = Some(p.join(out));
    session(o, p, &Arc::new(AtomicUsize::new(0))).build()
}

pub type Zip = BTreeMap<String, Vec<u8>>;

pub fn read_zip(path: &str) -> Zip {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().unwrap().into_owned(), b)
        })
        .collect()
}

/// The single artifact of a build without errors.
pub fn artifact(r: &BuildOutcome) -> (String, Zip) {
    assert!(errors(r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.report.artifacts.len(), 1, "{:?}", r.diagnostics);
    let p = r.report.artifacts[0].clone();
    let z = read_zip(&p);
    (p, z)
}

pub fn lock(p: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p.join("beacon.lock.json")).unwrap()).unwrap()
}
