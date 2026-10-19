//! Workstream A2: `beacon.lock.json` (4.2).

use crate::canonical;
use crate::config::{LOCK_FILE, ProjectConfig};
use crate::diag::{Code, Diagnostic};
use crate::model::{Edition, OutputId, Pack, Version};
use std::collections::BTreeMap;
use std::path::Path;

/// `beacon.lock.json`. Pack + module UUIDs per (output x edition), or one `shared` set when
/// `sharedEditionUuids`; script module UUID per output. Entries of outputs that disappeared
/// are kept (identities survive; `rename-output` is phase 3). World templates and skin packs
/// are keyed by project-relative folder and are never shared across editions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lockfile {
    pub canonical_version: u32,
    /// e.g. `esbuild 0.25.4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundler: Option<String>,
    #[serde(default)]
    pub outputs: BTreeMap<OutputId, OutputLock>,
    /// World template identities, keyed by world folder (`worlds/main`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub worlds: BTreeMap<String, EditionMap>,
    /// Skin pack identities, keyed by skins folder.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skins: BTreeMap<String, EditionMap>,
    /// Git-sourced plugin pins, keyed by plugin id (5.10, `plugin` module). Written only by
    /// `beacon plugin update`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, crate::plugin::PluginLock>,
}

impl Default for Lockfile {
    fn default() -> Self {
        Lockfile {
            canonical_version: canonical::CANONICAL_VERSION,
            bundler: None,
            outputs: BTreeMap::new(),
            worlds: BTreeMap::new(),
            skins: BTreeMap::new(),
            plugins: BTreeMap::new(),
        }
    }
}

/// Per-edition identities of one world template or skin pack.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EditionMap {
    #[serde(default)]
    pub editions: BTreeMap<Edition, PackLock>,
}

/// `[1, 2, 3]` or `"1.2.3"` (what legacy manifests use).
fn parse_version(v: &serde_json::Value) -> Option<Version> {
    let n: Vec<u32> = match v {
        serde_json::Value::Array(a) => a
            .iter()
            .map(|x| x.as_u64().and_then(|x| u32::try_from(x).ok()))
            .collect::<Option<_>>()?,
        serde_json::Value::String(s) => s
            .split('.')
            .map(|p| p.parse().ok())
            .collect::<Option<_>>()?,
        _ => return None,
    };
    n.try_into().ok()
}

/// Last release of a pack (4.2.1).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Release {
    pub version: Version,
    /// Content hash at that release, hex. Empty = imported from a legacy manifest (hash
    /// unknown): the next release adopts the current content at this same version.
    pub hash: String,
}

/// A manifest dependency on a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRef {
    pub uuid: String,
    pub version: Version,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputLock {
    pub script_module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<EditionLock>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub editions: BTreeMap<Edition, EditionLock>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EditionLock {
    #[serde(rename = "BP")]
    pub bp: PackLock,
    #[serde(rename = "RP")]
    pub rp: PackLock,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PackLock {
    pub header: String,
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<Release>,
    /// Releases per edition, for packs whose identity is shared across editions
    /// (`sharedEditionUuids`): their content differs per edition, so must their release.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub releases: BTreeMap<Edition, Release>,
}

impl PackLock {
    /// The last release for `edition`.
    pub fn release_of(&self, edition: Edition) -> Option<&Release> {
        self.releases.get(&edition).or(self.release.as_ref())
    }
}

fn uuid4() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("os randomness");
    fmt_uuid(b)
}

fn fmt_uuid(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

fn new_pack() -> PackLock {
    PackLock {
        header: uuid4(),
        module: uuid4(),
        ..Default::default()
    }
}

fn new_edition() -> EditionLock {
    EditionLock {
        bp: new_pack(),
        rp: new_pack(),
    }
}

impl Lockfile {
    /// Loads the lockfile (or starts empty), adds missing entries for `outputs` (new random
    /// UUIDs) for every edition of `cfg.beacon.targets` (Bedrock if none), plus one entry per
    /// (folder, edition) in `worlds` (world templates) and `skins` (skin packs), and saves it
    /// (canonical JSON, atomically) if it changed. With `frozen`, a needed change is
    /// `Code::LockfileFrozen` instead.
    pub fn ensure(
        root: &Path,
        cfg: &ProjectConfig,
        outputs: &[OutputId],
        worlds: &[(String, Edition)],
        skins: &[(String, Edition)],
        frozen: bool,
    ) -> Result<Lockfile, Diagnostic> {
        let path = root.join(LOCK_FILE);
        let invalid = |e: &dyn std::fmt::Display| {
            Diagnostic::new(Code::LockfileInvalid, format!("{LOCK_FILE}: {e}"))
        };
        let loaded = match std::fs::read(&path) {
            Ok(b) => Some(serde_json::from_slice::<Lockfile>(&b).map_err(|e| invalid(&e))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(invalid(&e)),
        };
        let mut lock = loaded.clone().unwrap_or_default();
        let mut editions: Vec<Edition> = cfg.beacon.targets.values().map(|t| t.edition).collect();
        editions.sort();
        editions.dedup();
        if editions.is_empty() {
            editions.push(Edition::Bedrock);
        }
        for id in outputs {
            let o = lock
                .outputs
                .entry(id.clone())
                .or_insert_with(|| OutputLock {
                    script_module: uuid4(),
                    ..Default::default()
                });
            if cfg.beacon.shared_edition_uuids {
                if o.shared.is_none() {
                    o.shared = Some(new_edition());
                }
            } else {
                for e in &editions {
                    o.editions.entry(*e).or_insert_with(new_edition);
                }
            }
        }
        for (map, list) in [(&mut lock.worlds, worlds), (&mut lock.skins, skins)] {
            for (dir, e) in list {
                map.entry(dir.clone())
                    .or_default()
                    .editions
                    .entry(*e)
                    .or_insert_with(new_pack);
            }
        }
        if loaded.as_ref() != Some(&lock) {
            if frozen {
                return Err(Diagnostic::new(
                    Code::LockfileFrozen,
                    format!("{LOCK_FILE} would change but --frozen was given"),
                ));
            }
            lock.save(root).map_err(|e| {
                Diagnostic::new(Code::EmitFailed, format!("cannot write {LOCK_FILE}: {e}"))
            })?;
        }
        Ok(lock)
    }

    /// Writes the lockfile (canonical JSON, atomically).
    pub fn save(&self, root: &Path) -> std::io::Result<()> {
        let bytes = canonical::to_vec(&serde_json::to_value(self).expect("lockfile json"));
        let mut rnd = [0u8; 8];
        getrandom::fill(&mut rnd).map_err(std::io::Error::other)?;
        let tmp = root.join(format!("{LOCK_FILE}.{:x}.tmp", u64::from_le_bytes(rnd)));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, root.join(LOCK_FILE)).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    fn pack_lock(&self, output: &str, pack: Pack, edition: Edition) -> Option<&PackLock> {
        let o = self.outputs.get(output)?;
        let e = o.shared.as_ref().or_else(|| o.editions.get(&edition))?;
        match pack {
            Pack::Behavior => Some(&e.bp),
            Pack::Resource => Some(&e.rp),
        }
    }

    /// The lock entry of a release key: `<output>/BP|RP`, `world:<dir>`, `skins:<dir>`
    /// (the `PackVersion.pack` strings).
    pub fn entry(&self, key: &str, edition: Edition) -> Option<&PackLock> {
        if let Some(d) = key.strip_prefix("world:") {
            return self.worlds.get(d)?.editions.get(&edition);
        }
        if let Some(d) = key.strip_prefix("skins:") {
            return self.skins.get(d)?.editions.get(&edition);
        }
        let (out, p) = key.rsplit_once('/')?;
        self.pack_lock(
            out,
            if p == "BP" {
                Pack::Behavior
            } else {
                Pack::Resource
            },
            edition,
        )
    }

    /// [`Lockfile::entry`], mutable.
    pub fn entry_mut(&mut self, key: &str, edition: Edition) -> Option<&mut PackLock> {
        if let Some(d) = key.strip_prefix("world:") {
            return self.worlds.get_mut(d)?.editions.get_mut(&edition);
        }
        if let Some(d) = key.strip_prefix("skins:") {
            return self.skins.get_mut(d)?.editions.get_mut(&edition);
        }
        let (out, p) = key.rsplit_once('/')?;
        let o = self.outputs.get_mut(out)?;
        let e = match o.shared.as_mut() {
            Some(s) => s,
            None => o.editions.get_mut(&edition)?,
        };
        Some(if p == "BP" { &mut e.bp } else { &mut e.rp })
    }

    /// Release UUID of a pack header; `None` if `ensure` did not cover the output/edition.
    pub fn pack_uuid(&self, output: &str, pack: Pack, edition: Edition) -> Option<String> {
        self.pack_lock(output, pack, edition)
            .map(|p| p.header.clone())
    }

    /// Release UUID of a pack's data/resources module.
    pub fn module_uuid(&self, output: &str, pack: Pack, edition: Edition) -> Option<String> {
        self.pack_lock(output, pack, edition)
            .map(|p| p.module.clone())
    }

    /// Release UUID of the output's script module (edition independent).
    pub fn script_module_uuid(&self, output: &str) -> Option<String> {
        self.outputs.get(output).map(|o| o.script_module.clone())
    }

    /// Migration (5.6): adopts the UUIDs of existing legacy manifests for `output` so the
    /// migrated packs keep their identity in worlds. `bp` / `rp` are parsed `manifest.json`
    /// values; header UUID -> `header`, the first data/resources module UUID -> `module`, a
    /// script module UUID -> `script_module`; the header version becomes the pack's last
    /// release as `Release { version, hash: "" }` (hash unknown: the next release adopts the content
    /// at this version, no bump). A present but unusable `header.version` is `LockfileInvalid`. `edition` `None` = `shared`. Never overwrites an existing
    /// entry: an output/edition already present is left as-is. A pack without a manifest (or a
    /// manifest module) gets fresh UUIDs; an invalid manifest is `LockfileInvalid`.
    pub fn import_manifests(
        &mut self,
        output: &str,
        edition: Option<Edition>,
        bp: Option<&serde_json::Value>,
        rp: Option<&serde_json::Value>,
    ) -> Result<(), Diagnostic> {
        let slot_taken = self.outputs.get(output).is_some_and(|o| match edition {
            None => o.shared.is_some(),
            Some(e) => o.editions.contains_key(&e),
        });
        let pack = |name: &str, m: Option<&serde_json::Value>, kind: &str| {
            let bad = |why: &str| {
                Diagnostic::new(
                    Code::LockfileInvalid,
                    format!("{name} manifest.json: {why}"),
                )
            };
            let Some(m) = m else {
                return Ok((new_pack(), None));
            };
            let uuid = |v: &serde_json::Value| v["uuid"].as_str().map(str::to_owned);
            let header = uuid(&m["header"]).ok_or_else(|| bad("missing header.uuid"))?;
            let modules = m["modules"].as_array().map_or(&[][..], |a| &a[..]);
            let of = |t: &str| modules.iter().find(|x| x["type"] == t).and_then(uuid);
            let version = match &m["header"]["version"] {
                serde_json::Value::Null => None,
                v => Some(parse_version(v).ok_or_else(|| {
                    bad(&format!(
                        "header.version {v} is not [major, minor, patch] or \"x.y.z\""
                    ))
                })?),
            };
            let pack = PackLock {
                header,
                module: of(kind).unwrap_or_else(uuid4),
                release: version.map(|version| Release {
                    version,
                    hash: String::new(),
                }),
                ..Default::default()
            };
            Ok::<_, Diagnostic>((pack, of("script")))
        };
        let ((bp, script), (rp, _)) = (pack("BP", bp, "data")?, pack("RP", rp, "resources")?);
        if slot_taken {
            return Ok(());
        }
        let o = self
            .outputs
            .entry(output.to_owned())
            .or_insert_with(|| OutputLock {
                script_module: script.unwrap_or_else(uuid4),
                ..Default::default()
            });
        let e = EditionLock { bp, rp };
        match edition {
            None => o.shared = Some(e),
            Some(ed) => {
                o.editions.insert(ed, e);
            }
        }
        Ok(())
    }

    /// `beacon rename-output`: moves the identities of output `from` to `to` (UUIDs and
    /// releases survive the rename). Errors if `from` is missing or `to` already exists.
    pub fn rename_output(&mut self, from: &str, to: &str) -> Result<(), Diagnostic> {
        if self.outputs.contains_key(to) {
            return Err(Diagnostic::new(
                Code::LockfileInvalid,
                format!("{LOCK_FILE}: output `{to}` already exists"),
            ));
        }
        let entry = self.outputs.remove(from).ok_or_else(|| {
            Diagnostic::new(
                Code::LockfileInvalid,
                format!("{LOCK_FILE}: no output `{from}`"),
            )
        })?;
        self.outputs.insert(to.to_owned(), entry);
        Ok(())
    }

    pub fn set_bundler(&mut self, version: Option<String>) {
        self.bundler = version;
    }

    /// Version of a pack: its last release, else `[1, 0, 0]`.
    pub fn version_of(&self, output: &str, pack: Pack, edition: Edition) -> Version {
        self.pack_lock(output, pack, edition)
            .and_then(|p| p.release_of(edition))
            .map_or([1, 0, 0], |r| r.version)
    }

    /// Header UUID (dev UUID when `dev` is the worktree root) and version of a pack.
    pub fn pack_ref(
        &self,
        output: &str,
        pack: Pack,
        edition: Edition,
        dev: Option<&Path>,
    ) -> Option<PackRef> {
        let uuid = self.pack_uuid(output, pack, edition)?;
        Some(PackRef {
            uuid: dev.map_or_else(|| uuid.clone(), |w| dev_uuid(&uuid, w)),
            version: self.version_of(output, pack, edition),
        })
    }
}

/// Dev UUID (4.2): deterministic from the release UUID and the worktree root path, never
/// committed (blake3-derived, formatted as a v4-shaped UUID).
pub fn dev_uuid(release: &str, worktree_root: &Path) -> String {
    let root = std::fs::canonicalize(worktree_root).unwrap_or_else(|_| worktree_root.into());
    let mut h = blake3::Hasher::new();
    h.update(release.as_bytes());
    h.update(&[0]);
    let root = root.to_string_lossy();
    // Windows paths are case-insensitive
    let root = if cfg!(windows) {
        root.to_lowercase()
    } else {
        root.into_owned()
    };
    h.update(root.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&h.finalize().as_bytes()[..16]);
    fmt_uuid(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_output_moves_identity() {
        let mut l = Lockfile::default();
        l.outputs.insert(
            "a".into(),
            OutputLock {
                script_module: "s".into(),
                ..Default::default()
            },
        );
        l.outputs.insert("c".into(), OutputLock::default());
        l.rename_output("a", "b").unwrap();
        assert_eq!(l.outputs["b"].script_module, "s");
        assert!(!l.outputs.contains_key("a"));
        assert!(l.rename_output("a", "x").is_err());
        assert!(l.rename_output("b", "c").is_err());
    }
}
