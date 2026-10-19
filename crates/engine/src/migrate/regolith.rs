//! Regolith profiles -> Beacon `targets` (5.6, 14.3). One-time; Regolith is never run.
//!
//! A profile is flattened (nested `profile` steps inlined) and classified by its `export`:
//!
//! | Regolith profile | Beacon target(s) |
//! |---|---|
//! | `export.target: development`, `build: education` | `edu-dev` (education, `dev`) |
//! | `export.target: development`, profile named `default` | `dev` |
//! | other `development` profiles (e.g. `debug`) | dropped: the default plus debug-only filters |
//! | `export.target: world` | `world` / `edu` (`build: education`), `mcworld`, `worldName` |
//! | `exact` / `local` with a `pack_anything` run | one target, artifact from its `output` extension |
//! | `exact` / `local`, `readOnly: true` | dropped: a dev mirror (e.g. `debugger`), `dev` replaces it |
//! | other `exact` / `local` (`packaging`, `bedrock_packaging`) | world pipeline, below |
//!
//! World pipeline: one run per `shapescape_world_settings` step (none = one anonymous run).
//! A run whose world folder ends in `_EDU` (or whose profile has `build: education`) is the
//! `edu` target, any other run the `world` target; a `pack/release_config.json` (the step's
//! `release_config_path`, else `../pack/release_config.json`) marks a distributable product, so
//! each also gets a `template` / `edu-template` (`mctemplate`). Per target:
//! - `world`: the folder of the step's `level_dat_path`, else the release config's
//!   `exported_world_edu|exported_world_bedrock|exported_world|testing_world` under `worlds/`;
//! - `worldName`: release config `product_name` (what the filter writes to `LevelName`);
//! - `levelDat`: the tags `shapescape_world_settings` writes (`level_dat_update` below);
//! - `targetVersion` (non-edu runs): `convert_to_bedrock`'s `target_format_version`. Its other
//!   edits (edu flags 0, `experiments.*` 0, `eduOffer` 0, manifests' `min_engine_version`) are
//!   the Bedrock edition transform (5.9) and need nothing here.
//!
//! Targets of several profiles with the same name merge (first value of a key wins), so
//! `packaging` (both worlds) and `bedrock_packaging` (adds the converted `targetVersion`)
//! become one `world` target. Filters that were in a profile get one diagnostic each.

use super::{FilterRun, RegolithConfig};
use crate::diag::{Code, Diagnostic};
use crate::merge::strip_comments;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Filters whose work is done elsewhere (planner, codemods, `autoNames`, targets).
const HANDLED: [&str; 13] = [
    "content_guide_generator",
    "shapescape_content_guide_generator",
    "modular_mc",
    "system_template",
    "system_template_esbuild",
    "subfunctions",
    "name_ninja",
    "shapescape_default",
    "text_replacer",
    "texture_list",
    "shapescape_world_settings",
    "convert_to_bedrock",
    "pack_anything",
];

/// Filters with no effect Beacon needs.
const DROPPED: [(&str, &str); 4] = [
    ("print_lang_diff", "a dev-time lang report"),
    ("debug_say_function_name", "a debug-only mcfunction rewrite"),
    (
        "shapescape_copy_files",
        "copies node_modules for the debugger; scripts bundle natively",
    ),
    (
        "copy_everywhere",
        "copies node_modules for the debugger; scripts bundle natively",
    ),
];

/// Maps every profile of `cfg` to targets. `root` (the Regolith project directory) lets the
/// release config and `convert_to_bedrock` data be read; without it those parts are skipped.
pub fn map_profiles(cfg: &RegolithConfig, root: Option<&Path>) -> (Value, Vec<Diagnostic>) {
    let mut targets: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut diags = Vec::new();
    let mut filters = BTreeSet::new();
    for (name, prof) in &cfg.regolith.profiles {
        let steps = flatten(cfg, name, &mut vec![name.clone()]);
        filters.extend(steps.iter().filter_map(|s| s.filter.clone()));
        let export = prof.export.as_ref();
        let edu_build = export.and_then(|e| e.build.as_deref()) == Some("education");
        let edition = if edu_build { "education" } else { "bedrock" };
        let read_only = export.and_then(|e| e.extra.get("readOnly")) == Some(&Value::Bool(true));
        let mut found: Vec<(String, Value)> = Vec::new();
        let mut drop = |why: &str| {
            let msg = format!("profile `{name}` has no Beacon target: {why}");
            diags.push(Diagnostic::new(Code::MigrateDropped, msg));
        };
        match export
            .and_then(|e| e.target.as_deref())
            .unwrap_or("development")
        {
            "development" if edu_build => found.push((
                "edu-dev".into(),
                json!({"edition": edition, "artifact": "dev"}),
            )),
            "development" if name == "default" => {
                found.push(("dev".into(), json!({"edition": edition, "artifact": "dev"})))
            }
            "development" => drop("the default profile plus debug-only filters"),
            "world" => {
                let mut t = json!({"edition": edition, "artifact": "mcworld"});
                if let Some(w) = export.and_then(|e| e.world_name.as_ref()) {
                    t["worldName"] = json!(w);
                }
                found.push((if edu_build { "edu" } else { "world" }.into(), t));
            }
            _ => match pack_anything_artifact(&steps) {
                Some(art) => found.push((
                    name.clone(),
                    json!({"edition": edition, "artifact": art, "outputs": ["*"]}),
                )),
                None if read_only => {
                    drop("a read-only export mirror; the `dev` target replaces it")
                }
                None => found.extend(world_pipeline(&steps, edu_build, root)),
            },
        }
        for (t, v) in found {
            let Value::Object(v) = v else { continue };
            let slot = targets.entry(t).or_default();
            for (k, v) in v {
                slot.entry(k).or_insert(v);
            }
        }
    }
    for (name, t) in &targets {
        let art = t.get("artifact").and_then(Value::as_str);
        if matches!(art, Some("mcworld" | "mctemplate")) && !t.contains_key("world") {
            let msg = format!("targets.{name}: set `world` to the world folder (not derivable)");
            diags.push(Diagnostic::new(Code::MigrateToPort, msg));
        }
    }
    for f in filters.iter().filter(|f| !HANDLED.contains(&f.as_str())) {
        diags.push(match DROPPED.iter().find(|(n, _)| n == f) {
            Some((_, why)) => Diagnostic::new(Code::MigrateDropped, format!("filter `{f}`: {why}")),
            None => Diagnostic::new(
                Code::MigrateToPort,
                format!("filter `{f}` has no Beacon equivalent: port it or keep it as a legacy_filter plugin"),
            ),
        });
    }
    let targets = targets.into_iter().map(|(k, v)| (k, Value::Object(v)));
    (Value::Object(targets.collect()), diags)
}

/// The profile's steps with nested `profile` steps inlined in place (cycles skipped).
fn flatten<'a>(cfg: &'a RegolithConfig, name: &str, seen: &mut Vec<String>) -> Vec<&'a FilterRun> {
    let mut out = Vec::new();
    for step in cfg
        .regolith
        .profiles
        .get(name)
        .map_or(&[][..], |p| &p.filters[..])
    {
        match step.profile.as_deref() {
            Some(p) if !seen.iter().any(|s| s == p) => {
                seen.push(p.to_string());
                out.extend(flatten(cfg, p, seen));
            }
            Some(_) => {}
            None => out.push(step),
        }
    }
    out
}

fn setting<'a>(step: &'a FilterRun, key: &str) -> Option<&'a str> {
    step.settings.as_ref()?.get(key)?.as_str()
}

/// `pack_anything`'s `output` extension -> artifact.
fn pack_anything_artifact(steps: &[&FilterRun]) -> Option<&'static str> {
    let step = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("pack_anything"))?;
    let out = setting(step, "output")?;
    ["mcaddon", "mcpack", "mcworld", "mctemplate"]
        .into_iter()
        .find(|e| out.ends_with(&format!(".{e}")))
}

fn read_json(root: &Path, rel: &str) -> Option<Value> {
    let b = std::fs::read(root.join(rel)).ok()?;
    serde_json::from_slice(&strip_comments(
        b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b),
    ))
    .ok()
}

/// `1.20.30` from `[1, 20, 30]`.
fn version_string(v: &Value) -> Option<String> {
    let parts: Option<Vec<String>> = v
        .as_array()?
        .iter()
        .map(|n| Some(n.as_u64()?.to_string()))
        .collect();
    parts.filter(|p| !p.is_empty()).map(|p| p.join("."))
}

/// `worlds/X` from `worlds/X/level.dat`; `None` for `../level.dat` and the like.
fn world_folder(level_dat_path: &str) -> Option<String> {
    let p = level_dat_path.replace('\\', "/");
    let dir = p.trim_start_matches("./").rsplit_once('/')?.0.to_string();
    (!dir.is_empty() && !dir.split('/').any(|c| c == ".." || c == ".")).then_some(dir)
}

/// The tags `shapescape_world_settings` writes from a release config (its `update_level_dat`).
/// `MultiplayerGameIntent`/`XBLBroadcastIntent` follow the filter's code (true -> 1 / 3), not
/// its docstring.
fn level_dat_update(rc: &Value) -> Map<String, Value> {
    let mut m = Map::new();
    let st = &rc["settings"];
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            m.insert(k.into(), v);
        }
    };
    let b = |k: &str| st.get(k).filter(|v| v.is_boolean()).cloned();
    let idx = |k: &str, names: &[&str]| {
        let s = st.get(k)?.as_str()?;
        Some(json!(names.iter().position(|n| *n == s)?))
    };
    let mp = b("multiplayer").and_then(|v| v.as_bool());
    put("MultiplayerGameIntent", mp.map(|v| json!(v)));
    put(
        "XBLBroadcastIntent",
        mp.map(|v| json!(if v { 3 } else { 1 })),
    );
    put("commandsEnabled", b("cheats"));
    put("sendcommandfeedback", b("sendCommandFeedback"));
    put("commandblockoutput", b("commandBlockOutput"));
    put("dodaylightcycle", b("doDayLightCycle"));
    put("domobspawning", b("doMobSpawning"));
    put(
        "Difficulty",
        idx("difficulty", &["peaceful", "easy", "normal", "hard"]),
    );
    put(
        "GameType",
        idx("defaultGamemode", &["survival", "creative", "adventure"]),
    );
    put(
        "EducationCreatorId",
        rc.get("edu_creator_uuid")
            .filter(|v| v.is_string())
            .cloned(),
    );
    put(
        "EducationWorldCreatorId",
        rc.get("edu_world_creator_uuid")
            .filter(|v| v.is_string())
            .cloned(),
    );
    m
}

fn world_pipeline(
    steps: &[&FilterRun],
    edu_build: bool,
    root: Option<&Path>,
) -> Vec<(String, Value)> {
    let convert = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("convert_to_bedrock"))
        .and_then(|s| read_json(root?, setting(s, "release_config_path")?))
        .and_then(|c| version_string(c.get("target_format_version")?));
    let runs: Vec<Option<&FilterRun>> = match steps
        .iter()
        .filter(|s| s.filter.as_deref() == Some("shapescape_world_settings"))
        .collect::<Vec<_>>()
    {
        v if v.is_empty() => vec![None],
        v => v.into_iter().map(|s| Some(*s)).collect(),
    };
    let mut out = Vec::new();
    for run in runs {
        let folder = run
            .and_then(|s| setting(s, "level_dat_path"))
            .and_then(world_folder);
        let edu = edu_build
            || folder
                .as_deref()
                .is_some_and(|f| f.to_uppercase().ends_with("_EDU"));
        let rc = root.and_then(|r| {
            let rel = run.and_then(|s| setting(s, "release_config_path"));
            read_json(r, rel.unwrap_or("../pack/release_config.json"))
        });
        let named = |k: &str| rc.as_ref()?.get(k)?.as_str().map(|n| format!("worlds/{n}"));
        let keys: &[&str] = if edu {
            &["exported_world_edu"]
        } else {
            &["exported_world_bedrock", "exported_world", "testing_world"]
        };
        let folder = folder.or_else(|| keys.iter().find_map(|k| named(k)));
        let mut t =
            json!({"edition": if edu { "education" } else { "bedrock" }, "artifact": "mcworld"});
        if let Some(f) = folder {
            t["world"] = json!(f);
        }
        if let Some(rc) = &rc {
            if let Some(n) = rc.get("product_name") {
                t["worldName"] = n.clone();
            }
            let lv = level_dat_update(rc);
            if !lv.is_empty() {
                t["levelDat"] = Value::Object(lv);
            }
        }
        if let (false, Some(v)) = (edu, &convert) {
            t["targetVersion"] = json!(v);
        }
        let (world, template) = if edu {
            ("edu", "edu-template")
        } else {
            ("world", "template")
        };
        if rc.is_some() {
            let mut tt = t.clone();
            tt["artifact"] = json!("mctemplate");
            out.push((template.to_string(), tt));
        }
        out.push((world.to_string(), t));
    }
    out
}
