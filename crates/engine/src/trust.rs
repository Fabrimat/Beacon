//! Workstream A5: per-user project trust (5.5). Stored in the user profile
//! (`%LOCALAPPDATA%\beacon\trust.json`, `~/Library/Application Support/beacon/`,
//! `$XDG_DATA_HOME/beacon/`), never in the project. Key = canonical project path + blake3 of
//! `beacon.plugins` (changing plugins asks again). `BEACON_HOME` overrides the dir (tests).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

fn store_path() -> io::Result<PathBuf> {
    let base = match std::env::var_os("BEACON_HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("no user data directory"))?
            .join("beacon"),
    };
    Ok(base.join("trust.json"))
}

/// Canonical path string; lowercased on Windows (case-insensitive file systems).
fn key(root: &Path) -> io::Result<String> {
    let s = fs::canonicalize(root)?.to_string_lossy().into_owned();
    Ok(if cfg!(windows) { s.to_lowercase() } else { s })
}

fn plugin_hash(plugins: &[serde_json::Value]) -> String {
    blake3::hash(&serde_json::to_vec(plugins).unwrap_or_default())
        .to_hex()
        .to_string()
}

fn load(p: &Path) -> BTreeMap<String, String> {
    // A missing or corrupt store means "nothing trusted".
    fs::read(p)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn is_trusted(root: &Path, plugins: &[serde_json::Value]) -> bool {
    let (Ok(p), Ok(k)) = (store_path(), key(root)) else {
        return false;
    };
    load(&p).get(&k) == Some(&plugin_hash(plugins))
}

pub fn trust(root: &Path, plugins: &[serde_json::Value]) -> io::Result<()> {
    let p = store_path()?;
    let mut map = load(&p);
    map.insert(key(root)?, plugin_hash(plugins));
    fs::create_dir_all(p.parent().unwrap())?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(io::Error::other)?;
    let tmp = p.with_extension(format!("{:x}.tmp", u64::from_le_bytes(rnd)));
    fs::write(&tmp, serde_json::to_vec_pretty(&map)?)?;
    fs::rename(&tmp, &p).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}
