//! Contract: `beacon migrate` (5.6), phase 3. A one-time conversion of a Regolith project
//! (ModularMC and/or system_template) into a native Beacon project; nothing here runs at build
//! time, and the result must build with Beacon alone.
//!
//! Flow: [`detect`] -> evaluate the legacy maps once ([`LegacyEval`]: `beacon_jshost::legacy`
//! for ModularMC, or a real legacy run passed as `--evaluated`) -> [`plan`] -> [`apply`],
//! then the migrated project is built and compared with `--oracle` ([`equiv::compare`]).

pub mod codemod;
pub mod equiv;
pub mod plan;
pub mod regolith;
pub mod system_template;
pub mod write;

use crate::diag::{Code, Diagnostic};
use crate::lock::Lockfile;
use crate::merge::strip_comments;
use crate::model::Body;
use crate::path::RelPath;
use equiv::EquivReport;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Regolith's `dataPath` when `config.json` does not set one.
pub const DEFAULT_DATA_PATH: &str = "./packs/data";

/// The legacy filters `beacon migrate` understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LegacyKind {
    ModularMc,
    SystemTemplate,
}

impl LegacyKind {
    pub const ALL: [LegacyKind; 2] = [LegacyKind::ModularMc, LegacyKind::SystemTemplate];

    /// The `filterDefinitions` key, which is also the filter's folder under `dataPath`.
    pub fn filter_name(self) -> &'static str {
        match self {
            LegacyKind::ModularMc => "modular_mc",
            LegacyKind::SystemTemplate => "system_template",
        }
    }
}

/// Regolith `config.json` (format 1.4). Only what migration reads is typed; every other key
/// is kept in `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegolithConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub packs: RegolithPacks,
    /// Project config standard: world folder globs.
    #[serde(default)]
    pub worlds: Vec<String>,
    #[serde(default)]
    pub regolith: RegolithSection,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegolithPacks {
    #[serde(default)]
    pub behavior_pack: Option<String>,
    #[serde(default)]
    pub resource_pack: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The `regolith` object of `config.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegolithSection {
    /// Project-relative; [`DEFAULT_DATA_PATH`] when absent.
    #[serde(default)]
    pub data_path: Option<String>,
    #[serde(default)]
    pub filter_definitions: BTreeMap<String, FilterDef>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub format_version: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A `filterDefinitions` entry: remote (`url` + `version`) or local (`runWith` + `script`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterDef {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub run_with: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A Regolith profile; migration maps profiles to Beacon targets (4.4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    #[serde(default)]
    pub export: Option<ExportDef>,
    #[serde(default)]
    pub filters: Vec<FilterRun>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A profile's `export`: `target` is `development`, `local`, `exact`, `world`, ...;
/// `build` is `standard`, `preview` or `education`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDef {
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub build: Option<String>,
    #[serde(default)]
    pub world_name: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// One step of a profile: a filter run (`filter` + `settings`) or a nested `profile`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterRun {
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub settings: Option<Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// A detected legacy project.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyProject {
    pub root: PathBuf,
    pub config: RegolithConfig,
    /// Legacy filters defined in `filterDefinitions`, in [`LegacyKind::ALL`] order.
    pub kinds: Vec<LegacyKind>,
    /// `root` + `dataPath`.
    pub data_dir: PathBuf,
}

/// A map entry's conflict policy (ModularMC `onConflict`, system_template `on_conflict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Policy {
    Stop,
    Skip,
    Merge,
    Overwrite,
    #[serde(alias = "append_start")]
    AppendStart,
    #[serde(alias = "append_end")]
    AppendEnd,
}

/// How a map entry's source was evaluated; the value is the template scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "scope", rename_all = "camelCase")]
pub enum Template {
    None,
    /// ModularMC `jsonTemplate` / system_template `json_template`.
    Json(Value),
    /// ModularMC `textTemplate` (`{ts: ... :}`).
    Text(Value),
}

/// What one contributor did to its output (system_template log statuses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Applied {
    Created,
    Merged,
    Appended,
    Skipped,
    Overwrote,
}

/// One map entry that targeted an output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contributor {
    /// Project-relative path of the `_map.ts` / `_map.py`.
    pub map: RelPath,
    /// Index of the entry in the evaluated map array (glob expansions share it).
    pub entry: usize,
    /// Project-relative source file.
    pub source: RelPath,
    pub policy: Policy,
    pub template: Template,
    pub applied: Applied,
}

/// One file the legacy tool produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalOutput {
    /// `BP/...`, `RP/...` or `data/...`.
    pub path: String,
    /// Final content. `Passthrough` (a project-relative source) for plain copies.
    pub body: Body,
    /// In application order.
    pub contributors: Vec<Contributor>,
}

/// The concrete result of evaluating the legacy maps once (5.6 "record their output").
/// Round-trips through JSON so later steps can be tested without Deno or Python.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyEval {
    /// Keyed by [`EvalOutput::path`].
    pub outputs: BTreeMap<String, EvalOutput>,
    /// ModularMC `SCRIPTS` entries, project-relative, in bundle order (module order, then
    /// array order).
    pub scripts: Vec<RelPath>,
    /// Environment variables the maps read, with the value they saw (`None` = unset).
    pub env_read: BTreeMap<String, Option<String>>,
    /// Console output of the maps (warnings, skip messages).
    pub logs: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// `beacon migrate` options.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrateOptions {
    /// The new Beacon project directory.
    pub out: PathBuf,
    /// Previously built legacy output (`BP/`, `RP/`), compared with the migrated build.
    pub oracle: Option<PathBuf>,
    /// Output of a real legacy run in Regolith's tmp layout (`BP/`, `RP/`, `data/`, optional
    /// `system_template_log.json`), used instead of evaluating the maps. Required for
    /// system_template (Beacon does not evaluate `_map.py` in phase 3).
    pub evaluated: Option<PathBuf>,
    /// `process.env` seen by the maps (`--env K=V`); anything else reads as unset.
    pub env: BTreeMap<String, String>,
    pub dry_run: bool,
    /// Allow a non-empty `out`.
    pub force: bool,
    /// Project namespace; default derived from the legacy project.
    pub namespace: Option<String>,
}

/// One step of a migration plan. Paths: `from` is legacy-root-relative, `to` is
/// `out`-relative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    Copy {
        from: RelPath,
        to: RelPath,
    },
    Write {
        to: RelPath,
        body: Body,
    },
    /// The new `config.json`.
    Config(Value),
    /// The new `beacon.lock.json` (legacy manifest UUIDs imported).
    Lock(Lockfile),
    Note(Diagnostic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToPortKind {
    /// A pre-build script that writes generated sources; its output was imported as files.
    PrebuildScript,
    /// A one-to-many map loop that could become a generator.
    GeneratorCandidate,
    /// A `process.env` branch; port to a target `define` or `ctx.edition`.
    EnvBranch,
    /// A subfunctions source (system_template).
    SubfunctionSource,
    /// A system_template Python plugin.
    PythonPlugin,
    /// A Regolith filter kept as a `legacy_filter` plugin.
    LegacyFilter,
}

/// Something left for a human to port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToPort {
    pub kind: ToPortKind,
    /// Legacy-root-relative.
    pub path: RelPath,
    pub why: String,
}

/// A report line about one path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub why: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Module folders created, `out`-relative.
    pub modules: Vec<RelPath>,
    /// Files copied or written.
    pub files: usize,
    /// Legacy constructs imported as their evaluated output (`MigrateFlattened`).
    pub flattened: Vec<Entry>,
    /// Legacy constructs dropped (`MigrateDropped`).
    pub dropped: Vec<Entry>,
    /// Source rewrites applied (e.g. script import paths).
    pub codemods: Vec<Entry>,
    pub to_port: Vec<ToPort>,
    /// Equivalence result when `--oracle` was given.
    pub oracle: Option<EquivReport>,
    /// blake3 (hex) of the legacy sources the plan was made from; `apply` refuses if they
    /// changed since.
    pub source_hash: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MigrationPlan {
    pub changes: Vec<Change>,
    pub report: Report,
}

/// Reads `root/config.json` (JSONC accepted), finds the `modular_mc` / `system_template`
/// filters by their `filterDefinitions` key, and locates the data dir (`dataPath`, default
/// [`DEFAULT_DATA_PATH`]). Errors (`MigrateUnsupported`): no or invalid `config.json`, no
/// known legacy filter, or a missing data dir.
pub fn detect(root: &Path) -> Result<LegacyProject, Vec<Diagnostic>> {
    let err = |m: String| vec![Diagnostic::new(Code::MigrateUnsupported, m)];
    let path = root.join("config.json");
    let bytes = std::fs::read(&path).map_err(|e| err(format!("{}: {e}", path.display())))?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let config: RegolithConfig = serde_json::from_slice(&strip_comments(bytes))
        .map_err(|e| err(format!("{}: {e}", path.display())))?;
    let kinds: Vec<LegacyKind> = LegacyKind::ALL
        .into_iter()
        .filter(|k| {
            config
                .regolith
                .filter_definitions
                .contains_key(k.filter_name())
        })
        .collect();
    if kinds.is_empty() {
        return Err(err(format!(
            "{}: no modular_mc or system_template filter in regolith.filterDefinitions",
            path.display()
        )));
    }
    let data = config
        .regolith
        .data_path
        .as_deref()
        .unwrap_or(DEFAULT_DATA_PATH);
    let data_dir = root.join(data);
    if !data_dir.is_dir() {
        return Err(err(format!(
            "data dir {} (regolith.dataPath) does not exist",
            data_dir.display()
        )));
    }
    Ok(LegacyProject {
        root: root.to_path_buf(),
        config,
        kinds,
        data_dir,
    })
}

/// Turns the evaluated legacy output into a plan for a Beacon project at `opts.out`
/// (convention modules, `module.json` with `packRoots`/`conflicts`, `config.json` targets
/// from profiles, lockfile with the legacy manifest UUIDs). Writes nothing.
pub fn plan(
    project: &LegacyProject,
    eval: &LegacyEval,
    opts: &MigrateOptions,
) -> Result<MigrationPlan, Vec<Diagnostic>> {
    plan::plan(project, eval, opts)
}

/// Writes `plan` into `opts.out`. Refuses (`MigrateOutInvalid`) when `out` is inside, equal
/// to, or linked to (symlink/junction, at any level) `project.root`, or contains the root;
/// when `out` is not empty and `force` is off; and when the legacy sources no longer hash to
/// `plan.report.source_hash`. Never modifies the legacy project.
pub fn apply(
    project: &LegacyProject,
    plan: &MigrationPlan,
    opts: &MigrateOptions,
) -> Result<(), Vec<Diagnostic>> {
    write::apply(project, plan, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/migrate")
            .join(name)
    }

    #[test]
    fn detects_modular_mc() {
        let p = detect(&fixture("modular_mc")).unwrap();
        assert_eq!(p.kinds, [LegacyKind::ModularMc]);
        assert!(p.data_dir.join("modular_mc/auto-map.ts").is_file());
        assert_eq!(p.config.packs.behavior_pack.as_deref(), Some("./packs/BP"));
        let f = &p.config.regolith.profiles["default"].filters[0];
        assert_eq!(f.filter.as_deref(), Some("modular_mc"));
        assert_eq!(
            f.settings.as_ref().unwrap()["esbuild"]["buildPath"],
            ".modular_mc/main.js"
        );
        assert!(p.config.extra.contains_key("$schema"));
    }

    #[test]
    fn detects_system_template() {
        let p = detect(&fixture("system_template")).unwrap();
        assert_eq!(p.kinds, [LegacyKind::SystemTemplate]);
        assert!(p.data_dir.join("system_template/auto_map.json").is_file());
    }

    #[test]
    fn rejects_missing_config() {
        let d = detect(&fixture("regolith_config")).unwrap_err();
        assert_eq!(d[0].code, Code::MigrateUnsupported);
    }

    #[test]
    fn regolith_config_samples_parse() {
        for name in ["default", "packaging", "edu_world"] {
            let b = std::fs::read(fixture("regolith_config").join(format!("{name}.json"))).unwrap();
            let c: RegolithConfig = serde_json::from_slice(&b).unwrap();
            assert!(!c.regolith.profiles.is_empty(), "{name}");
        }
        let b = std::fs::read(fixture("regolith_config/packaging.json")).unwrap();
        let c: RegolithConfig = serde_json::from_slice(&b).unwrap();
        let p = &c.regolith.profiles["package"];
        assert_eq!(p.filters[0].profile.as_deref(), Some("default"));
        assert!(p.filters[1].extra.contains_key("when"));
        assert_eq!(
            c.regolith.filter_definitions["strip_debug"]
                .run_with
                .as_deref(),
            Some("python")
        );
        let b = std::fs::read(fixture("regolith_config/edu_world.json")).unwrap();
        let c: RegolithConfig = serde_json::from_slice(&b).unwrap();
        let e = c.regolith.profiles["edu"].export.as_ref().unwrap();
        assert_eq!(
            (e.target.as_deref(), e.build.as_deref()),
            (Some("world"), Some("education"))
        );
        assert_eq!(c.worlds, ["./worlds/*"]);
    }

    #[test]
    fn legacy_eval_round_trips() {
        let mut e = LegacyEval::default();
        e.outputs.insert(
            "RP/texts/en_US.lang".into(),
            EvalOutput {
                path: "RP/texts/en_US.lang".into(),
                body: Body::Text("a=b\n".into()),
                contributors: vec![Contributor {
                    map: RelPath::parse("packs/data/modular_mc/a/_map.ts").unwrap(),
                    entry: 0,
                    source: RelPath::parse("packs/data/modular_mc/a/en_US.lang").unwrap(),
                    policy: serde_json::from_str("\"append_end\"").unwrap(),
                    template: Template::Text(serde_json::json!({"x": 1})),
                    applied: Applied::Appended,
                }],
            },
        );
        e.env_read.insert("FIXTURE_EDITION".into(), None);
        let back: LegacyEval = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        assert_eq!(
            back.outputs["RP/texts/en_US.lang"].contributors[0].policy,
            Policy::AppendEnd
        );
    }
}
