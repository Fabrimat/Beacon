//! Pinned esbuild downloads (9.2): the npm registry tarball of each platform package.
//! Regenerate when bumping `VERSION`: download
//! `https://registry.npmjs.org/@esbuild/<platform>/-/<platform>-<VERSION>.tgz` for every
//! platform and take the byte size and `blake3` hex of the file YOURSELF (e.g. `b3sum`); never
//! copy the registry's `integrity` field, that is the registry vouching for itself.

use beacon_fetch::Pinned;

pub const VERSION: &str = "0.28.2";

macro_rules! pin {
    ($p:literal, $size:literal, $hash:literal) => {
        (
            $p,
            Pinned {
                url: concat!(
                    "https://registry.npmjs.org/@esbuild/",
                    $p,
                    "/-/",
                    $p,
                    "-0.28.2.tgz"
                ),
                blake3: $hash,
                size: $size,
            },
        )
    };
}

const PINS: [(&str, Pinned); 6] = [
    pin!(
        "win32-x64",
        4829680,
        "6c487b88fbd71c3434514e302dc98631284465bd08ca1c4bae2cd234b5b27f6f"
    ),
    pin!(
        "win32-arm64",
        4323046,
        "970c760256d91bcea0a4096b17f9fc3e82c1e904b27d8defc3f1043f07ba04fc"
    ),
    pin!(
        "darwin-arm64",
        4434422,
        "388e69ab1e31b40d22383cc42c6528c9fbd5aa8da22ab9d9d00881adaea51d15"
    ),
    pin!(
        "darwin-x64",
        4802970,
        "bebdc3251304d9c0ebb3db1464fd2e03741ef539fa3c766563544d1285efe16a"
    ),
    pin!(
        "linux-x64",
        4741809,
        "037b25b415c65b933fa3a35e3acfdf957e92b193d87ef1aa9b1761ec41d26f53"
    ),
    pin!(
        "linux-arm64",
        4283627,
        "c4bc88a742e5cba3cf2ab673318f543b8add7614754444bc7e5f94057dc6ff33"
    ),
];

pub fn pin(platform: &str) -> Option<Pinned> {
    PINS.iter().find(|(p, _)| *p == platform).map(|(_, p)| *p)
}

/// npm's `<os>-<arch>` name of the host.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        a => a,
    };
    format!("{os}-{arch}")
}
