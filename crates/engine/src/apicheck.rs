//! API checks of `check` (phase 4, 8.4a): vanilla ids, script imports, module versions and
//! component names against the cached API index. Lexical and deliberately narrow: false
//! positives cost more than misses (member-level script checks are `check --full`'s).
//!
//! - `BCN0150` (Note): no index for the target's game version; **one** per check, and every
//!   other check is skipped. Never downloads.
//! - `BCN0152` (Warning): a `minecraft:` id in a typed reference (mcfunction `summon` / `give` /
//!   `clear` / `setblock` / `fill` / `type=`, BP recipe `item` / `result` / `input` / `output`)
//!   that is not in the vanilla ids of its kind (`item` also accepts block ids).
//! - `BCN0153` (Error): a named import from an indexed `@minecraft/*` module that the declared
//!   (`scriptModules`) version does not export.
//! - `BCN0155` (Warning): a named import whose export is `@deprecated` (exports only; member
//!   deprecations need types, so they stay with `check --full`).
//! - `BCN0154` (Error): a declared module version (or the installed
//!   `node_modules/@minecraft/*/package.json`) newer than the pinned stable/beta of the index.
//! - `BCN0151` (Note, not a warning): a `minecraft:` name in the top-level `components` of a BP
//!   entity / item / block that the schema does not list. A note because component availability
//!   depends on `format_version`, and `component_groups` / permutations are not looked at.

use crate::api_index::{self, ApiIndex};
use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::diag::{Code, Diagnostic, Severity};
use crate::merge::strip_comments;
use crate::model::{Body, ContentItem, Pack};
use crate::path::{RelPath, Vfs};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

/// All API diagnostics for `versions` (the targets' game versions, deduplicated by the caller).
pub(crate) fn check(
    root: &Path,
    cfg: &ProjectConfig,
    vfs: &dyn Vfs,
    files: &[&RelPath],
    items: &[ContentItem],
    versions: &BTreeSet<&str>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut noted = false;
    for v in versions {
        match api_index::shared(v) {
            Ok(ix) => {
                module_versions(&ix, root, cfg, v, &mut out);
                content(&ix, vfs, items, v, &mut out);
                scripts(&ix, cfg, vfs, files, &mut out);
            }
            Err(d) if !noted => {
                noted = true;
                out.push(d);
            }
            Err(_) => {}
        }
    }
    out
}

// ---------------------------------------------------------------- positions

fn line_col(text: &str, off: usize) -> (Option<u32>, Option<u32>) {
    let before = &text.as_bytes()[..off.min(text.len())];
    let line = before.iter().filter(|&&c| c == b'\n').count();
    let col = off
        - before
            .iter()
            .rposition(|&c| c == b'\n')
            .map_or(0, |p| p + 1);
    (Some(line as u32 + 1), Some(col as u32 + 1))
}

fn at(d: Diagnostic, path: &RelPath, text: &str, off: Option<usize>) -> Diagnostic {
    let (l, c) = off.map_or((None, None), |o| line_col(text, o));
    d.at(path.clone(), l, c)
}

// ---------------------------------------------------------------- module versions (0154)

/// `2.11.0-beta.1.26.50-stable` / `2.6.0` -> [2, 11, 0].
fn triple(v: &str) -> Option<[u64; 3]> {
    let mut it = v.split('-').next()?.split('.');
    let t = [it.next()?, it.next()?, it.next()?].map(|n| n.parse().ok());
    match (t, it.next()) {
        ([Some(a), Some(b), Some(c)], None) => Some([a, b, c]),
        _ => None,
    }
}

fn module_versions(
    ix: &ApiIndex,
    root: &Path,
    cfg: &ProjectConfig,
    game: &str,
    out: &mut Vec<Diagnostic>,
) {
    let newer = |name: &str, v: &str| -> Option<String> {
        let beta = v.contains("-beta");
        let pinned = ix
            .script_modules
            .get(name)?
            .iter()
            .filter(|m| m.beta == beta)
            .filter_map(|m| triple(&m.version))
            .max()?;
        (triple(v)? > pinned).then(|| {
            format!(
                "{}.{}.{}{}",
                pinned[0],
                pinned[1],
                pinned[2],
                if beta { "-beta" } else { "" }
            )
        })
    };
    let cfg_path = RelPath::parse(CONFIG_FILE).expect("const");
    for (name, v) in &cfg.beacon.script_modules {
        if let Some(p) = newer(name, v) {
            out.push(
                Diagnostic::new(
                    Code::ApiScriptModuleVersion,
                    format!(
                        "{name} {v} is newer than {p}, the latest known for game version {game}"
                    ),
                )
                .at(cfg_path.clone(), None, None),
            );
        }
    }
    for name in ix.script_modules.keys() {
        let rel = format!("node_modules/{name}/package.json");
        let Ok(p) = RelPath::parse(&rel) else {
            continue;
        };
        let Some(v) = read_small(&root.join(&rel))
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|j| j["version"].as_str().map(str::to_owned))
        else {
            continue;
        };
        if let Some(pin) = newer(name, &v) {
            // installed typings only: the bundle keeps @minecraft/* external and the manifest
            // pins `scriptModules`, so this never breaks a build or the game (a Warning, not an Error)
            let mut d = Diagnostic::new(
                Code::ApiScriptModuleVersion,
                format!(
                    "installed {name} {v} is newer than {pin}, the latest known for game version {game}"
                ),
            )
            .at(p, None, None);
            d.severity = Severity::Warning;
            out.push(d);
        }
    }
}

fn read_small(p: &Path) -> Option<Vec<u8>> {
    let md = std::fs::metadata(p).ok()?;
    (md.is_file() && md.len() <= 1 << 20)
        .then(|| std::fs::read(p).ok())
        .flatten()
}

// ---------------------------------------------------------------- content (0151, 0152)

fn ids_of<'a>(ix: &'a ApiIndex, kind: &str) -> Vec<&'a BTreeSet<String>> {
    let (one, many) = match kind {
        "entity" => ("entity", "entities"),
        _ => (kind, if kind == "item" { "items" } else { "blocks" }),
    };
    let mut v: Vec<_> = [one, many]
        .into_iter()
        .filter_map(|k| ix.vanilla.get(k))
        .collect();
    if kind == "item" {
        // block items are items too (recipes, `give`)
        v.extend(
            ["block", "blocks"]
                .into_iter()
                .filter_map(|k| ix.vanilla.get(k)),
        );
    }
    v
}

/// `minecraft:wool:3` -> `minecraft:wool`.
fn vanilla_id(s: &str) -> Option<&str> {
    const NS: &str = "minecraft:";
    let rest = s.strip_prefix(NS)?;
    let n = rest
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.')))
        .unwrap_or(rest.len());
    (n > 0).then(|| &s[..NS.len() + n])
}

fn unknown_id(ix: &ApiIndex, kind: &str, id: &str) -> bool {
    let sets = ids_of(ix, kind);
    !sets.is_empty() && !sets.iter().any(|s| s.contains(id))
}

/// Typed references of a mcfunction: (kind, byte offset of the id).
fn command_ids(text: &str) -> Vec<(&'static str, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        if line.trim_start().starts_with('#') {
            continue;
        }
        let words: Vec<(usize, &str)> = line
            .split_whitespace()
            .map(|w| (w.as_ptr() as usize - line.as_ptr() as usize, w))
            .collect();
        for (k, &(off, w)) in words.iter().enumerate() {
            let back = |n: usize| k.checked_sub(n).map(|i| words[i].1);
            let kind = match (back(1), back(2), back(4), back(7)) {
                (Some("summon"), ..) => Some("entity"),
                (_, Some("give" | "clear"), ..) => Some("item"),
                (_, _, Some("setblock"), _) | (.., Some("fill")) => Some("block"),
                _ => None,
            };
            if let Some(kind) = kind {
                out.push((kind, start + off));
            }
            let mut from = 0;
            while let Some(p) = w[from..].find("type=") {
                let s = from + p + "type=".len();
                let s = s + usize::from(w[s..].starts_with('!'));
                out.push(("entity", start + off + s));
                from = s;
            }
        }
    }
    out
}

fn walk_recipe(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                match v.as_str() {
                    Some(s) if matches!(k.as_str(), "item" | "result" | "input" | "output") => {
                        out.push(s.to_owned())
                    }
                    _ => walk_recipe(v, out),
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|v| walk_recipe(v, out)),
        _ => {}
    }
}

fn content(
    ix: &ApiIndex,
    vfs: &dyn Vfs,
    items: &[ContentItem],
    game: &str,
    out: &mut Vec<Diagnostic>,
) {
    let mut seen = BTreeSet::new();
    for it in items {
        let Body::Passthrough(p) = &it.body else {
            continue;
        };
        if it.dest.pack != Pack::Behavior || !seen.insert(p) {
            continue;
        }
        let dest = it.dest.path.as_str();
        let is_fn = dest.ends_with(".mcfunction");
        if !(is_fn || dest.ends_with(".json")) {
            continue;
        }
        let Ok(bytes) = vfs.read(p) else { continue };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let bad = |kind: &str, id: &str, off: Option<usize>, out: &mut Vec<Diagnostic>| {
            out.push(at(
                Diagnostic::new(
                    Code::ApiUnknownVanillaId,
                    format!("{id:?} is not a vanilla {kind} id in game version {game}"),
                ),
                p,
                text,
                off,
            ));
        };
        if is_fn {
            for (kind, off) in command_ids(text) {
                if let Some(id) = vanilla_id(&text[off..]).filter(|id| unknown_id(ix, kind, id)) {
                    bad(kind, id, Some(off), out);
                }
            }
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&strip_comments(&bytes)) else {
            continue;
        };
        if dest.starts_with("recipes/") {
            let mut ids = Vec::new();
            walk_recipe(&v, &mut ids);
            let mut done = BTreeSet::new();
            for raw in ids {
                if let Some(id) = vanilla_id(&raw)
                    && unknown_id(ix, "item", id)
                    && done.insert(id.to_owned())
                {
                    bad("item", id, text.find(&format!("\"{raw}\"")), out);
                }
            }
        }
        for (top, kind) in [
            ("minecraft:entity", "entity"),
            ("minecraft:item", "item"),
            ("minecraft:block", "block"),
        ] {
            let (Some(comps), Some(known)) =
                (v[top]["components"].as_object(), ix.components.get(kind))
            else {
                continue;
            };
            for name in comps
                .keys()
                .filter(|n| n.starts_with("minecraft:") && !known.contains_key(*n))
            {
                let mut d = at(
                    Diagnostic::new(
                        Code::ApiUnknownComponent,
                        format!("{name:?} is not a known {kind} component in game version {game}"),
                    ),
                    p,
                    text,
                    text.find(&format!("\"{name}\"")),
                );
                d.severity = Severity::Note; // format_version, component groups, permutations
                out.push(d);
            }
        }
    }
}

// ---------------------------------------------------------------- scripts (0153, 0155)

struct Import<'a> {
    module: &'a str,
    name: &'a str,
    at: usize,
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    loop {
        match (b.get(i), b.get(i + 1)) {
            (Some(c), _) if c.is_ascii_whitespace() => i += 1,
            (Some(b'/'), Some(b'/')) => {
                i = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p)
            }
            (Some(b'/'), Some(b'*')) => {
                i = b[i + 2..]
                    .windows(2)
                    .position(|w| w == b"*/")
                    .map_or(b.len(), |p| i + 2 + p + 2)
            }
            _ => return i,
        }
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

fn ident(b: &[u8], i: usize) -> (usize, usize) {
    let i = i.min(b.len());
    let e = b[i..]
        .iter()
        .position(|&c| !is_ident(c))
        .map_or(b.len(), |p| i + p);
    (i, e)
}

type Named<'a> = Vec<(&'a str, usize)>;

/// `import [type] [def,] { A, type B, C as D } from "m"` at `i` (after `import`).
fn parse_import(text: &str, mut i: usize) -> Option<(Named<'_>, &str)> {
    let b = text.as_bytes();
    i = skip_ws(b, i);
    let (s, e) = ident(b, i);
    if e > s {
        let after = skip_ws(b, e);
        if &text[s..e] == "type" && b.get(after) == Some(&b'{') {
            i = after;
        } else {
            i = after; // default import
            if b.get(i) != Some(&b',') {
                return None;
            }
            i = skip_ws(b, i + 1);
        }
    }
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let mut names = Vec::new();
    loop {
        i = skip_ws(b, i);
        if b.get(i) == Some(&b'}') {
            i += 1;
            break;
        }
        let (s, e) = ident(b, i);
        if e == s {
            return None;
        }
        let (mut ns, mut ne) = (s, e);
        let (s2, e2) = ident(b, skip_ws(b, e));
        if &text[s..e] == "type" && e2 > s2 && &text[s2..e2] != "as" {
            (ns, ne) = (s2, e2);
            i = e2;
        } else {
            i = e;
        }
        names.push((&text[ns..ne], ns));
        i = skip_ws(b, i);
        let (s, e) = ident(b, i);
        if &text[s..e] == "as" {
            let (s, e) = ident(b, skip_ws(b, e));
            if e == s {
                return None;
            }
            i = skip_ws(b, e);
        }
        if b.get(i) == Some(&b',') {
            i += 1;
        }
    }
    i = skip_ws(b, i);
    let (s, e) = ident(b, i);
    if &text[s..e] != "from" {
        return None;
    }
    i = skip_ws(b, e);
    let q = *b.get(i).filter(|&&c| c == b'"' || c == b'\'')?;
    let end = b[i + 1..].iter().position(|&c| c == q || c == b'\n')? + i + 1;
    (b[end] == q).then(|| (names, &text[i + 1..end]))
}

/// Named imports outside comments and strings.
fn imports(text: &str) -> Vec<Import<'_>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let j = skip_ws(b, i);
        if j != i {
            i = j;
            continue;
        }
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q && !(q != b'`' && b[i] == b'\n') {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            c if is_ident(c) => {
                let (s, e) = ident(b, i);
                if &text[s..e] == "import"
                    && (s == 0 || b[s - 1] != b'.')
                    && let Some((names, module)) = parse_import(text, e)
                {
                    out.extend(
                        names
                            .into_iter()
                            .map(|(name, at)| Import { module, name, at }),
                    );
                }
                i = e;
            }
            _ => i += 1,
        }
    }
    out
}

fn scripts(
    ix: &ApiIndex,
    cfg: &ProjectConfig,
    vfs: &dyn Vfs,
    files: &[&RelPath],
    out: &mut Vec<Diagnostic>,
) {
    // module -> exports of the declared version
    let declared: Vec<_> = cfg
        .beacon
        .script_modules
        .iter()
        .filter_map(|(n, v)| {
            let m = ix.script_modules.get(n)?.iter().find(|m| &m.version == v)?;
            Some((n.as_str(), v.as_str(), m))
        })
        .collect();
    if declared.is_empty() {
        return;
    }
    for p in files {
        let n = p.file_name();
        if !(n.ends_with(".ts") || n.ends_with(".js") || n.ends_with(".mjs"))
            || n.ends_with(".d.ts")
        {
            continue;
        }
        let Ok(bytes) = vfs.read(p) else { continue };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };
        if !text.contains("@minecraft/") {
            continue;
        }
        for imp in imports(text) {
            let Some(&(name, ver, m)) = declared.iter().find(|d| d.0 == imp.module) else {
                continue;
            };
            let d = match m.exports.get(imp.name) {
                None => Diagnostic::new(
                    Code::ApiUnknownScriptMember,
                    format!("{name} {ver} does not export {:?}", imp.name),
                ),
                Some(it) if it.deprecated => Diagnostic::new(
                    Code::ApiDeprecated,
                    format!("{:?} is deprecated in {name} {ver}", imp.name),
                ),
                Some(_) => continue,
            };
            out.push(at(d, p, text, Some(imp.at)));
        }
    }
}
