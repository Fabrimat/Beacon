//! One-time source transforms applied by the migration planner (5.6). Pure functions over
//! `(path, bytes)`: `None` = not applicable or nothing changed, `Some((bytes, entry))` = the
//! rewritten file and its report line (`Report::codemods`). Paths are legacy pack paths
//! (`BP/...`, `RP/...`). Apply them in the profile's filter order (entity patch, then
//! `text_replacer`, as in the real profiles).

use super::Entry;
use crate::merge::strip_comments;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub type Rewrite = Option<(Vec<u8>, Entry)>;

fn entry(path: &str, why: impl Into<String>) -> Entry {
    Entry {
        path: path.into(),
        why: why.into(),
    }
}

fn parse_json(bytes: &[u8]) -> Option<Value> {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    serde_json::from_slice(&strip_comments(b)).ok()
}

fn to_json(v: &Value, indent: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let f = serde_json::ser::PrettyFormatter::with_indent(indent);
    let mut s = serde_json::Serializer::with_formatter(&mut out, f);
    v.serialize(&mut s).expect("Value serializes");
    out
}

// ---- text_replacer -------------------------------------------------------------------

/// One `text_replacer` batch: `(source, target)` pairs applied in a single simultaneous pass.
pub type Batch = Vec<(String, String)>;

/// Extensions `text_replacer` never touches (`EXCLUDED_EXTENSIONS` in its `main.py`),
/// matched case-sensitively against the last suffix, like `Path.suffix`.
const TEXT_REPLACER_EXCLUDED: [&str; 11] = [
    "fsb",
    "mp4",
    "ogg",
    "wav",
    "tga",
    "jpeg",
    "jpg",
    "png",
    "mcstructure",
    "nbt",
    "blend",
];

/// Parses a `text_replacer` filter's `settings` into its ordered batches: `rules` = one batch,
/// `batches: [{rules}]` = a sequence; both at once is an error (`parse_config`). A rule's
/// `source`/`target` is a string, `{type: literal, value}` or `{type: scope_variable, path,
/// value}`; `scope(path, value)` resolves the latter (the filter reads `data/<path>`). A
/// source repeated within a batch keeps its first position and its last target (the filter's
/// dict). Empty sources are dropped.
pub fn text_replacer_batches(
    settings: &Value,
    scope: &dyn Fn(&str, &str) -> Option<String>,
) -> Result<Vec<Batch>, String> {
    let rules = |v: &Value, at: &str| -> Result<Batch, String> {
        let mut batch: Batch = Vec::new();
        for (i, r) in v
            .as_array()
            .ok_or(format!("{at} is not a list"))?
            .iter()
            .enumerate()
        {
            let side = |k: &str| -> Result<String, String> {
                match r.get(k) {
                    Some(Value::String(s)) => Ok(s.clone()),
                    Some(o) => match (o["type"].as_str(), o["value"].as_str()) {
                        (Some("literal"), Some(v)) => Ok(v.into()),
                        (Some("scope_variable"), Some(v)) => o["path"]
                            .as_str()
                            .and_then(|p| scope(p, v))
                            .ok_or(format!("{at}[{i}].{k}: scope variable `{v}` not found")),
                        _ => Err(format!("{at}[{i}].{k}: invalid syntax")),
                    },
                    None => Err(format!("{at}[{i}]: needs `source` and `target`")),
                }
            };
            let (s, t) = (side("source")?, side("target")?);
            match batch.iter_mut().find(|(bs, _)| *bs == s) {
                Some(slot) => slot.1 = t,
                None if !s.is_empty() => batch.push((s, t)),
                None => {}
            }
        }
        Ok(batch)
    };
    match (settings.get("rules"), settings.get("batches")) {
        (Some(_), Some(_)) => Err("both `rules` and `batches` are set".into()),
        (Some(r), None) => Ok(vec![rules(r, "rules")?]),
        (None, Some(b)) => b
            .as_array()
            .ok_or("`batches` is not a list")?
            .iter()
            .enumerate()
            .map(|(i, b)| {
                rules(
                    b.get("rules")
                        .ok_or(format!("batches[{i}] has no `rules`"))?,
                    &format!("batches[{i}].rules"),
                )
            })
            .collect(),
        (None, None) => Ok(Vec::new()),
    }
}

/// `text_replacer` on one file: only `BP/` and `RP/` files, never `node_modules`, never the
/// excluded extensions, UTF-8 only. Each batch is one left-to-right pass where, at a position,
/// the first matching source (batch order) wins and replaced text is not rescanned; the next
/// batch sees the previous batch's output. Line endings are untouched.
pub fn text_replace(path: &str, bytes: &[u8], batches: &[Batch]) -> Rewrite {
    let mut parts = path.split('/');
    if !matches!(parts.next(), Some("BP" | "RP")) || path.split('/').any(|c| c == "node_modules") {
        return None;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name
        .rsplit_once('.')
        .is_some_and(|(_, e)| TEXT_REPLACER_EXCLUDED.contains(&e))
    {
        return None;
    }
    let original = std::str::from_utf8(bytes).ok()?;
    let mut text = original.to_string();
    for batch in batches.iter().filter(|b| !b.is_empty()) {
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            match batch.iter().find(|(s, _)| rest.starts_with(s.as_str())) {
                Some((s, t)) => {
                    out.push_str(t);
                    i += s.len();
                }
                None => {
                    let c = rest.chars().next().expect("non-empty");
                    out.push(c);
                    i += c.len_utf8();
                }
            }
        }
        text = out;
    }
    (text != original).then(|| (text.into_bytes(), entry(path, "text_replacer")))
}

// ---- shapescape_default --------------------------------------------------------------

const DEFAULT_FAMILIES: [&str; 2] = ["shapescape", "shapescape_"];

fn obj<'a>(m: &'a mut Map<String, Value>, k: &str) -> Option<&'a mut Map<String, Value>> {
    m.entry(k).or_insert_with(|| json!({})).as_object_mut()
}

fn add_families(type_family: &mut Value) {
    if !type_family.is_object() {
        *type_family = json!({});
    }
    let fam = type_family
        .as_object_mut()
        .expect("object")
        .entry("family")
        .or_insert_with(|| json!([]));
    if let Some(fam) = fam.as_array_mut() {
        for f in DEFAULT_FAMILIES {
            if !fam.iter().any(|x| x == f) {
                fam.push(json!(f));
            }
        }
    }
}

/// `shapescape_default` on a `BP/entities/**.json`: for every entity whose identifier
/// namespace is not `minecraft`, `minecraft:type_family` gains the families `shapescape` and
/// `shapescape_` (also in component groups that have one), and the `shapescape:despawn`
/// component group (`minecraft:instant_despawn`) and event (adds that group) are added when
/// absent. Output is 4-space JSON (the filter's `indent=4`), keys sorted. Idempotent.
pub fn entity_patch(path: &str, bytes: &[u8]) -> Rewrite {
    if !path.starts_with("BP/entities/") || !path.ends_with(".json") {
        return None;
    }
    let mut data = parse_json(bytes)?;
    let orig = data.clone();
    let ent = data.get_mut("minecraft:entity")?.as_object_mut()?;
    let id = ent.get("description")?.get("identifier")?.as_str()?;
    if id.split(':').next() == Some("minecraft") {
        return None;
    }
    let comps = obj(ent, "components")?;
    add_families(
        comps
            .entry("minecraft:type_family")
            .or_insert_with(|| json!({"family": []})),
    );
    let groups = obj(ent, "component_groups")?;
    for g in groups.values_mut() {
        if let Some(tf) = g.get_mut("minecraft:type_family") {
            add_families(tf);
        }
    }
    groups
        .entry("shapescape:despawn")
        .or_insert_with(|| json!({"minecraft:instant_despawn": {}}));
    obj(ent, "events")?
        .entry("shapescape:despawn")
        .or_insert_with(|| json!({"add": {"component_groups": ["shapescape:despawn"]}}));
    (data != orig).then(|| {
        (
            to_json(&data, b"    "),
            entry(path, "shapescape_default: families + despawn group/event"),
        )
    })
}

// ---- chunker_block_mappings ----------------------------------------------------------

/// `converted_from` removal (`chunker_block_mappings`, `strip_converted_from` default true) on
/// a `BP/blocks/**.json`: deleted from `minecraft:block.description` when truthy (a falsy value
/// stays, as in the filter). Output is tab-indented UTF-8. The Chunker mapping files the filter
/// also generates (`worlds/conversion_settings_*`) are not reproduced; the entry says so.
pub fn strip_converted_from(path: &str, bytes: &[u8]) -> Rewrite {
    if !path.starts_with("BP/blocks/") || !path.ends_with(".json") {
        return None;
    }
    let mut data = parse_json(bytes)?;
    let desc = data
        .get_mut("minecraft:block")?
        .get_mut("description")?
        .as_object_mut()?;
    let truthy = match desc.get("converted_from")? {
        Value::Null | Value::Bool(false) => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::Bool(true) => true,
    };
    if !truthy {
        return None;
    }
    desc.remove("converted_from");
    let why = "converted_from removed; chunker block_mappings files are not regenerated";
    Some((to_json(&data, b"\t"), entry(path, why)))
}

// ---- lang ----------------------------------------------------------------------------

fn lang_key(line: &str) -> Option<&str> {
    let l = line.trim_start_matches('\u{feff}');
    (!l.trim_start().starts_with("##")).then_some(())?;
    l.split_once('=')
        .map(|(k, _)| k.trim())
        .filter(|k| !k.is_empty())
}

/// `.lang` dedupe: an earlier line of a key that appears again later is removed (the last
/// value wins, as in the game); the report lists the keys. Other lines and line endings are
/// kept.
pub fn lang_dedupe(path: &str, bytes: &[u8]) -> Rewrite {
    if !path.ends_with(".lang") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut last: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(k) = lang_key(l) {
            last.insert(k, i);
        }
    }
    let mut dups = BTreeSet::new();
    let kept: String = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| match lang_key(l) {
            Some(k) if last[k] != *i => {
                dups.insert(k);
                false
            }
            _ => true,
        })
        .map(|(_, l)| *l)
        .collect();
    (!dups.is_empty()).then(|| {
        let why = format!(
            "duplicate lang keys, last value kept: {}",
            dups.into_iter().collect::<Vec<_>>().join(", ")
        );
        (kept.into_bytes(), entry(path, why))
    })
}

// ---- name_ninja ----------------------------------------------------------------------

/// What `autoNames` generates (config `beacon.autoNames`, mirrored by name_ninja's settings).
#[derive(Debug, Clone, Default)]
pub struct AutoNameOpts {
    pub entities: bool,
    pub items: bool,
    pub blocks: bool,
    /// Spawn eggs: `(prefix, postfix)`.
    pub spawn_eggs: Option<(String, String)>,
    /// name_ninja's `ignored_namespaces`; `None` = `["minecraft"]`.
    pub ignored_namespaces: Option<Vec<String>>,
}

/// name_ninja's display name for an identifier: namespace removed, `_` -> space, title-cased
/// like Python's `str.title()`. `None` without a namespace.
pub fn auto_name_value(identifier: &str) -> Option<String> {
    let (_, id) = identifier.split_once(':')?;
    let mut out = String::new();
    let mut prev_alpha = false;
    for c in id.replace('_', " ").chars() {
        if !c.is_alphabetic() {
            out.push(c);
        } else if prev_alpha {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        prev_alpha = c.is_alphabetic();
    }
    Some(out)
}

fn old_item_format(fv: Option<&str>) -> bool {
    fv.is_some_and(|v| {
        let mut p = v.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
        let mut n = || p.next().unwrap_or(0);
        [n(), n(), n()] < [1, 16, 100]
    })
}

/// The `.lang` `(key, value)` pairs name_ninja writes for one parsed `BP` entity / item /
/// block JSON (detected by its `minecraft:entity|item|block` root; format_version `< 1.16.100`
/// items use `item.<id>.name`, else `item.<id>`):
/// - entity `entity.<id>.name`, spawn egg `item.spawn_egg.entity.<id>.name`,
///   item `item.<id>` / `item.<id>.name`, block `tile.<id>.name`;
/// - value: the asset's explicit `description.name` when present (emitted even when the kind's
///   `auto_name` is off, as name_ninja does; for the spawn egg too, without prefix/postfix);
///   otherwise, if the kind is enabled, `auto_name_value(id)` (spawn egg: `prefix + that +
///   postfix`);
/// - order per entity: spawn egg, then entity (name_ninja gathers spawn eggs first).
///
/// Assets in the ignored namespaces (default `minecraft`) are skipped. This is the rule set
/// A2's `autoNames` registry must share; the migration strips exactly these lines.
pub fn auto_name_keys(json: &Value, opts: &AutoNameOpts) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (root, on) in [
        ("minecraft:entity", opts.entities),
        ("minecraft:item", opts.items),
        ("minecraft:block", opts.blocks),
    ] {
        let Some(desc) = json.get(root).and_then(|e| e.get("description")) else {
            continue;
        };
        let Some(id) = desc.get("identifier").and_then(Value::as_str) else {
            continue;
        };
        let ns = id.split(':').next().unwrap_or("");
        let ignored = match &opts.ignored_namespaces {
            Some(v) => v.iter().any(|n| n == ns),
            None => ns == "minecraft",
        };
        if ignored {
            continue;
        }
        let text = |k: &str| desc.get(k).and_then(Value::as_str).map(str::to_string);
        let (explicit, auto) = (text("name"), auto_name_value(id));
        let value = explicit.clone().or(auto.clone().filter(|_| on));
        let mut egg: Option<(String, String)> = None;
        let key = match root {
            "minecraft:entity" => {
                // name_ninja >= 1.3.1: `spawn_egg_name` first (raw), then `name`
                let v = text("spawn_egg_name")
                    .or_else(|| explicit.clone())
                    .or_else(|| {
                        let (pre, post) = opts.spawn_eggs.as_ref()?;
                        Some(format!("{pre}{}{post}", auto?))
                    });
                egg = v.map(|v| (format!("item.spawn_egg.entity.{id}.name"), v));
                format!("entity.{id}.name")
            }
            "minecraft:item" => {
                let old = old_item_format(json.get("format_version").and_then(Value::as_str));
                match old {
                    true => format!("item.{id}.name"),
                    false => format!("item.{id}"),
                }
            }
            _ => format!("tile.{id}.name"),
        };
        out.extend(egg);
        if let Some(v) = value {
            out.push((key, v));
        }
    }
    out
}

/// Removes from a `.lang` the lines `generated` (from [`auto_name_keys`]) would regenerate:
/// same key and same value. Lines with another value (hand-edited) stay.
pub fn strip_auto_names(path: &str, bytes: &[u8], generated: &BTreeMap<String, String>) -> Rewrite {
    if !path.ends_with(".lang") {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut gone = Vec::new();
    let kept: String = text
        .split_inclusive('\n')
        .filter(|l| {
            let hit = lang_key(l).filter(|k| {
                let v = l.split_once('=').map_or("", |(_, v)| v);
                let v = v.split("##").next().unwrap_or("").trim_end();
                generated.get(*k).is_some_and(|g| g == v)
            });
            if let Some(k) = hit {
                gone.push(k.to_string());
            }
            hit.is_none()
        })
        .collect();
    (!gone.is_empty()).then(|| {
        let why = format!("autoNames regenerates: {}", gone.join(", "));
        (kept.into_bytes(), entry(path, why))
    })
}

// ---- content_guide_generator ---------------------------------------------------------

/// `content_guide_generator` / `shapescape_content_guide_generator` (1.7.5, 1.8.0) on a
/// `BP/{entities,items,blocks}/**.json`: the documentation fields it reads are removed from
/// `description` (`clear_cgg_properties`): entities (identifier not `minecraft:`)
/// `description`, `category`, `locations`, and the spawn egg's `spawn_egg_description` /
/// `spawn_egg_player_facing` (also on vanilla ids when not spawnable); items and blocks
/// `description`, `player_facing`. Output is tab-indented, as the filter writes it. The
/// content guide itself (`OUTPUT.md`) is not generated.
pub fn strip_content_guide(path: &str, bytes: &[u8]) -> Rewrite {
    let (root, fields): (&str, &[&str]) = if path.starts_with("BP/entities/") {
        (
            "minecraft:entity",
            &["description", "category", "locations"],
        )
    } else if path.starts_with("BP/items/") {
        ("minecraft:item", &["description", "player_facing"])
    } else if path.starts_with("BP/blocks/") {
        ("minecraft:block", &["description", "player_facing"])
    } else {
        return None;
    };
    if !path.ends_with(".json") {
        return None;
    }
    let mut data = parse_json(bytes)?;
    let desc = data
        .get_mut(root)?
        .get_mut("description")?
        .as_object_mut()?;
    let id = desc.get("identifier")?.as_str()?.to_string();
    let vanilla = id.starts_with("minecraft:");
    let mut gone = Vec::new();
    if root == "minecraft:entity" {
        let spawnable = desc.get("is_spawnable") == Some(&Value::Bool(true));
        if !(vanilla && spawnable) {
            for k in ["spawn_egg_description", "spawn_egg_player_facing"] {
                gone.extend(desc.remove(k).map(|_| k));
            }
        }
    }
    if !(vanilla && root == "minecraft:entity") {
        for k in fields {
            gone.extend(desc.remove(*k).map(|_| *k));
        }
    }
    (!gone.is_empty()).then(|| {
        let why = format!(
            "content_guide_generator fields removed: {}",
            gone.join(", ")
        );
        (to_json(&data, b"\t"), entry(path, why))
    })
}
