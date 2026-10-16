//! Workstream A5: per-user project trust (5.5). Stored in the user profile
//! (`%LOCALAPPDATA%\beacon\trust.json`, `~/Library/Application Support/beacon/`,
//! `$XDG_DATA_HOME/beacon/`), never in the project. Key = canonical project path + blake3 of
//! `beacon.plugins` (changing plugins asks again). `BEACON_HOME` overrides the dir (tests).

use std::path::Path;

pub fn is_trusted(_root: &Path, _plugins: &[serde_json::Value]) -> bool {
    todo!("A5")
}

pub fn trust(_root: &Path, _plugins: &[serde_json::Value]) -> std::io::Result<()> {
    todo!("A5")
}
