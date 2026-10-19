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
use std::collections::{BTreeMap, BTreeSet};

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
            if routed
                .modules
                .get(&it.module)
                .is_some_and(|m| !m.auto_textures)
            {
                continue;
            }
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

/// Python `str.title()`: the first letter of every run of letters upper, the rest lower.
fn title(s: &str) -> String {
    let mut prev = false;
    let mut out = String::new();
    for c in s.chars() {
        let alpha = c.is_alphabetic();
        if alpha && prev {
            out.extend(c.to_lowercase());
        } else if alpha {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        prev = alpha;
    }
    out
}

/// `name_ninja` `format_name`: `ns:some_thing` -> `Some Thing` (`name.split(":")[1]`).
fn format_name(id: &str) -> String {
    title(&id.split(':').nth(1).unwrap_or(id).replace('_', " "))
}

/// `json.dump(list, indent='\t')` (ensure_ascii: non-ASCII as `\uXXXX`), no trailing newline.
fn json_list(items: &[String]) -> String {
    let quote = |s: &String| {
        let mut o = String::new();
        for c in serde_json::to_string(s).expect("string json").chars() {
            if c.is_ascii() {
                o.push(c);
            } else {
                for u in c.encode_utf16(&mut [0u16; 2]) {
                    o.push_str(&format!("\\u{u:04x}"));
                }
            }
        }
        o
    };
    let body: Vec<String> = items.iter().map(|s| format!("\t{}", quote(s))).collect();
    format!("[\n{}\n]", body.join(",\n"))
}

/// `FormatVersion(v) < FormatVersion('1.16.100')`, missing parts 0.
fn old_format(v: &str) -> bool {
    let n: Vec<u32> = v.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let at = |i: usize| n.get(i).copied().unwrap_or(0);
    (at(0), at(1), at(2)) < (1, 16, 100)
}

/// Body of an item as text (BOM stripped).
fn text_of(i: &ContentItem, vfs: &dyn Vfs) -> String {
    let bytes = match &i.body {
        Body::Text(t) => return t.clone(),
        Body::Passthrough(p) => vfs.read(p).unwrap_or_default(),
        Body::Bytes(b) => b.clone(),
        Body::Json(v) => v.to_string().into_bytes(),
    };
    String::from_utf8_lossy(&bytes)
        .trim_start_matches('\u{feff}')
        .to_owned()
}

/// Registries that need the merged result (after `merge::resolve`): `autoNames` (the
/// `name_ninja` filter) and `texturesList` (the `texture_list` filter). Static outputs are
/// included as-is and never touched.
pub(crate) fn after_merge(
    cfg: &ProjectConfig,
    merged: &mut Vec<ContentItem>,
    vfs: &dyn Vfs,
    is_static: &dyn Fn(&OutputId) -> bool,
) {
    let outputs: BTreeSet<OutputId> = merged
        .iter()
        .filter(|i| i.dest.pack == Pack::Resource && !is_static(&i.dest.output))
        .map(|i| i.dest.output.clone())
        .collect();
    let origin = RelPath::parse(crate::config::CONFIG_FILE).expect("const");
    let rp = |o: &OutputId, i: &ContentItem, path: &str| {
        i.dest.output == *o && i.dest.pack == Pack::Resource && i.dest.path.as_str() == path
    };
    for o in &outputs {
        let mut new = Vec::new();
        if let Some(n) = &cfg.beacon.auto_names {
            let path = format!("texts/{}.lang", n.language);
            let at = merged.iter().position(|i| rp(o, i, &path));
            let mut text = at.map(|k| text_of(&merged[k], vfs)).unwrap_or_default();
            let mut known: BTreeSet<String> = text
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_owned()))
                .collect();
            let mut added = false;
            for (k, v) in auto_name_lines(n, merged, o, vfs) {
                if known.insert(k.clone()) {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&format!("{k}={v}\t##\n"));
                    added = true;
                }
            }
            if added {
                match at {
                    Some(k) => merged[k].body = Body::Text(text),
                    None => new.push((path, Body::Text(text), Merge::LangLines)),
                }
            }
        }
        if cfg.beacon.textures_list {
            let tex = |ext: &str| {
                let mut v: Vec<String> = merged
                    .iter()
                    .filter(|i| i.dest.output == *o && i.dest.pack == Pack::Resource)
                    .map(|i| i.dest.path.as_str())
                    .filter(|p| p.starts_with("textures/") && p.ends_with(ext))
                    .map(|p| strip_ext(p).to_owned())
                    .collect();
                v.sort();
                v
            };
            // texture_list.py: every png, then every tga (each sorted here; glob order there)
            let list = [tex(".png"), tex(".tga")].concat();
            if !list.is_empty() {
                let path = "textures/textures_list.json";
                merged.retain(|i| !rp(o, i, path));
                new.push((path.into(), Body::Text(json_list(&list)), Merge::Error));
            }
        }
        merged.extend(new.into_iter().map(|(path, body, merge)| ContentItem {
            dest: Dest {
                output: o.clone(),
                pack: Pack::Resource,
                path: RelPath::parse(&path).expect("static path"),
            },
            body,
            merge,
            origin: origin.clone(),
            module: origin.clone(),
        }));
    }
}

/// (identifier, `format_version`, explicit `description.name`)
/// (id, format_version, description.name, description.spawn_egg_name)
type Def = (String, String, Option<String>, Option<String>);

/// `name_ninja` translations for output `o`, in the filter's order: spawn eggs, items, blocks,
/// entities (each in dest order). `minecraft:` ids are skipped. A definition's explicit
/// `description/name` is the value (name_ninja pops it from the definition; we leave it).
fn auto_name_lines(
    n: &crate::config::AutoNames,
    merged: &[ContentItem],
    o: &OutputId,
    vfs: &dyn Vfs,
) -> Vec<(String, String)> {
    // folder -> (identifier, format_version, explicit name)
    let mut defs: BTreeMap<&str, Vec<Def>> = BTreeMap::new();
    for i in merged
        .iter()
        .filter(|i| i.dest.output == *o && i.dest.pack == Pack::Behavior)
    {
        let p = i.dest.path.as_str();
        let Some((dir, root)) = [
            ("entities/", "minecraft:entity"),
            ("items/", "minecraft:item"),
            ("blocks/", "minecraft:block"),
        ]
        .into_iter()
        .find(|(d, _)| p.starts_with(d) && p.ends_with(".json")) else {
            continue;
        };
        let text = text_of(i, vfs);
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&crate::merge::strip_comments(
            text.as_bytes(),
        )) else {
            continue;
        };
        let d = &v[root]["description"];
        let Some(id) = d["identifier"]
            .as_str()
            .filter(|id| id.contains(':') && !id.starts_with("minecraft:"))
        else {
            continue;
        };
        defs.entry(dir).or_default().push((
            id.to_owned(),
            v["format_version"].as_str().unwrap_or("1.20.0").to_owned(),
            d["name"].as_str().map(str::to_owned),
            d["spawn_egg_name"].as_str().map(str::to_owned),
        ));
    }
    let eggs = n.spawn_eggs.as_ref();
    let (pre, post) = eggs.map_or(("", ""), |e| (&e.prefix[..], &e.postfix[..]));
    // (folder, enabled, prefix, postfix, is spawn egg)
    let kinds = [
        ("entities/", eggs.is_some(), pre, post, true),
        ("items/", n.items, "", "", false),
        ("blocks/", n.blocks, "", "", false),
        ("entities/", n.entities, "", "", false),
    ];
    let mut out = Vec::new();
    for (dir, on, pre, post, egg) in kinds {
        for (id, fv, name, egg_name) in defs.get(dir).into_iter().flatten() {
            // name_ninja >= 1.3.1 tries `spawn_egg_name` first for eggs; older versions
            // only `name`. Supporting both is a superset (absent field = old behaviour).
            let name = match egg {
                true => egg_name.as_ref().or(name.as_ref()),
                false => name.as_ref(),
            };
            // gather_translations: an explicit name is always translated (any kind, no
            // prefix/postfix); `auto_name` only decides the fallback
            if name.is_none() && !on {
                continue;
            }
            let key = match dir {
                _ if egg => format!("item.spawn_egg.entity.{id}.name"),
                "items/" if !old_format(fv) => format!("item.{id}"),
                "items/" => format!("item.{id}.name"),
                "blocks/" => format!("tile.{id}.name"),
                _ => format!("entity.{id}.name"),
            };
            let value = name
                .cloned()
                .unwrap_or_else(|| format!("{pre}{}{post}", format_name(id)));
            out.push((key, value));
        }
    }
    out
}
