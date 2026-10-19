//! `module.json` forms (phase 5, 2B): read a module's file with a base hash, write a new value
//! back keeping the key order and indentation of the file. The only writer is the daemon's
//! `write_settings` tool; the desktop app goes through it.
//!
//! Order: `serde_json::Value` objects are sorted (no `preserve_order`; enabling it would
//! reorder every JSON the workspace emits), and the value crossing IPC is a `Value` too. So the
//! write merges the new value onto the file's own parse, which keeps key order ([`Ord`]):
//! keys of the old file stay in place, removed keys go, new keys are appended.

use crate::config::{MODULE_FILE, ModuleJson};
use crate::diag::{Code, Diagnostic};
use crate::path::RelPath;
use crate::route;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::Path;

/// The JSON Schema of `module.json` (the form is rendered from it).
pub const MODULE_SCHEMA: &str = include_str!("../schemas/module.schema.json");

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    pub value: Value,
    /// blake3 hex of the bytes on disk (of nothing when the file does not exist).
    pub base: String,
}

pub fn schema() -> Value {
    serde_json::from_str(MODULE_SCHEMA).expect("module.schema.json is valid JSON")
}

pub fn hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// A JSON value whose objects keep file order.
#[derive(Debug)]
enum Ord {
    Obj(Vec<(String, Ord)>),
    Arr(Vec<Ord>),
    Leaf(Value),
}

impl<'de> Deserialize<'de> for Ord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ord;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Ord, A::Error> {
                let mut v = Vec::new();
                while let Some(kv) = m.next_entry()? {
                    v.push(kv);
                }
                Ok(Ord::Obj(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut s: A) -> Result<Ord, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = s.next_element()? {
                    v.push(x);
                }
                Ok(Ord::Arr(v))
            }
            fn visit_bool<E>(self, b: bool) -> Result<Ord, E> {
                Ok(Ord::Leaf(Value::Bool(b)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Ord, E> {
                Ok(Ord::Leaf(n.into()))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Ord, E> {
                Ok(Ord::Leaf(n.into()))
            }
            fn visit_f64<E>(self, n: f64) -> Result<Ord, E> {
                Ok(Ord::Leaf(
                    serde_json::Number::from_f64(n).map_or(Value::Null, Value::Number),
                ))
            }
            fn visit_str<E>(self, s: &str) -> Result<Ord, E> {
                Ok(Ord::Leaf(Value::String(s.into())))
            }
            fn visit_unit<E>(self) -> Result<Ord, E> {
                Ok(Ord::Leaf(Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}

impl Ord {
    fn to_value(&self) -> Value {
        match self {
            Ord::Obj(v) => {
                Value::Object(v.iter().map(|(k, x)| (k.clone(), x.to_value())).collect())
            }
            Ord::Arr(v) => Value::Array(v.iter().map(Ord::to_value).collect()),
            Ord::Leaf(v) => v.clone(),
        }
    }

    /// `new`, laid out like `self`: old key order kept, removed keys dropped, new keys last;
    /// arrays merge element-wise.
    fn merged(&self, new: &Value) -> Ord {
        match (self, new) {
            (Ord::Obj(old), Value::Object(n)) => {
                let mut out: Vec<(String, Ord)> = Vec::new();
                for (k, o) in old {
                    if let Some(nv) = n.get(k)
                        && !out.iter().any(|(x, _)| x == k)
                    {
                        out.push((k.clone(), o.merged(nv)));
                    }
                }
                for (k, nv) in n {
                    if !out.iter().any(|(x, _)| x == k) {
                        out.push((k.clone(), Ord::of(nv)));
                    }
                }
                Ord::Obj(out)
            }
            (Ord::Arr(old), Value::Array(n)) => Ord::Arr(
                n.iter()
                    .enumerate()
                    .map(|(i, nv)| old.get(i).map_or_else(|| Ord::of(nv), |o| o.merged(nv)))
                    .collect(),
            ),
            _ => Ord::of(new),
        }
    }

    fn of(v: &Value) -> Ord {
        match v {
            Value::Object(m) => Ord::Obj(m.iter().map(|(k, x)| (k.clone(), Ord::of(x))).collect()),
            Value::Array(a) => Ord::Arr(a.iter().map(Ord::of).collect()),
            v => Ord::Leaf(v.clone()),
        }
    }

    /// Pretty-printed like `serde_json::to_string_pretty`, with `indent`.
    fn write(&self, out: &mut String, indent: &str, depth: usize) {
        let pad = |out: &mut String, d: usize| (0..d).for_each(|_| out.push_str(indent));
        match self {
            Ord::Obj(v) if !v.is_empty() => {
                out.push_str("{\n");
                for (i, (k, x)) in v.iter().enumerate() {
                    pad(out, depth + 1);
                    let _ = write!(out, "{}: ", Value::String(k.clone()));
                    x.write(out, indent, depth + 1);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push('}');
            }
            Ord::Arr(v) if !v.is_empty() => {
                out.push_str("[\n");
                for (i, x) in v.iter().enumerate() {
                    pad(out, depth + 1);
                    x.write(out, indent, depth + 1);
                    out.push_str(if i + 1 < v.len() { ",\n" } else { "\n" });
                }
                pad(out, depth);
                out.push(']');
            }
            Ord::Obj(_) => out.push_str("{}"),
            Ord::Arr(_) => out.push_str("[]"),
            Ord::Leaf(v) => {
                let _ = write!(out, "{v}");
            }
        }
    }
}

/// The first indented line's leading whitespace (a tab or spaces), else two spaces.
fn indent_of(text: &str) -> String {
    text.lines()
        .find(|l| l.starts_with([' ', '\t']) && !l.trim().is_empty())
        .map(|l| l[..l.len() - l.trim_start().len()].to_owned())
        .unwrap_or_else(|| "  ".into())
}

fn bad(rel: &RelPath, msg: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(Code::ModuleJsonInvalid, format!("{rel}: {msg}")).at(rel.clone(), None, None)
}

/// Every module's `module.json` path (the file may not exist yet): each folder directly under
/// a source dir of `config.json`. The only paths [`read`] and [`write`] accept.
pub fn module_files(root: &Path) -> Result<Vec<RelPath>, Vec<Diagnostic>> {
    let cfg = route::load(root)?;
    let mut out = Vec::new();
    for src in &cfg.beacon.sources {
        let s = src.trim_start_matches("./").trim_end_matches('/');
        let s = if s == "." { "" } else { s };
        let Ok(rd) = std::fs::read_dir(root.join(s)) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = if s.is_empty() {
                name
            } else {
                format!("{s}/{name}")
            };
            if e.path().is_dir()
                && let Ok(m) = RelPath::parse(&p).and_then(|m| m.join(MODULE_FILE))
            {
                out.push(m);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn check_path(root: &Path, rel: &RelPath) -> Result<(), Diagnostic> {
    let known = module_files(root).map_err(|d| d.into_iter().next().expect("one"))?;
    if known.contains(rel) {
        Ok(())
    } else {
        Err(bad(rel, "not the module.json of a module in this project"))
    }
}

fn read_bytes(root: &Path, rel: &RelPath) -> Result<Vec<u8>, Diagnostic> {
    match std::fs::read(root.join(rel.as_str())) {
        Ok(b) => Ok(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(bad(rel, format!("cannot read: {e}"))),
    }
}

fn parse(rel: &RelPath, bytes: &[u8]) -> Result<Ord, Diagnostic> {
    let body = bytes.strip_prefix(&BOM[..]).unwrap_or(bytes);
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(Ord::Obj(vec![]));
    }
    serde_json::from_slice(body).map_err(|e| bad(rel, e))
}

/// The module's `module.json` as a value (`{}` when absent) with its base hash.
pub fn read(root: &Path, rel: &RelPath) -> Result<Doc, Diagnostic> {
    check_path(root, rel)?;
    let bytes = read_bytes(root, rel)?;
    Ok(Doc {
        value: parse(rel, &bytes)?.to_value(),
        base: hash(&bytes),
    })
}

/// Validates `new` as a `ModuleJson` (`BCN0003`), refuses a changed file (`BCN0164`: the disk
/// no longer hashes to `base`), then replaces the file atomically with `new` laid out like the
/// old text. Nothing is written on any error. The check and the rename are not one atomic
/// step; the daemon runs this on its single engine thread, which is the single writer.
pub fn write(root: &Path, rel: &RelPath, base: &str, new: &Value) -> Result<Doc, Diagnostic> {
    let (bytes, doc) = plan(root, rel, base, new)?;
    atomic_write(root, &root.join(rel.as_str()), &bytes)
        .map_err(|e| bad(rel, format!("cannot write: {e}")))?;
    Ok(doc)
}

/// [`write`] without writing: the file's new bytes and the resulting document (`dryRun`).
pub fn plan(
    root: &Path,
    rel: &RelPath,
    base: &str,
    new: &Value,
) -> Result<(Vec<u8>, Doc), Diagnostic> {
    check_path(root, rel)?;
    if !new.is_object() {
        return Err(bad(rel, "expected a JSON object"));
    }
    serde_json::from_value::<ModuleJson>(new.clone()).map_err(|e| bad(rel, e))?;
    let old = read_bytes(root, rel)?;
    if hash(&old) != base {
        return Err(Diagnostic::new(
            Code::DocumentStale,
            format!("{rel} changed on disk since it was read; reload it and apply the edit again"),
        )
        .at(rel.clone(), None, None));
    }
    let text = String::from_utf8_lossy(old.strip_prefix(&BOM[..]).unwrap_or(&old));
    let merged = parse(rel, &old)?.merged(new);
    let mut out = String::new();
    merged.write(&mut out, &indent_of(&text), 0);
    if text.ends_with('\n') || old.is_empty() {
        out.push('\n');
    }
    if text.contains("\r\n") {
        // JSON strings escape their newlines: only the layout changes
        out = out.replace('\n', "\r\n");
    }
    let mut bytes = if old.starts_with(&BOM) {
        BOM.to_vec()
    } else {
        vec![]
    };
    bytes.extend_from_slice(out.as_bytes());
    let doc = Doc {
        value: merged.to_value(),
        base: hash(&bytes),
    };
    Ok((bytes, doc))
}

/// Writes `bytes` to a temp file in `<root>/.beacon/` (the watcher ignores it and a crash leaves
/// nothing in the sources), then renames it over `path`. Falls back to a sibling temp file when
/// `.beacon` cannot be used (including a rename across filesystems).
fn atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(std::io::Error::other)?;
    let name = format!("module.json.{:x}.tmp", u64::from_le_bytes(rnd));
    let via = |tmp: &Path| {
        std::fs::write(tmp, bytes)
            .and_then(|()| std::fs::rename(tmp, path))
            .inspect_err(|_| {
                let _ = std::fs::remove_file(tmp);
            })
    };
    let dir = root.join(".beacon");
    if std::fs::create_dir_all(&dir).is_ok() && via(&dir.join(&name)).is_ok() {
        return Ok(());
    }
    via(&path.with_file_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Hooks;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// Just enough JSON Schema for `module.schema.json`: type, enum, properties,
    /// additionalProperties (bool or schema), items, minimum, maximum.
    fn valid(s: &Value, v: &Value) -> bool {
        if let Some(e) = s.get("enum") {
            return e.as_array().unwrap().contains(v);
        }
        if let Some(t) = s.get("type") {
            let ts: Vec<&str> = match t {
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                t => vec![t.as_str().unwrap()],
            };
            let ok = ts.iter().any(|t| match *t {
                "string" => v.is_string(),
                "boolean" => v.is_boolean(),
                "integer" => v.is_i64() || v.is_u64(),
                "null" => v.is_null(),
                "object" => v.is_object(),
                "array" => v.is_array(),
                _ => false,
            });
            if !ok {
                return false;
            }
        }
        if let Some(n) = v.as_i64() {
            let lo = s.get("minimum").and_then(Value::as_i64).unwrap_or(i64::MIN);
            let hi = s.get("maximum").and_then(Value::as_i64).unwrap_or(i64::MAX);
            if n < lo || n > hi {
                return false;
            }
        }
        if let Some(o) = v.as_object() {
            let props = s.get("properties").and_then(Value::as_object);
            for (k, x) in o {
                match props.and_then(|p| p.get(k)) {
                    Some(ps) => {
                        if !valid(ps, x) {
                            return false;
                        }
                    }
                    None => match s.get("additionalProperties") {
                        Some(Value::Bool(false)) => return false,
                        Some(a @ Value::Object(_)) if !valid(a, x) => return false,
                        _ => {}
                    },
                }
            }
        }
        if let (Some(a), Some(items)) = (v.as_array(), s.get("items")) {
            return a.iter().all(|x| valid(items, x));
        }
        true
    }

    /// The field names serde expects, from its own unknown-field error.
    fn struct_fields<T: serde::de::DeserializeOwned>() -> BTreeSet<String> {
        let e = serde_json::from_value::<T>(json!({ "__nope": 1 }))
            .err()
            .expect("deny_unknown_fields")
            .to_string();
        let list = e.split("expected").nth(1).unwrap();
        list.split('`')
            .skip(1)
            .step_by(2)
            .map(String::from)
            .collect()
    }

    fn keys(v: &Value) -> BTreeSet<String> {
        v["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn schema_properties_equal_the_struct_fields() {
        let s = schema();
        assert_eq!(keys(&s), struct_fields::<ModuleJson>());
        assert_eq!(keys(&s["properties"]["hooks"]), struct_fields::<Hooks>());
        // pack ids: exactly the `Pack` enum's spellings
        assert_eq!(
            keys(&s["properties"]["packRoots"]),
            ["BP", "RP"].map(String::from).into()
        );
        assert!(
            serde_json::from_value::<ModuleJson>(json!({"packRoots": {"BP": "a", "RP": "b"}}))
                .is_ok()
        );
    }

    /// (sample, accepted): the schema and `ModuleJson` must agree on each.
    fn samples() -> Vec<(Value, bool)> {
        vec![
            (json!({}), true),
            (
                json!({
                    "$schema": "x", "output": "main", "onConflict": "merge",
                    "paths": {"ui/*.json": "RP/ui"}, "hooks": {"tick": {"a/b": -1}, "init": {}},
                    "itemTextures": {"gem": "t/gem"}, "terrainTextures": {"a": "b"},
                    "packRoots": {"BP": "bp"}, "conflicts": {"*.lang": "skip"}, "autoTextures": false
                }),
                true,
            ),
            (json!({"output": null, "onConflict": null}), true),
            (json!({"nope": 1}), false),
            (json!({"onConflict": "replace"}), false),
            (json!({"conflicts": {"*": "replace"}}), false),
            (json!({"autoTextures": "yes"}), false),
            (json!({"paths": {"a": 1}}), false),
            (json!({"hooks": {"tick": {"a": 1.5}}}), false),
            (json!({"hooks": {"tick": {"a": 3000000000_i64}}}), false),
            (json!({"hooks": {"late": {}}}), false),
            (json!({"packRoots": {"XX": "a"}}), false),
            (json!({"output": 3}), false),
        ]
    }

    #[test]
    fn schema_and_struct_agree_on_samples() {
        let s = schema();
        for (v, ok) in samples() {
            assert_eq!(valid(&s, &v), ok, "schema: {v}");
            assert_eq!(
                serde_json::from_value::<ModuleJson>(v.clone()).is_ok(),
                ok,
                "struct: {v}"
            );
        }
    }

    #[test]
    fn every_fixture_and_template_module_json_validates() {
        fn walk(d: &Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.file_name().is_some_and(|n| n == MODULE_FILE) {
                    out.push(p);
                }
            }
        }
        let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = vec![];
        for d in ["fixtures", "templates"] {
            walk(&top.join(d), &mut files);
        }
        let s = schema();
        for f in files {
            let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
            assert!(valid(&s, &v), "schema rejects {}", f.display());
            assert!(
                serde_json::from_value::<ModuleJson>(v).is_ok(),
                "{}",
                f.display()
            );
        }
    }

    const CONFIG: &str = r#"{"type":"minecraftBedrock","name":"p","authors":["a"],"targetVersion":"1.26.50","namespace":"p","beacon":{"sources":["./src"]}}"#;

    fn project(module_json: Option<&str>) -> (tempfile::TempDir, RelPath) {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.json"), CONFIG).unwrap();
        std::fs::create_dir_all(d.path().join("src/m")).unwrap();
        if let Some(t) = module_json {
            std::fs::write(d.path().join("src/m/module.json"), t).unwrap();
        }
        (d, RelPath::parse("src/m/module.json").unwrap())
    }

    fn disk(d: &tempfile::TempDir) -> String {
        std::fs::read_to_string(d.path().join("src/m/module.json")).unwrap()
    }

    #[test]
    fn crlf_is_kept_and_no_tmp_file_is_left_in_the_module_dir() {
        let text = "{\r\n  \"autoTextures\": true\r\n}\r\n";
        let (d, rel) = project(Some(text));
        let doc = read(d.path(), &rel).unwrap();
        let mut v = doc.value.clone();
        v["autoTextures"] = json!(false);
        write(d.path(), &rel, &doc.base, &v).unwrap();
        assert_eq!(disk(&d), text.replace("true", "false"));
        let names = |p: &str| -> Vec<String> {
            std::fs::read_dir(d.path().join(p))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names("src/m"), ["module.json"]);
        assert!(names(".beacon").is_empty(), "the tmp file was renamed away");
    }

    #[test]
    fn changing_one_value_keeps_key_order_and_indent() {
        for (ind, nl) in [("  ", "\n"), ("    ", ""), ("\t", "\n")] {
            let text = format!(
                "{{\n{ind}\"output\": \"main\",\n{ind}\"autoTextures\": true,\n{ind}\"paths\": {{\n{ind}{ind}\"z/*\": \"RP/z\",\n{ind}{ind}\"a/*\": \"RP/a\"\n{ind}}},\n{ind}\"onConflict\": \"skip\"\n}}{nl}"
            );
            let (d, rel) = project(Some(&text));
            let doc = read(d.path(), &rel).unwrap();
            let mut v = doc.value.clone();
            v["autoTextures"] = json!(false);
            let out = write(d.path(), &rel, &doc.base, &v).unwrap();
            assert_eq!(disk(&d), text.replace("true", "false"), "indent {ind:?}");
            assert_eq!(out.base, hash(disk(&d).as_bytes()));
            assert_eq!(out.value, v);
        }
    }

    #[test]
    fn added_keys_go_last_and_removed_keys_go() {
        let (d, rel) = project(Some(
            "{\n  \"output\": \"a\",\n  \"onConflict\": \"skip\"\n}\n",
        ));
        let doc = read(d.path(), &rel).unwrap();
        write(
            d.path(),
            &rel,
            &doc.base,
            &json!({"autoTextures": false, "onConflict": "skip"}),
        )
        .unwrap();
        assert_eq!(
            disk(&d),
            "{\n  \"onConflict\": \"skip\",\n  \"autoTextures\": false\n}\n"
        );
    }

    #[test]
    fn a_missing_file_is_created() {
        let (d, rel) = project(None);
        let doc = read(d.path(), &rel).unwrap();
        assert_eq!(doc.value, json!({}));
        write(d.path(), &rel, &doc.base, &json!({"output": "main"})).unwrap();
        assert_eq!(disk(&d), "{\n  \"output\": \"main\"\n}\n");
    }

    #[test]
    fn a_stale_base_is_refused_and_writes_nothing() {
        let (d, rel) = project(Some("{\"output\":\"a\"}"));
        let doc = read(d.path(), &rel).unwrap();
        std::fs::write(d.path().join("src/m/module.json"), "{\"output\":\"b\"}").unwrap();
        let e = write(d.path(), &rel, &doc.base, &json!({"output": "c"})).unwrap_err();
        assert_eq!(e.code, Code::DocumentStale);
        assert_eq!(disk(&d), "{\"output\":\"b\"}");
    }

    #[test]
    fn an_invalid_value_is_refused_and_writes_nothing() {
        let (d, rel) = project(Some("{}"));
        let doc = read(d.path(), &rel).unwrap();
        for bad in [
            json!({"onConflict": "replace"}),
            json!({"nope": 1}),
            json!([1]),
        ] {
            let e = write(d.path(), &rel, &doc.base, &bad).unwrap_err();
            assert_eq!(e.code, Code::ModuleJsonInvalid, "{bad}");
        }
        assert_eq!(disk(&d), "{}");
    }

    #[test]
    fn only_module_json_of_project_modules_is_accepted() {
        let (d, _) = project(Some("{}"));
        std::fs::write(d.path().join("secret.json"), "{}").unwrap();
        for p in [
            "secret.json",
            "config.json",
            "src/m/other.json",
            "src/ghost/module.json",
            "src/m/x/module.json",
        ] {
            let rel = RelPath::parse(p).unwrap();
            assert!(read(d.path(), &rel).is_err(), "{p}");
            assert!(
                write(d.path(), &rel, &hash(b"{}"), &json!({})).is_err(),
                "{p}"
            );
        }
        assert_eq!(module_files(d.path()).unwrap().len(), 1);
    }
}
