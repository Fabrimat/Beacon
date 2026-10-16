//! Derived registries (5.3): files the game wants as one list per pack, derived from a plan's
//! items and the modules' `module.json`: `item_texture.json` / `terrain_texture.json` (as
//! `JsonDeep` fragments, so equal keys merge and different values are `MergeConflict`) and the
//! `tick` / `init` hooks. Runs per plan, before merge.

use crate::build::Plan;
use crate::config::{MODULE_FILE, ProjectConfig};
use crate::diag::{Code, Diagnostic};
use crate::model::{Body, ContentItem, Dest, Merge, OutputId, Pack};
use crate::path::{RelPath, Vfs};
use crate::route::Routed;
use serde_json::json;
use std::collections::BTreeMap;

/// `RP/textures/<reg>.json` fragment registering `key` -> `path` (pack path, no extension).
fn texture(i: &ContentItem, kind: &str, key: &str, path: &str, origin: &RelPath) -> ContentItem {
    let (reg, atlas) = if kind == "item" {
        ("item_texture", "atlas.items")
    } else {
        ("terrain_texture", "atlas.terrain")
    };
    item(
        i.dest.output.clone(),
        Pack::Resource,
        &format!("textures/{reg}.json"),
        Body::Json(
            json!({ "resource_pack_name": "vanilla", "texture_name": atlas,
            "texture_data": { key: { "textures": path } } }),
        ),
        Merge::JsonDeep,
        origin,
        &i.module,
    )
}

fn item(
    output: OutputId,
    pack: Pack,
    path: &str,
    body: Body,
    merge: Merge,
    origin: &RelPath,
    module: &RelPath,
) -> ContentItem {
    ContentItem {
        dest: Dest {
            output,
            pack,
            path: RelPath::parse(path).expect("static path"),
        },
        body,
        merge,
        origin: origin.clone(),
        module: module.clone(),
    }
}

fn strip_ext(p: &str) -> &str {
    p.rsplit_once('.').map_or(p, |(s, _)| s)
}

/// Extra items for `plan` (merged with the rest by `merge::resolve`), plus diagnostics.
pub(crate) fn derive(
    cfg: &ProjectConfig,
    routed: &Routed,
    plan: &Plan,
    items: &[ContentItem],
    vfs: &dyn Vfs,
) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    let _ = (plan, vfs);
    let (mut out, mut diags) = (Vec::new(), Vec::new());
    // raw convention files by origin: where they were emitted
    let raw: BTreeMap<&RelPath, &ContentItem> = items
        .iter()
        .filter(|i| matches!(&i.body, Body::Passthrough(p) if *p == i.origin))
        .map(|i| (&i.origin, i))
        .collect();
    let ns = |o: &str| {
        cfg.beacon
            .outputs
            .iter()
            .find(|d| d.id == o)
            .and_then(|d| d.namespace.as_deref())
            .unwrap_or(&cfg.namespace)
    };

    // convention: `x.png` beside `x.item.json` / `x.block.json`
    for (suffix, kind) in [(".item.json", "item"), (".block.json", "terrain")] {
        for (origin, it) in &raw {
            let Some(stem) = origin.file_name().strip_suffix(suffix) else {
                continue;
            };
            let png = origin
                .parent()
                .and_then(|d| d.join(&format!("{stem}.png")).ok());
            if let Some(t) = png.and_then(|p| raw.get(&p).copied()) {
                let key = format!("{}:{stem}", ns(&it.dest.output));
                out.push(texture(
                    it,
                    kind,
                    &key,
                    strip_ext(t.dest.path.as_str()),
                    &t.origin,
                ));
            }
        }
    }

    // module.json: explicit entries and hooks
    let mut hooks: BTreeMap<&OutputId, Vec<(i32, &RelPath, String, bool)>> = BTreeMap::new();
    for (module, mj) in &routed.modules {
        let Some(first) = items.iter().find(|i| &i.module == module) else {
            continue; // not in this plan
        };
        let mj_path = module.join(MODULE_FILE).unwrap_or_else(|_| module.clone());
        let bad = |code, msg: String| Diagnostic::new(code, msg).at(mj_path.clone(), None, None);
        for (kind, map) in [
            ("item", &mj.item_textures),
            ("terrain", &mj.terrain_textures),
        ] {
            for (k, png) in map {
                let t = module.join(png).ok().and_then(|p| raw.get(&p).copied());
                let Some(t) = t else {
                    diags.push(bad(
                        Code::ModuleJsonInvalid,
                        format!("{k:?}: {png:?} is not a routed texture of this module"),
                    ));
                    continue;
                };
                let key = if k.contains(':') {
                    k.clone()
                } else {
                    format!("{}:{k}", ns(&first.dest.output))
                };
                out.push(texture(
                    first,
                    kind,
                    &key,
                    strip_ext(t.dest.path.as_str()),
                    &mj_path,
                ));
            }
        }
        for (is_tick, map) in [(true, &mj.hooks.tick), (false, &mj.hooks.init)] {
            for (f, prio) in map {
                let func = module
                    .join(&format!("{f}.mcfunction"))
                    .ok()
                    .and_then(|p| raw.get(&p).copied())
                    .and_then(|i| i.dest.path.as_str().strip_prefix("functions/"))
                    .map(|p| strip_ext(p).to_owned());
                match func {
                    Some(v) => hooks
                        .entry(&first.dest.output)
                        .or_default()
                        .push((*prio, module, v, is_tick)),
                    None => diags.push(bad(
                        Code::HookUnknownFunction,
                        format!("hook {f:?}: no function file {f}.mcfunction in {module}"),
                    )),
                }
            }
        }
    }

    // hooks: one `tick.json` and one `beacon/init.mcfunction` per output, ordered by
    // priority (lower first), then origin. The project calls `function beacon/init` itself
    // (no scheduler).
    for (o, mut list) in hooks {
        list.sort();
        let origin = list[0].1;
        let of = |tick| -> Vec<&str> {
            list.iter()
                .filter(|h| h.3 == tick)
                .map(|h| h.2.as_str())
                .collect()
        };
        let mut add = |path: &str, body, merge| {
            out.push(item(
                o.clone(),
                Pack::Behavior,
                path,
                body,
                merge,
                origin,
                origin,
            ));
        };
        if !of(true).is_empty() {
            let body = Body::Json(json!({ "values": of(true) }));
            add("functions/tick.json", body, Merge::JsonDeep);
        }
        if !of(false).is_empty() {
            let text: String = of(false)
                .iter()
                .map(|f| format!("function {f}\n"))
                .collect();
            add(
                "functions/beacon/init.mcfunction",
                Body::Text(text),
                Merge::Error,
            );
        }
    }
    (out, diags)
}
