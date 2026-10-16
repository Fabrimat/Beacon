//! Contract: `config.json` (4.1) and `module.json` (5.3) shapes. Parsing/validation logic lives
//! in workstream A1 (`load`); this module only fixes the data shapes.

use crate::model::{Edition, Merge};
use serde_json::Value;
use std::collections::BTreeMap;

pub const CONFIG_FILE: &str = "config.json";
pub const LOCK_FILE: &str = "beacon.lock.json";
pub const MODULE_FILE: &str = "module.json";
/// Per-project state dir (cache, trust is NOT here: it is per-user, 5.5).
pub const STATE_DIR: &str = ".beacon";

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub authors: Vec<String>,
    pub target_version: String,
    pub namespace: String,
    #[serde(default)]
    pub packs: Packs,
    #[serde(default)]
    pub beacon: BeaconConfig,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Packs {
    pub behavior_pack: Option<String>,
    pub resource_pack: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeaconConfig {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default = "default_sources")]
    pub sources: Vec<String>,
    #[serde(default)]
    pub pack_meta: Option<PackMeta>,
    #[serde(default)]
    pub outputs: Vec<OutputDef>,
    #[serde(default)]
    pub primary_output: Option<String>,
    #[serde(default)]
    pub targets: BTreeMap<String, TargetDef>,
    #[serde(default)]
    pub plugins: Vec<Value>,
    /// Script module dependencies for BP manifests, e.g. `{"@minecraft/server": "2.0.0"}`.
    /// Phase 1 stand-in for the API index (phase 4); fallback: `package.json` dependencies.
    #[serde(default)]
    pub script_modules: BTreeMap<String, String>,
    #[serde(default)]
    pub shared_edition_uuids: bool,
}

impl Default for BeaconConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("defaults")
    }
}

fn default_sources() -> Vec<String> {
    vec!["./src".into()]
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct PackMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Project-relative `pack_icon.png`, emitted into every pack.
    pub icon: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDef {
    pub id: String,
    /// Globs over project-relative module folders, e.g. `src/lib_*/**`.
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Static (prebuilt) outputs: phase 2.
    #[serde(default, rename = "static")]
    pub static_packs: Option<Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDef {
    pub edition: Edition,
    /// `dev` | `mcpack` | `mcaddon` (phase 1); others are phase 2.
    pub artifact: String,
    #[serde(default)]
    pub outputs: Option<Vec<String>>,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub define: BTreeMap<String, Value>,
}

/// `module.json`: exceptions to convention only.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleJson {
    #[serde(default, rename = "$schema")]
    pub schema: Option<String>,
    /// Output this module belongs to (overrides `include` globs).
    #[serde(default)]
    pub output: Option<String>,
    /// Merge policy for this module's items that collide with others.
    /// `merge` = JSON deep merge for `.json`, line merge for `.lang`.
    #[serde(default)]
    pub on_conflict: Option<OnConflict>,
    /// Module-relative glob -> pack folder (`"ui/*.json": "RP/ui"`); file name kept.
    #[serde(default)]
    pub paths: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OnConflict {
    Error,
    Skip,
    Merge,
}

impl OnConflict {
    /// Resolves `merge` by file kind.
    pub fn to_merge(self, file_name: &str) -> Merge {
        match self {
            OnConflict::Error => Merge::Error,
            OnConflict::Skip => Merge::Skip,
            OnConflict::Merge if file_name.ends_with(".lang") => Merge::LangLines,
            OnConflict::Merge => Merge::JsonDeep,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_shapes_parse() {
        let m: ModuleJson = serde_json::from_str(
            r#"{"onConflict":"merge","output":"main","paths":{"ui/*.json":"RP/ui"}}"#,
        )
        .unwrap();
        assert_eq!(m.on_conflict, Some(OnConflict::Merge));
        let c: ProjectConfig = serde_json::from_str(
            r#"{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"demo",
                "packs":{"behaviorPack":"./packs/BP","resourcePack":"./packs/RP"},
                "beacon":{"version":"0.4.0","sources":["./src"],"versionPolicy":"auto",
                  "outputs":[{"id":"main","include":["src/mod_*/**"]},{"id":"lib","include":["src/lib_*/**"],"namespace":"lib"}]}}"#,
        )
        .unwrap();
        assert_eq!(c.beacon.outputs.len(), 2);
        assert_eq!(c.beacon.sources, vec!["./src"]);
    }
}
