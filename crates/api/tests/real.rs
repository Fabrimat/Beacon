//! Maintainer check against the real pinned tarballs: `BEACON_API_TARBALLS=<dir holding the
//! <name>-<version>.tgz files> cargo test -p beacon-api --test real -- --ignored --nocapture`.

use beacon_api::{PINS, Tarball, build, to_json};

#[test]
#[ignore = "needs the real tarballs"]
fn real_pins_build() {
    let dir = std::env::var("BEACON_API_TARBALLS").expect("BEACON_API_TARBALLS");
    for vp in PINS {
        let files: Vec<_> = vp
            .packages
            .iter()
            .map(|(pkg, ver, pin)| {
                let name = pkg.rsplit('/').next().unwrap();
                let b = std::fs::read(format!("{dir}/{name}-{ver}.tgz")).unwrap();
                assert_eq!(b.len() as u64, pin.size, "{pkg}@{ver} size");
                (*pkg, *ver, pin.blake3, b)
            })
            .collect();
        let list: Vec<Tarball> = files
            .iter()
            .map(|(p, v, h, b)| (*p, *v, *h, b.as_slice()))
            .collect();
        let ix = build(vp.game_version, &list).unwrap();
        let json = to_json(&ix);
        println!("{} index.json: {} bytes", vp.game_version, json.len());
        for (m, v) in &ix.script_modules {
            for s in v {
                println!("{m}@{} exports={}", s.version, s.exports.len());
            }
        }
        for (k, v) in &ix.vanilla {
            println!("vanilla {k}: {}", v.len());
        }
        for (k, v) in &ix.components {
            println!("components {k}: {}", v.len());
        }
        println!("{:#?}", ix.lookup("World.getDimension", 3));
        println!("{:#?}", ix.lookup("minecraft:health", 3));
    }
}
