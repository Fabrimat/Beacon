//! Contract: `config.json` (4.1) and `module.json` (5.3) shapes. Parsing/validation logic lives
//! in workstream A1 (`load`); this module only fixes the data shapes.

use crate::model::{Edition, Merge, Pack};
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
    /// Standard field: world folder globs (`./worlds/*`). Read, never written.
    #[serde(default)]
    pub worlds: Vec<String>,
    #[serde(default)]
    pub beacon: BeaconConfig,
}

/// BP-relative path of an output's script bundle unless `scriptEntry` says otherwise.
pub const DEFAULT_SCRIPT_ENTRY: &str = "scripts/main.js";

impl ProjectConfig {
    /// BP-relative bundle path of `output` (`scriptEntry`, else `scripts/main.js`); also the
    /// manifest `entry`. The bundle's source map is `<path>.map`.
    pub fn script_entry(&self, output: &str) -> &str {
        self.beacon
            .outputs
            .iter()
            .find(|o| o.id == output)
            .and_then(|o| o.script_entry.as_deref())
            .unwrap_or(DEFAULT_SCRIPT_ENTRY)
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Packs {
    pub behavior_pack: Option<String>,
    pub resource_pack: Option<String>,
    /// When a `skinpack` target exists.
    pub skin_pack: Option<String>,
    /// When a `mctemplate` target exists.
    pub world_template: Option<String>,
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
    /// Pack versioning (4.2.1).
    #[serde(default)]
    pub version_policy: VersionPolicy,
    /// Derived `.lang` names for content without an explicit one (phase 3, A2).
    #[serde(default)]
    pub auto_names: Option<AutoNames>,
    /// Emit `RP/textures/textures_list.json` from the RP's textures (phase 3, A2).
    #[serde(default)]
    pub textures_list: bool,
}

/// `beacon.autoNames`: which content kinds get a derived display name in `language`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoNames {
    #[serde(default)]
    pub entities: bool,
    #[serde(default)]
    pub items: bool,
    #[serde(default)]
    pub blocks: bool,
    /// Spawn egg names (`item.spawn_egg.entity.<id>.name`), built as `prefix + name + postfix`.
    #[serde(default)]
    pub spawn_eggs: Option<SpawnEggNames>,
    /// `.lang` file stem the names go to.
    #[serde(default = "default_language")]
    pub language: String,
}

fn default_language() -> String {
    "en_US".into()
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SpawnEggNames {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub postfix: String,
}

/// 4.2.1: `auto` bumps from content hashes on release; `manual` takes `--set-version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VersionPolicy {
    #[default]
    Auto,
    Manual,
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
    /// Extra manifest fields per pack, deep-merged into the generated manifest
    /// (`pack_scope`, `metadata`, `capabilities`, ...). `header.uuid`, `header.version` and
    /// `modules[].uuid` are never overridable (lockfile-owned).
    #[serde(default)]
    pub manifest: BTreeMap<Pack, Value>,
    /// Which pack manifests depend on their sibling (uuid + released version).
    #[serde(default, rename = "packDependencies")]
    pub pack_dependencies: PackDependencies,
}

/// `packMeta.packDependencies`: `bp-rp` (the BP depends on the RP; default), `rp-bp` (the RP
/// depends on the BP) or `mutual` (both).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackDependencies {
    #[default]
    BpRp,
    RpBp,
    Mutual,
}

impl PackDependencies {
    pub fn bp_on_rp(self) -> bool {
        self != Self::RpBp
    }
    pub fn rp_on_bp(self) -> bool {
        self != Self::BpRp
    }
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
    /// Static (prebuilt) packs, included as-is (4.3).
    #[serde(default, rename = "static")]
    pub static_packs: Option<StaticPacks>,
    /// BP-relative path of this output's script bundle (a `.js` `RelPath`; default
    /// `scripts/main.js`): the emitted file, its `.map` and the manifest `entry`.
    #[serde(default)]
    pub script_entry: Option<String>,
}

/// Project-relative folders of a static output's prebuilt packs.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct StaticPacks {
    #[serde(default)]
    pub bp: Option<String>,
    #[serde(default)]
    pub rp: Option<String>,
}

/// What a target produces (4.4). An unknown value fails `config.json` parsing (`ConfigField`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Artifact {
    Dev,
    McPack,
    McAddon,
    McWorld,
    McTemplate,
    SkinPack,
    Marketplace,
}

impl Artifact {
    /// The JSON value (`mcaddon`).
    pub fn name(self) -> &'static str {
        match self {
            Artifact::Dev => "dev",
            Artifact::McPack => "mcpack",
            Artifact::McAddon => "mcaddon",
            Artifact::McWorld => "mcworld",
            Artifact::McTemplate => "mctemplate",
            Artifact::SkinPack => "skinpack",
            Artifact::Marketplace => "marketplace",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDef {
    pub edition: Edition,
    pub artifact: Artifact,
    #[serde(default)]
    pub outputs: Option<Vec<String>>,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub define: BTreeMap<String, Value>,
    /// Project-relative world folder (`dev` world, `mcworld`, `mctemplate`, `marketplace`).
    #[serde(default)]
    pub world: Option<String>,
    /// Overrides the world's `LevelName`.
    #[serde(default)]
    pub world_name: Option<String>,
    /// Embed the outputs' packs into the world (`false` warns about missing packs).
    #[serde(default = "yes")]
    pub embed_packs: bool,
    /// Skin pack source folder (`skinpack`).
    #[serde(default)]
    pub skins: Option<String>,
    /// Store art folder (`marketplace`).
    #[serde(default)]
    pub art: Option<String>,
    /// Experiment toggles written into the world (`level.dat`).
    #[serde(default)]
    pub experiments: Vec<String>,
    /// `level.dat` overrides; keys are dotted NBT paths.
    #[serde(default)]
    pub level_dat: BTreeMap<String, Value>,
    #[serde(default)]
    pub template: TemplateOptions,
}

fn yes() -> bool {
    true
}

/// World template manifest options (`mctemplate`, `marketplace`).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateOptions {
    #[serde(default)]
    pub lock_template_options: bool,
    #[serde(default)]
    pub allow_random_seed: bool,
    #[serde(default)]
    pub base_game_version: Option<String>,
}

/// `module.json`: exceptions to convention only.
#[derive(Debug, Clone, serde::Deserialize)]
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
    /// Script hooks: module-relative function path (no extension) -> priority, lower first.
    #[serde(default)]
    pub hooks: Hooks,
    /// `item_texture.json` entries: short name -> module-relative texture path.
    #[serde(default)]
    pub item_textures: BTreeMap<String, String>,
    /// `terrain_texture.json` entries: short name -> module-relative texture path.
    #[serde(default)]
    pub terrain_textures: BTreeMap<String, String>,
    /// Module-relative folders whose contents are pack-relative paths (`{"BP": "bp"}`):
    /// routed verbatim (never via convention routing), and the scripts rule is off inside them.
    #[serde(default)]
    pub pack_roots: BTreeMap<Pack, String>,
    /// Module-relative glob -> merge policy; overrides `onConflict` for matching files.
    #[serde(default)]
    pub conflicts: BTreeMap<String, OnConflict>,
    /// Derive `item_texture.json` / `terrain_texture.json` entries from sibling pngs.
    #[serde(default = "yes")]
    pub auto_textures: bool,
}

impl Default for ModuleJson {
    fn default() -> Self {
        serde_json::from_str("{}").expect("defaults")
    }
}

/// `module.json` `hooks`: module-relative function paths without extension -> priority
/// (lower runs first).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    #[serde(default)]
    pub tick: BTreeMap<String, i32>,
    #[serde(default)]
    pub init: BTreeMap<String, i32>,
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
        assert_eq!(c.beacon.version_policy, VersionPolicy::Auto);
    }

    #[test]
    fn phase2_shapes_parse() {
        let c: ProjectConfig = serde_json::from_str(
            r#"{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"demo",
                "packs":{"skinPack":"./packs/skins","worldTemplate":"./packs/WT"},"worlds":["./worlds/*"],
                "beacon":{"versionPolicy":"manual",
                  "outputs":[{"id":"v","static":{"bp":"./vendor/BP"}}],
                  "targets":{"w":{"edition":"education","artifact":"mcworld","world":"worlds/main",
                    "embedPacks":false,"levelDat":{"abilities.mayfly":1},"experiments":["cameras"],
                    "template":{"lockTemplateOptions":true}},
                    "s":{"edition":"bedrock","artifact":"skinpack","skins":"skins"}}}}"#,
        )
        .unwrap();
        assert_eq!(c.packs.skin_pack.as_deref(), Some("./packs/skins"));
        assert_eq!(c.worlds, vec!["./worlds/*"]);
        assert_eq!(c.beacon.version_policy, VersionPolicy::Manual);
        let st = c.beacon.outputs[0].static_packs.as_ref().unwrap();
        assert_eq!(
            (st.bp.as_deref(), st.rp.as_deref()),
            (Some("./vendor/BP"), None)
        );
        let w = &c.beacon.targets["w"];
        assert_eq!(w.artifact, Artifact::McWorld);
        assert!(!w.embed_packs && w.template.lock_template_options);
        assert!(c.beacon.targets["s"].embed_packs);
        assert_eq!(c.beacon.targets["s"].artifact, Artifact::SkinPack);
        let bad = serde_json::from_str::<TargetDef>(r#"{"edition":"bedrock","artifact":"zip"}"#);
        assert!(bad.unwrap_err().is_data());
        let m: ModuleJson = serde_json::from_str(
            r#"{"hooks":{"tick":{"functions/loop":0}},"itemTextures":{"ruby":"items/ruby.png"}}"#,
        )
        .unwrap();
        assert_eq!(m.hooks.tick["functions/loop"], 0);
        assert_eq!(m.item_textures["ruby"], "items/ruby.png");
    }

    #[test]
    fn phase3_shapes_parse() {
        let m: ModuleJson = serde_json::from_str(
            r#"{"packRoots":{"BP":"bp","RP":"rp"},"conflicts":{"texts/*.lang":"merge"},"autoTextures":false}"#,
        )
        .unwrap();
        assert_eq!(m.pack_roots[&Pack::Behavior], "bp");
        assert_eq!(m.conflicts["texts/*.lang"], OnConflict::Merge);
        assert!(!m.auto_textures);
        assert!(ModuleJson::default().auto_textures);
        let c: BeaconConfig = serde_json::from_str(
            r#"{"autoNames":{"entities":true,"spawnEggs":{"prefix":"Spawn "}},"texturesList":true,
                "packMeta":{"manifest":{"BP":{"metadata":{"authors":["x"]}}}},
                "outputs":[{"id":"main","scriptEntry":"src/main/scripts/main.ts"}]}"#,
        )
        .unwrap();
        let n = c.auto_names.unwrap();
        assert!(n.entities && !n.items);
        assert_eq!(n.language, "en_US");
        assert_eq!(n.spawn_eggs.unwrap().prefix, "Spawn ");
        assert!(c.textures_list);
        assert_eq!(
            c.pack_meta.unwrap().manifest[&Pack::Behavior]["metadata"]["authors"][0],
            "x"
        );
        assert_eq!(
            c.outputs[0].script_entry.as_deref(),
            Some("src/main/scripts/main.ts")
        );
    }
}
