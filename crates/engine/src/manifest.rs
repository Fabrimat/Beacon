//! Workstream A2: generated `manifest.json` and base lang (4.1, 5.9).

use crate::config::ProjectConfig;
use crate::lock::{Lockfile, dev_uuid};
use crate::model::{Body, ContentItem, Dest, Edition, Merge, OutputId, Pack};
use crate::path::RelPath;
use serde_json::{Value, json};
use std::path::Path;

pub struct ManifestInput<'a> {
    pub cfg: &'a ProjectConfig,
    pub lock: &'a Lockfile,
    pub output: &'a OutputId,
    pub edition: Edition,
    /// The target's `targetVersion` (else the project's): `min_engine_version`.
    pub target_version: &'a str,
    /// Dev build: dev UUIDs derived from this worktree root; versions = last release.
    pub dev_worktree: Option<&'a Path>,
    /// BP-relative path of the script bundle entry (e.g. `scripts/main.js`), if any: adds the
    /// script module and `beacon.scriptModules` dependencies.
    pub script_entry: Option<&'a str>,
    pub has_bp: bool,
    pub has_rp: bool,
}

fn min_engine(m: &ManifestInput) -> Vec<u32> {
    let mut v: Vec<u32> = m
        .target_version
        .split('.')
        .filter_map(|p| p.parse().ok())
        .take(3)
        .collect();
    v.resize(3, 0);
    v
}

/// `manifest.json` for each pack of the output, base `texts/en_US.lang` (`pack.name`,
/// `pack.description`, `Merge::LangLines` so module lang files merge into it) and
/// `pack_icon.png` (passthrough) when `packMeta.icon` is set. `texts/languages.json` is NOT
/// generated here: the orchestrator derives it after merge from the `texts/*.lang` dests.
/// `origin` and `module` of the items are `config.json`.
pub fn generate(m: &ManifestInput) -> Vec<ContentItem> {
    let (out, ed) = (m.output.as_str(), m.edition);
    // A missing lock entry (generate called for an output/edition `ensure` did not cover)
    // skips that pack: the signature has no diagnostics channel and `ensure` guarantees entries.
    let id = |release: Option<String>| -> Option<String> {
        let r = release?;
        Some(
            m.dev_worktree
                .map_or_else(|| r.clone(), |w| dev_uuid(&r, w)),
        )
    };
    let meta = m.cfg.beacon.pack_meta.as_ref();
    let clean = |s: &str| s.replace(['\r', '\n'], " ");
    let name = clean(meta.and_then(|p| p.name.as_deref()).unwrap_or(&m.cfg.name));
    let desc = clean(meta.and_then(|p| p.description.as_deref()).unwrap_or(""));
    let icon = meta
        .and_then(|p| p.icon.as_deref())
        .and_then(|i| RelPath::parse(i.trim_start_matches("./")).ok());
    let origin = RelPath::parse("config.json").expect("static path");
    let min = min_engine(m);
    // versions: the last release (`beacon pack` puts this run's release in `m.lock` first)
    let version = |p: Pack| m.lock.version_of(out, p, ed);

    let mut items = Vec::new();
    for (pack, present) in [(Pack::Behavior, m.has_bp), (Pack::Resource, m.has_rp)] {
        if !present {
            continue;
        }
        let bp = pack == Pack::Behavior;
        let Some(manifest) = (|| -> Option<Value> {
            let mut modules = vec![json!({
                "type": if bp { "data" } else { "resources" },
                "uuid": id(m.lock.module_uuid(out, pack, ed))?,
                "version": version(pack),
            })];
            let mut deps = Vec::new();
            if bp {
                if m.has_rp {
                    deps.push(json!({
                        "uuid": id(m.lock.pack_uuid(out, Pack::Resource, ed))?,
                        "version": version(Pack::Resource),
                    }));
                }
                if let Some(entry) = m.script_entry {
                    modules.push(json!({
                        "type": "script",
                        "language": "javascript",
                        "entry": entry,
                        "uuid": id(m.lock.script_module_uuid(out))?,
                        "version": version(pack),
                    }));
                    deps.extend(
                        m.cfg
                            .beacon
                            .script_modules
                            .iter()
                            .map(|(n, v)| json!({"module_name": n, "version": v})),
                    );
                }
            }
            let mut manifest = json!({
                "format_version": 2,
                "header": {
                    "name": "pack.name",
                    "description": "pack.description",
                    "uuid": id(m.lock.pack_uuid(out, pack, ed))?,
                    "version": version(pack),
                    "min_engine_version": min,
                },
                "modules": modules,
            });
            if !deps.is_empty() {
                manifest["dependencies"] = Value::Array(deps);
            }
            Some(manifest)
        })() else {
            continue;
        };
        let mut item = |path: &str, body: Body, merge: Merge| {
            items.push(ContentItem {
                dest: Dest {
                    output: m.output.clone(),
                    pack,
                    path: RelPath::parse(path).expect("static path"),
                },
                body,
                merge,
                origin: origin.clone(),
                module: origin.clone(),
            });
        };
        item("manifest.json", Body::Json(manifest), Merge::Error);
        item(
            "texts/en_US.lang",
            Body::Text(format!("pack.name={name}\npack.description={desc}\n")),
            Merge::LangLines,
        );
        if let Some(i) = &icon {
            item("pack_icon.png", Body::Passthrough(i.clone()), Merge::Error);
        }
    }
    items
}
