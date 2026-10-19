//! Contract (phase 4): the versioned API index (8.4a). One index per game version
//! (`targetVersion`, per target), built **locally** by `beacon-api` from hash-pinned npm
//! packages (`"license": "MIT"` in each tarball's `package.json`):
//!
//! - `@minecraft/server`, `@minecraft/server-ui` (and the other `@minecraft/*` script
//!   modules): exports, members, `@deprecated`, from their `.d.ts`;
//! - `@minecraft/vanilla-data`: vanilla block / item / entity / ... ids;
//! - `@minecraft/bedrock-schemas`: component names and descriptions per file kind (from the
//!   schemas, not full JSON Schema validation).
//!
//! Mojang/bedrock-samples `metadata/` (EULA, "all rights reserved") is **not** a source in
//! phase 4: commands and Molang grammars wait for a legal review (13).
//!
//! Cache: `<data dir>/beacon/api/<game version>/index.json` (`BEACON_HOME` replaces
//! `<data dir>/beacon`), canonical JSON, written atomically, read lazily on first use and
//! shared by `check`, the daemon (LSP completion/hover, MCP `api_lookup`) and
//! `beacon api --json` (ctxtruth-bedrock `bedrock/ids`). Never committed, never in the
//! project. A missing index is `ApiIndexMissing` (a note), never an error: checks that need
//! it are skipped.

use crate::diag::{Code, Diagnostic};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Bumped when the index layout changes (old caches are rebuilt).
pub const API_INDEX_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiIndex {
    pub schema: u32,
    /// e.g. `1.26.50`.
    pub game_version: String,
    /// Where every part came from (provenance, 8.4).
    pub sources: Vec<Provenance>,
    /// `@minecraft/server` -> versions available for this game version.
    pub script_modules: BTreeMap<String, Vec<ScriptModule>>,
    /// `block` | `item` | `entity` | `biome` | `effect` | ... -> vanilla ids.
    pub vanilla: BTreeMap<String, BTreeSet<String>>,
    /// File kind (`entity`, `client_entity`, `item`, `block`, ...) -> component name -> info.
    pub components: BTreeMap<String, BTreeMap<String, Component>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    /// npm package name.
    pub package: String,
    pub version: String,
    /// blake3 of the tarball, equal to the pin embedded in the binary.
    pub blake3: String,
    /// SPDX id from the tarball's `package.json` `license`.
    pub license: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptModule {
    /// Manifest dependency version (`2.6.0`, `2.7.0-beta`).
    pub version: String,
    pub beta: bool,
    /// Export name -> item.
    pub exports: BTreeMap<String, ApiItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKind {
    Class,
    Interface,
    Enum,
    Function,
    Const,
    Type,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiItem {
    pub kind: ApiKind,
    /// Member name -> one-line signature as written in the `.d.ts`.
    pub members: BTreeMap<String, String>,
    pub deprecated: bool,
    /// First paragraph of the doc comment (hover, `api_lookup`).
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    pub description: Option<String>,
    /// Requires an experimental toggle (name as in `experimentalGameplay`).
    pub experimental: Option<String>,
}

/// One `api_lookup` / `beacon api --find` hit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiHit {
    /// `@minecraft/server@2.6.0#World.getDimension`, `component:entity#minecraft:health`,
    /// `vanilla:entity#minecraft:zombie`.
    pub path: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub deprecated: bool,
}

/// `<data dir>/beacon/api/<game_version>/index.json` (`BEACON_HOME` replaces `<data dir>/beacon`).
/// `None` when there is no data dir or `game_version` is not a plain version string (it names
/// a directory, so no separators or dots-only components).
pub fn cache_path(game_version: &str) -> Option<PathBuf> {
    let plain = !game_version.is_empty()
        && game_version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && !game_version.starts_with('.');
    if !plain {
        return None;
    }
    let base = match std::env::var_os("BEACON_HOME") {
        Some(h) => PathBuf::from(h),
        None => dirs::data_local_dir()?.join("beacon"),
    };
    Some(base.join("api").join(game_version).join("index.json"))
}

/// The installed index to use for `target` (a `targetVersion`): the exact one, else the newest
/// installed version at or below it, else the nearest above it (dotted versions compared
/// numerically). Returns the game version actually used and the index (via [`shared`], so a
/// rewritten file is reloaded). `ApiIndexMissing` when nothing is installed.
pub fn resolve(target: &str) -> Result<(String, std::sync::Arc<ApiIndex>), Diagnostic> {
    let root = cache_path(target)
        .and_then(|p| p.parent()?.parent().map(PathBuf::from))
        .ok_or_else(|| ApiIndex::missing(target, "no cache dir".into()))?;
    resolve_in(&root, target)
}

fn resolve_in(
    root: &std::path::Path,
    target: &str,
) -> Result<(String, std::sync::Arc<ApiIndex>), Diagnostic> {
    let mut installed: Vec<String> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("index.json").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    installed.sort_by_key(|v| version_key(v));
    let t = version_key(target);
    // ascending: the last one <= target, else the first one above
    let pick = installed
        .iter()
        .rfind(|v| version_key(v) <= t)
        .or_else(|| installed.first())
        .ok_or_else(|| {
            ApiIndex::missing(target, format!("none installed in {}", root.display()))
        })?;
    let ix = shared_at(pick, &root.join(pick).join("index.json"))?;
    Ok((pick.clone(), ix))
}

/// `1.26.50` -> [1, 26, 50]; a non-numeric tail (`-beta`) is ignored per component.
pub(crate) fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .map(|c| {
            let d: String = c.chars().take_while(char::is_ascii_digit).collect();
            d.parse().unwrap_or(0)
        })
        .collect()
}

/// Cached index plus the (mtime, size) of its file when it was read.
type Stamp = (Option<std::time::SystemTime>, u64);
static SHARED: std::sync::Mutex<BTreeMap<String, (Stamp, std::sync::Arc<ApiIndex>)>> =
    std::sync::Mutex::new(BTreeMap::new());

fn stamp(path: &std::path::Path) -> Stamp {
    std::fs::metadata(path).map_or((None, 0), |m| (m.modified().ok(), m.len()))
}

/// Process-wide cached index for `game_version` (shared by `check` and the daemon). Only hits
/// are cached: a miss (`ApiIndexMissing`) is retried next call, so a later `beacon api`
/// install is picked up. A hit costs one `stat`: if the cache file's mtime or size changed
/// (`beacon api install` rewrote it) the index is reloaded. Never downloads.
pub fn shared(game_version: &str) -> Result<std::sync::Arc<ApiIndex>, Diagnostic> {
    let path = cache_path(game_version)
        .ok_or_else(|| ApiIndex::missing(game_version, "no cache dir".into()))?;
    shared_at(game_version, &path)
}

fn shared_at(
    game_version: &str,
    path: &std::path::Path,
) -> Result<std::sync::Arc<ApiIndex>, Diagnostic> {
    let mut m = SHARED.lock().unwrap_or_else(|e| e.into_inner());
    // Stamp before the read: a write landing in between only causes one extra reload.
    let now = stamp(path);
    if let Some((st, ix)) = m.get(game_version)
        && *st == now
    {
        return Ok(ix.clone());
    }
    let ix = std::sync::Arc::new(ApiIndex::load_at(game_version, path)?);
    m.insert(game_version.to_owned(), (now, ix.clone()));
    Ok(ix)
}

/// (path, signature, doc, deprecated) -> keep going?
type Visitor<'a> = dyn FnMut(&str, Option<&str>, Option<&str>, bool) -> bool + 'a;

impl ApiIndex {
    /// Loads the cached index for `game_version`: `Err(ApiIndexMissing)` when absent, of an
    /// older `schema`, or unreadable. Never downloads.
    pub fn load(game_version: &str) -> Result<ApiIndex, Diagnostic> {
        let path = cache_path(game_version)
            .ok_or_else(|| Self::missing(game_version, "no cache dir".into()))?;
        Self::load_at(game_version, &path)
    }

    fn missing(game_version: &str, why: String) -> Diagnostic {
        Diagnostic::new(
            Code::ApiIndexMissing,
            format!(
                "no API index for game version {game_version} ({why}); run `beacon api install --game {game_version}`"
            ),
        )
    }

    fn load_at(game_version: &str, path: &std::path::Path) -> Result<ApiIndex, Diagnostic> {
        let missing = |why: String| Self::missing(game_version, why);
        let bytes = std::fs::read(path).map_err(|e| missing(format!("{}: {e}", path.display())))?;
        let ix: ApiIndex = serde_json::from_slice(&bytes)
            .map_err(|e| missing(format!("{}: {e}", path.display())))?;
        if ix.schema != API_INDEX_SCHEMA || ix.game_version != game_version {
            return Err(missing(format!("{}: stale", path.display())));
        }
        Ok(ix)
    }

    /// Exact path first, then prefix, then substring; capped at `limit`. A query matches the
    /// full path (`@minecraft/server@2.10.0#World.getDimension`) or its part after `#`
    /// (`World.getDimension`, `minecraft:zombie`); prefix and substring ignore case.
    pub fn lookup(&self, query: &str, limit: usize) -> Vec<ApiHit> {
        let q = query.trim();
        let ql = q.to_lowercase();
        let mut out: Vec<ApiHit> = Vec::new();
        if q.is_empty() || limit == 0 {
            return out;
        }
        let mut seen = BTreeSet::new();
        for level in 0..3 {
            self.visit(&mut |path, sig, doc, deprecated| {
                let key = path.split_once('#').map_or(path, |(_, k)| k);
                let hit = match level {
                    0 => path == q || key == q,
                    1 => {
                        path.to_lowercase().starts_with(&ql) || key.to_lowercase().starts_with(&ql)
                    }
                    _ => path.to_lowercase().contains(&ql),
                };
                if hit && seen.insert(path.to_string()) {
                    out.push(ApiHit {
                        path: path.to_string(),
                        signature: sig.map(str::to_string),
                        doc: doc.map(str::to_string),
                        deprecated,
                    });
                }
                out.len() < limit
            });
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    /// Every addressable thing, in index order; `f` returns false to stop.
    fn visit(&self, f: &mut Visitor) {
        for (module, versions) in &self.script_modules {
            for m in versions {
                for (name, item) in &m.exports {
                    let base = format!("{module}@{}#{name}", m.version);
                    if !f(&base, None, item.doc.as_deref(), item.deprecated) {
                        return;
                    }
                    for (member, sig) in &item.members {
                        let p = format!("{base}.{member}");
                        if !f(&p, Some(sig), None, item.deprecated) {
                            return;
                        }
                    }
                }
            }
        }
        for (kind, ids) in &self.vanilla {
            for id in ids {
                if !f(&format!("vanilla:{kind}#{id}"), None, None, false) {
                    return;
                }
            }
        }
        for (kind, comps) in &self.components {
            for (name, c) in comps {
                if !f(
                    &format!("component:{kind}#{name}"),
                    None,
                    c.description.as_deref(),
                    false,
                ) {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_picks_newest_at_or_below_else_nearest_above() {
        let home = std::env::temp_dir().join(format!("beacon-apires-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for gv in ["1.21.100", "1.26.50"] {
            let ix = ApiIndex {
                schema: API_INDEX_SCHEMA,
                game_version: gv.into(),
                sources: vec![],
                script_modules: BTreeMap::new(),
                vanilla: BTreeMap::new(),
                components: BTreeMap::new(),
            };
            std::fs::create_dir_all(home.join(gv)).unwrap();
            std::fs::write(
                home.join(gv).join("index.json"),
                serde_json::to_vec(&ix).unwrap(),
            )
            .unwrap();
        }
        let used = |t: &str| resolve_in(&home, t).unwrap().0;
        assert_eq!(used("1.26.50"), "1.26.50");
        assert_eq!(used("1.26.30"), "1.21.100"); // between: the older one
        assert_eq!(used("1.21.100"), "1.21.100");
        assert_eq!(used("1.30.0"), "1.26.50"); // above all
        assert_eq!(used("1.20.30"), "1.21.100"); // below all: nearest above
        assert_eq!(used("1.9.0"), "1.21.100"); // numeric, not lexical
        assert!(resolve_in(&home.join("none"), "1.0.0").is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn shared_reloads_when_the_cache_file_changes() {
        let home = std::env::temp_dir().join(format!("beacon-apiidx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let gv = "9.9.9-shared-test";
        let path = home.join("index.json");
        let write = |extra: &str| {
            let ix = ApiIndex {
                schema: API_INDEX_SCHEMA,
                game_version: gv.into(),
                sources: vec![],
                script_modules: BTreeMap::new(),
                vanilla: BTreeMap::from([("block".into(), BTreeSet::from([extra.to_owned()]))]),
                components: BTreeMap::new(),
            };
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(&path, serde_json::to_vec(&ix).unwrap()).unwrap();
        };
        write("a");
        assert!(shared_at(gv, &path).unwrap().vanilla["block"].contains("a"));
        write("bb"); // size changes even when the mtime tick is coarse
        let ix = shared_at(gv, &path).unwrap();
        assert!(ix.vanilla["block"].contains("bb"), "stale index served");
        assert!(
            std::sync::Arc::ptr_eq(&ix, &shared_at(gv, &path).unwrap()),
            "unchanged file reloaded"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
