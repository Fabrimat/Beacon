//! Reads an npm tarball (gzip + tar) in memory. Nothing is extracted to disk.

use flate2::read::GzDecoder;
use std::collections::BTreeMap;
use std::io::Read;

const MAX_MEMBER: u64 = 32 << 20; // largest wanted member is ~1 MB (server index.d.ts)
const MAX_TOTAL: u64 = 256 << 20; // gzip-bomb guard over everything decompressed

/// The members for which `want(path)` holds, keyed by path without the leading `package/`
/// directory, in sorted order (never tar order). Rejects absolute paths, `..`, links.
pub fn read(tgz: &[u8], want: &dyn Fn(&str) -> bool) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut ar = tar::Archive::new(GzDecoder::new(tgz).take(MAX_TOTAL));
    let mut out = BTreeMap::new();
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let ty = entry.header().entry_type();
        let unsafe_path = name.starts_with(['/', '\\'])
            || name.as_bytes().get(1) == Some(&b':')
            || name.split(['/', '\\']).any(|c| c == "..");
        if ty.is_symlink() || ty.is_hard_link() || unsafe_path {
            return Err(format!("unsafe archive member {name:?}"));
        }
        let Some((_, rel)) = name.split_once('/') else {
            continue;
        };
        if !ty.is_file() || !want(rel) {
            continue;
        }
        let mut buf = Vec::new();
        (&mut entry)
            .take(MAX_MEMBER + 1)
            .read_to_end(&mut buf)
            .map_err(|e| e.to_string())?;
        if buf.len() as u64 > MAX_MEMBER {
            return Err(format!("{rel}: implausibly large"));
        }
        out.entry(rel.to_string()).or_insert(buf);
    }
    Ok(out)
}
