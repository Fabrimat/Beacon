//! Workstream A1: the one merge primitive (5.3). Groups items by `Dest` and applies `Merge`.

use crate::diag::{Code, Diagnostic};
use crate::model::{Body, ContentItem, Dest, Merge};
use crate::path::Vfs;
use serde_json::Value;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Returns one item per `Dest` (sorted by dest) plus diagnostics: `RouteCollision`,
/// `CaseFoldCollision` (dest paths equal after case folding within one pack; only the dest
/// with the first origin is kept, the others are dropped: they would overwrite each other on
/// Windows/macOS),
/// `LangDuplicateKey`, `MergeConflict`. Passthrough bodies taking part in a JSON or lang merge
/// are read through `vfs` (JSON with `//` and `/* */` comments must parse).
pub fn resolve(mut items: Vec<ContentItem>, vfs: &dyn Vfs) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    // One stable sort + runs instead of a map of vectors: the same groups in dest order,
    // each in input order, without a `Dest` clone and a `Vec` per item.
    items.sort_by(|a, b| a.dest.cmp(&b.dest));
    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for (n, i) in items.iter().enumerate() {
        match ranges.last_mut() {
            Some(r) if items[r.start].dest == i.dest => r.end = n + 1,
            _ => ranges.push(n..n + 1),
        }
    }

    // groups whose dests differ only by case, in (output, pack, folded path) order
    type Fold<'a> = (&'a str, crate::model::Pack, String);
    let mut first_of: HashMap<Fold, usize> = HashMap::with_capacity(ranges.len());
    let mut folded: BTreeMap<Fold, Vec<usize>> = BTreeMap::new();
    for (g, r) in ranges.iter().enumerate() {
        let d = &items[r.start].dest;
        match first_of.entry((&d.output, d.pack, d.path.fold_key())) {
            Entry::Occupied(e) => folded
                .entry(e.key().clone())
                .or_insert_with(|| vec![*e.get()])
                .push(g),
            Entry::Vacant(e) => {
                e.insert(g);
            }
        }
    }
    let first = |g: usize| items[ranges[g].clone()].iter().map(|i| &i.origin).min();
    let mut dropped = vec![false; ranges.len()];
    for gs in folded.values() {
        let keep = gs.iter().min_by_key(|g| first(**g)).copied();
        for g in gs.iter().filter(|g| Some(**g) != keep) {
            dropped[*g] = true;
        }
        let d0 = &items[ranges[gs[0]].start].dest;
        let mut d = Diagnostic::new(
            Code::CaseFoldCollision,
            format!(
                "{} destinations differ only by case: {}",
                d0.pack_name(),
                join(
                    gs.iter()
                        .map(|g| items[ranges[*g].start].dest.path.as_str())
                )
            ),
        );
        for g in gs {
            let o = &items[ranges[*g].start].origin;
            d = if d.span.is_none() {
                d.at(o.clone(), None, None)
            } else {
                d.related(o.clone())
            };
        }
        diags.push(d);
    }

    let mut out = Vec::with_capacity(ranges.len());
    let mut it = items.into_iter();
    for (r, dropped) in ranges.iter().zip(dropped) {
        if r.len() == 1 {
            let i = it.next().expect("range item");
            if !dropped {
                out.push(i); // the common case: no clone
            }
            continue;
        }
        let mut g: Vec<ContentItem> = it.by_ref().take(r.len()).collect();
        if !dropped {
            g.sort_by(|a, b| a.origin.cmp(&b.origin));
            out.push(group(g, vfs, &mut diags));
        }
    }
    (out, diags)
}

impl Dest {
    fn pack_name(&self) -> &'static str {
        match self.pack {
            crate::model::Pack::Behavior => "BP",
            crate::model::Pack::Resource => "RP",
        }
    }
}

fn join<'a>(it: impl Iterator<Item = &'a str>) -> String {
    it.collect::<Vec<_>>().join(", ")
}

/// Resolves one `Dest` group (already sorted by origin).
fn group(g: Vec<ContentItem>, vfs: &dyn Vfs, diags: &mut Vec<Diagnostic>) -> ContentItem {
    if g.len() == 1 {
        return g.into_iter().next().expect("len 1"); // the common case: no clone
    }
    let mut live: Vec<ContentItem> = g
        .iter()
        .filter(|i| i.merge != Merge::Skip)
        .cloned()
        .collect();
    // The same source file reaching one dest twice (convention route + a generator's
    // `asset()` reference) is one item, not a collision; different sources still collide.
    let mut seen = BTreeSet::new();
    live.retain(|i| match &i.body {
        Body::Passthrough(src) => seen.insert(src.clone()),
        _ => true,
    });
    if live.is_empty() {
        return g.into_iter().next().expect("non-empty group");
    }
    if live.len() == 1 {
        return live.remove(0);
    }
    let policies: BTreeSet<_> = live.iter().map(|i| i.merge as u8).collect();
    if live[0].merge == Merge::Error || policies.len() > 1 {
        let mut d = Diagnostic::new(
            Code::RouteCollision,
            format!(
                "{} items route to {} with no common merge policy",
                live.len(),
                live[0].dest.path
            ),
        )
        .at(live[0].origin.clone(), None, None);
        for i in &live[1..] {
            d = d.related(i.origin.clone());
        }
        diags.push(d);
        return live.remove(0);
    }
    let merge = live[0].merge;
    let mut first = live.remove(0);
    first.merge = merge;
    first.body = if merge == Merge::JsonDeep {
        json_deep(&first, &live, vfs, diags)
    } else {
        lang(&first, &live, vfs, diags)
    };
    first
}

fn bytes(i: &ContentItem, vfs: &dyn Vfs) -> Result<Vec<u8>, String> {
    match &i.body {
        Body::Passthrough(p) => vfs.read(p).map_err(|e| format!("cannot read {p}: {e}")),
        Body::Json(v) => Ok(v.to_string().into_bytes()),
        Body::Text(t) => Ok(t.clone().into_bytes()),
        Body::Bytes(b) => Ok(b.clone()),
    }
}

fn json_deep(
    first: &ContentItem,
    rest: &[ContentItem],
    vfs: &dyn Vfs,
    diags: &mut Vec<Diagnostic>,
) -> Body {
    let parse = |i: &ContentItem, diags: &mut Vec<Diagnostic>| -> Option<Value> {
        let r = match &i.body {
            Body::Json(v) => return Some(v.clone()),
            _ => bytes(i, vfs).and_then(|b| {
                let b = b.strip_prefix(&[0xEF_u8, 0xBB, 0xBF][..]).unwrap_or(&b);
                serde_json::from_slice(&strip_comments(b)).map_err(|e| format!("{e}"))
            }),
        };
        r.map_err(|e| {
            diags.push(
                Diagnostic::new(
                    Code::MergeInputInvalid,
                    format!("cannot merge, not valid JSON: {e}"),
                )
                .at(i.origin.clone(), None, None),
            )
        })
        .ok()
    };
    // a bad first item: keep it as is, report once, no spurious conflicts
    let Some(mut acc) = parse(first, diags) else {
        return first.body.clone();
    };
    for i in rest {
        let Some(v) = parse(i, diags) else { continue };
        let mut conflicts = Vec::new();
        deep(&mut acc, v, "", &mut conflicts);
        for c in conflicts {
            diags.push(
                Diagnostic::new(
                    Code::MergeConflict,
                    format!(
                        "different values for {}",
                        if c.is_empty() { "/" } else { &c }
                    ),
                )
                .at(i.origin.clone(), None, None)
                .related(first.origin.clone()),
            );
        }
    }
    Body::Json(acc)
}

fn deep(a: &mut Value, b: Value, path: &str, conflicts: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in y {
                match x.get_mut(&k) {
                    Some(e) => deep(e, v, &format!("{path}/{k}"), conflicts),
                    None => {
                        x.insert(k, v);
                    }
                }
            }
        }
        (Value::Array(x), Value::Array(y)) => x.extend(y),
        (a, b) => {
            if *a != b {
                conflicts.push(path.to_owned());
            }
        }
    }
}

fn lang(
    first: &ContentItem,
    rest: &[ContentItem],
    vfs: &dyn Vfs,
    diags: &mut Vec<Diagnostic>,
) -> Body {
    let mut lines: Vec<String> = Vec::new();
    let mut seen: BTreeMap<String, &ContentItem> = BTreeMap::new();
    for i in std::iter::once(first).chain(rest) {
        let text = match bytes(i, vfs)
            .and_then(|b| String::from_utf8(b).map_err(|e| format!("not valid UTF-8: {e}")))
        {
            Ok(t) => t,
            Err(e) => {
                diags.push(Diagnostic::new(Code::MergeInputInvalid, e).at(
                    i.origin.clone(),
                    None,
                    None,
                ));
                if std::ptr::eq(i, first) {
                    return first.body.clone();
                }
                continue;
            }
        };
        for (n, line) in text
            .strip_prefix('\u{feff}')
            .unwrap_or(&text)
            .lines()
            .enumerate()
        {
            let t = line.trim();
            if let (false, Some((k, _))) = (t.starts_with('#'), t.split_once('=')) {
                let k = k.trim();
                if let Some(prev) = seen.get(k) {
                    diags.push(
                        Diagnostic::new(
                            Code::LangDuplicateKey,
                            format!("lang key {k:?} defined twice"),
                        )
                        .at(i.origin.clone(), Some(n as u32 + 1), None)
                        .related(prev.origin.clone()),
                    );
                } else {
                    seen.insert(k.to_owned(), i);
                }
            }
            lines.push(line.to_owned());
        }
    }
    Body::Text(lines.iter().map(|l| format!("{l}\n")).collect())
}

/// Removes `//` and `/* */` comments outside strings (newlines kept for line numbers).
pub(crate) fn strip_comments(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                i += 1;
                out.push(b[i]);
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            out.push(c);
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                if b[i] == b'\n' {
                    out.push(b'\n');
                }
                i += 1;
            }
            i += 1;
            out.push(b' ');
        } else {
            out.push(c);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_comments_not_strings() {
        let s = super::strip_comments(br#"{"a": "/* x */", "b": "q\" // not", "c": 1}"#);
        let v: serde_json::Value = serde_json::from_slice(&s).unwrap();
        assert_eq!(v["a"], "/* x */");
        assert_eq!(v["b"], "q\" // not");
        assert_eq!(v["c"], 1);
        let s = super::strip_comments(b"{\"u\": \"http://x\" /* c */, // t\n\"a\": 1}");
        let v: serde_json::Value = serde_json::from_slice(&s).unwrap();
        assert_eq!(v["u"], "http://x");
        assert_eq!(v["a"], 1);
    }
}
