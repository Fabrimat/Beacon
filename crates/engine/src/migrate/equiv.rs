//! Equivalence check (5.6): the migrated project's packs vs the legacy output, after
//! normalization (parsed JSON equal, `.lang` by key, other text exact, NBT by key, script
//! bundles after sourcemap/path normalization). Owned by phase 3 A3.

use crate::merge::strip_comments;
use crate::nbt::{self, Tag};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// One difference. `path` is `BP/...` / `RP/...`; "left" is the oracle (legacy output),
/// "right" the Beacon build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Diff {
    /// In the oracle only.
    Missing { path: String },
    /// In the Beacon build only.
    Extra { path: String },
    /// JSON values differ at `pointer` (RFC 6901); `None` = key absent on that side.
    Json {
        path: String,
        pointer: String,
        left: Option<Value>,
        right: Option<Value>,
    },
    /// `.lang` key differs; `None` = key absent on that side.
    Lang {
        path: String,
        key: String,
        left: Option<String>,
        right: Option<String>,
    },
    /// Text differs, first differing 1-based line.
    Text { path: String, line: u32 },
    /// Binary bytes differ.
    Binary { path: String },
    /// NBT (`level.dat`) differs at dotted `key`.
    Nbt { path: String, key: String },
    /// Script bundles differ after normalization; `left`/`right` are the first differing
    /// normalized lines.
    Bundle {
        path: String,
        left: String,
        right: String,
    },
    /// A file or directory could not be read or listed; it was not compared. `side` is
    /// `oracle` or `built`.
    Unreadable { path: String, side: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EquivReport {
    pub diffs: Vec<Diff>,
    /// Files compared.
    pub compared: usize,
    /// Files skipped by an `ignore` glob.
    pub ignored: usize,
    /// `ignore` globs that did not parse (they ignore nothing).
    #[serde(default)]
    pub invalid_ignores: Vec<String>,
}

/// Always ignored: derived at build time (`contents.json`, the emit ledger).
const DEFAULT_IGNORES: [&str; 2] = ["**/contents.json", "**/.beacon-emit.json"];
/// `level.dat` tags that change on every world load.
const NBT_VOLATILE: [&str; 4] = ["LastPlayed", "Time", "currentTick", "worldStartCount"];

/// Compares two pack trees (`BP/`, `RP/` under each root; any other file under the roots is
/// compared too). `ignore` holds globs over `BP/...` / `RP/...` paths (e.g. `BP/*/manifest.json`;
/// `*` does not cross `/`), added to the default ignores. An invalid glob ignores nothing and is
/// listed in `invalid_ignores`. Output is sorted by path; unreadable directories and files are
/// reported as [`Diff::Unreadable`], never compared as empty.
pub fn compare(oracle: &Path, built: &Path, ignore: &[String]) -> EquivReport {
    let mut rep = EquivReport::default();
    let mut set = globset::GlobSetBuilder::new();
    for g in ignore.iter().map(String::as_str).chain(DEFAULT_IGNORES) {
        match globset::GlobBuilder::new(g).literal_separator(true).build() {
            Ok(g) => {
                set.add(g);
            }
            Err(_) => rep.invalid_ignores.push(g.to_string()),
        }
    }
    let set = set.build().unwrap_or_else(|_| globset::GlobSet::empty());
    let ((a, ea), (b, eb)) = (walk(oracle), walk(built));
    for (side, errs) in [("oracle", ea), ("built", eb)] {
        for path in errs {
            rep.diffs.push(Diff::Unreadable {
                path,
                side: side.into(),
            });
        }
    }
    let paths: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    for p in paths {
        if set.is_match(p.as_str()) {
            rep.ignored += 1;
            continue;
        }
        match (a.get(p), b.get(p)) {
            (Some(_), None) => rep.diffs.push(Diff::Missing { path: p.clone() }),
            (None, Some(_)) => rep.diffs.push(Diff::Extra { path: p.clone() }),
            (Some(x), Some(y)) => {
                rep.compared += 1;
                match (fs::read(x), fs::read(y)) {
                    (Ok(l), Ok(r)) => file_diff(p, &l, &r, &mut rep.diffs),
                    (l, r) => {
                        for (side, ok) in [("oracle", l.is_ok()), ("built", r.is_ok())] {
                            if !ok {
                                rep.diffs.push(Diff::Unreadable {
                                    path: p.clone(),
                                    side: side.into(),
                                });
                            }
                        }
                    }
                }
            }
            (None, None) => {}
        }
    }
    rep
}

/// Root-relative `/` path -> file for every regular file below `root`, plus the sorted paths
/// that could not be listed or typed (`.` = the root itself).
fn walk(root: &Path) -> (BTreeMap<String, PathBuf>, Vec<String>) {
    let (mut out, mut errs) = (BTreeMap::new(), Vec::new());
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        let here = || match rel.trim_end_matches('/') {
            "" => ".".to_string(),
            r => r.to_string(),
        };
        let Ok(rd) = fs::read_dir(&dir) else {
            errs.push(here());
            continue;
        };
        for e in rd {
            let Ok(e) = e else {
                errs.push(here());
                continue;
            };
            let name = e.file_name().to_string_lossy().into_owned();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push((e.path(), format!("{rel}{name}/"))),
                Ok(t) if t.is_file() => {
                    out.insert(format!("{rel}{name}"), e.path());
                }
                // symlinks, junctions, devices: not followed, so not compared; say so
                Ok(_) => errs.push(format!("{rel}{name}")),
                Err(_) => errs.push(format!("{rel}{name}")),
            }
        }
    }
    errs.sort();
    (out, errs)
}

fn file_diff(path: &str, l: &[u8], r: &[u8], out: &mut Vec<Diff>) {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "level.dat"
        && let (Ok((_, a)), Ok((_, b))) = (nbt::parse_level_dat(l), nbt::parse_level_dat(r))
    {
        return nbt_diff(path, "", &a, &b, out);
    }
    let texts = (std::str::from_utf8(l), std::str::from_utf8(r));
    let (Ok(ls), Ok(rs)) = texts else {
        return binary(path, l, r, out);
    };
    if l.contains(&0) || r.contains(&0) {
        return binary(path, l, r, out);
    }
    match ext {
        "json" | "material" => {
            let parse = |s: &str| {
                let s = s.strip_prefix('\u{feff}').unwrap_or(s);
                serde_json::from_slice::<Value>(&strip_comments(s.as_bytes())).ok()
            };
            if let (Some(a), Some(b)) = (parse(ls), parse(rs)) {
                let as_set = path.ends_with("textures/textures_list.json")
                    || path.ends_with("texts/languages.json");
                match (as_set, &a, &b) {
                    (true, Value::Array(x), Value::Array(y)) => set_diff(path, x, y, out),
                    _ => json_diff(path, &mut String::new(), &a, &b, out),
                }
                return;
            }
        }
        "lang" => {
            let (a, b) = (lang_map(ls), lang_map(rs));
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys.into_iter().filter(|k| a.get(*k) != b.get(*k)) {
                out.push(Diff::Lang {
                    path: path.into(),
                    key: k.clone(),
                    left: a.get(k).cloned(),
                    right: b.get(k).cloned(),
                });
            }
            return;
        }
        "js" if path.split('/').any(|c| c == "scripts")
            && !(bundle_headers(ls).is_empty() && bundle_headers(rs).is_empty()) =>
        {
            // An esbuild bundle (at least one side has module headers): the module order is the
            // contract, a byte diff is informational. Headerless scripts fall to the text compare.
            let (a, b) = (bundle_headers(ls), bundle_headers(rs));
            if a != b {
                let i = a.iter().zip(&b).position(|(x, y)| x != y);
                let i = i.unwrap_or(a.len().min(b.len()));
                let at = |v: &[String]| v.get(i).cloned().unwrap_or_default();
                out.push(Diff::Bundle {
                    path: path.into(),
                    left: at(&a),
                    right: at(&b),
                });
            }
            return;
        }
        _ => {}
    }
    let norm = |s: &str| -> Vec<String> {
        s.replace("\r\n", "\n")
            .split('\n')
            .map(|l| l.trim_end_matches([' ', '\t']).to_string())
            .collect()
    };
    let (a, b) = (norm(ls), norm(rs));
    if a != b {
        let i = a.iter().zip(&b).position(|(x, y)| x != y);
        let line = i.unwrap_or(a.len().min(b.len())) + 1;
        out.push(Diff::Text {
            path: path.into(),
            line: line as u32,
        });
    }
}

fn binary(path: &str, l: &[u8], r: &[u8], out: &mut Vec<Diff>) {
    if blake3::hash(l) != blake3::hash(r) {
        out.push(Diff::Binary { path: path.into() });
    }
}

/// Objects by key (order-insensitive), arrays by index.
fn json_diff(path: &str, ptr: &mut String, l: &Value, r: &Value, out: &mut Vec<Diff>) {
    let n = ptr.len();
    let mut leaf = |ptr: &str, x: Option<&Value>, y: Option<&Value>| {
        out.push(Diff::Json {
            path: path.into(),
            pointer: ptr.into(),
            left: x.cloned(),
            right: y.cloned(),
        })
    };
    let mut kids: Vec<(String, Option<&Value>, Option<&Value>)> = Vec::new();
    match (l, r) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let tok = k.replace('~', "~0").replace('/', "~1");
                kids.push((tok, a.get(k), b.get(k)));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                kids.push((i.to_string(), a.get(i), b.get(i)));
            }
        }
        _ => {
            if l != r {
                leaf(ptr, Some(l), Some(r));
            }
            return;
        }
    }
    for (tok, x, y) in kids {
        ptr.push('/');
        ptr.push_str(&tok);
        match (x, y) {
            (Some(x), Some(y)) => json_diff(path, ptr, x, y, out),
            _ => out.push(Diff::Json {
                path: path.into(),
                pointer: ptr.clone(),
                left: x.cloned(),
                right: y.cloned(),
            }),
        }
        ptr.truncate(n);
    }
}

/// Arrays compared as sets; each pointer indexes the side the element is on.
fn set_diff(path: &str, a: &[Value], b: &[Value], out: &mut Vec<Diff>) {
    let mut one_sided = |from: &[Value], other: &[Value], left: bool| {
        for (i, v) in from.iter().enumerate().filter(|(_, v)| !other.contains(v)) {
            let v = Some(v.clone());
            out.push(Diff::Json {
                path: path.into(),
                pointer: format!("/{i}"),
                left: if left { v.clone() } else { None },
                right: if left { None } else { v },
            });
        }
    };
    one_sided(a, b, true);
    one_sided(b, a, false);
}

/// `key=value` lines; blank lines and `##` comments ignored, trailing `##...` comments and
/// whitespace stripped, last duplicate wins.
fn lang_map(s: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in s.lines() {
        let line = line.strip_prefix('\u{feff}').unwrap_or(line);
        if line.trim().is_empty() || line.trim_start().starts_with("##") {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.split("##").next().unwrap_or("").trim_end();
            m.insert(k.trim().to_string(), v.to_string());
        }
    }
    m
}

/// esbuild's `// <path>` module headers in order, as project-relative `/` paths.
fn bundle_headers(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.strip_prefix("// "))
        .map(|p| {
            let mut p = p.trim().replace('\\', "/");
            while let Some(r) = p.strip_prefix("./").or_else(|| p.strip_prefix("../")) {
                p = r.to_string();
            }
            p
        })
        .collect()
}

fn nbt_map(v: &[(Vec<u8>, Tag)]) -> BTreeMap<String, &Tag> {
    v.iter()
        .map(|(n, t)| (String::from_utf8_lossy(n).into_owned(), t))
        .collect()
}

/// Compounds by key (order-insensitive), lists by index; volatile tags skipped.
fn nbt_diff(path: &str, key: &str, l: &Tag, r: &Tag, out: &mut Vec<Diff>) {
    let kids: Vec<(String, Option<&Tag>, Option<&Tag>)> = match (l, r) {
        (Tag::Compound(a), Tag::Compound(b)) => {
            let (a, b) = (nbt_map(a), nbt_map(b));
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter()
                .filter(|k| !NBT_VOLATILE.contains(&k.as_str()))
                .map(|k| {
                    let full = if key.is_empty() {
                        k.clone()
                    } else {
                        format!("{key}.{k}")
                    };
                    (full, a.get(k).copied(), b.get(k).copied())
                })
                .collect()
        }
        (Tag::List(_, a), Tag::List(_, b)) => (0..a.len().max(b.len()))
            .map(|i| (format!("{key}[{i}]"), a.get(i), b.get(i)))
            .collect(),
        _ => {
            if l != r {
                out.push(Diff::Nbt {
                    path: path.into(),
                    key: key.into(),
                });
            }
            return;
        }
    };
    for (k, x, y) in kids {
        match (x, y) {
            (Some(x), Some(y)) => nbt_diff(path, &k, x, y, out),
            _ => out.push(Diff::Nbt {
                path: path.into(),
                key: k,
            }),
        }
    }
}
