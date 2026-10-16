use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::merge::resolve;
use beacon_engine::model::{Body, ContentItem, Dest, Merge, Pack};
use beacon_engine::path::{Hash, RelPath, Vfs};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Write};

#[derive(Default)]
struct Mem(BTreeMap<RelPath, Vec<u8>>);

impl Vfs for Mem {
    fn exists(&self, p: &RelPath) -> bool {
        self.0.contains_key(p)
    }
    fn read(&self, p: &RelPath) -> std::io::Result<Vec<u8>> {
        self.0
            .get(p)
            .cloned()
            .ok_or_else(|| Error::from(ErrorKind::NotFound))
    }
    fn hash(&self, p: &RelPath) -> std::io::Result<Hash> {
        Ok(*blake3::hash(&self.read(p)?).as_bytes())
    }
    fn list(&self, _: Option<&RelPath>) -> Vec<String> {
        vec![]
    }
    fn stream(&self, p: &RelPath, out: &mut dyn Write) -> std::io::Result<u64> {
        let b = self.read(p)?;
        out.write_all(&b)?;
        Ok(b.len() as u64)
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn item(dest: &str, origin: &str, merge: Merge) -> ContentItem {
    ContentItem {
        dest: Dest {
            output: "main".into(),
            pack: Pack::Behavior,
            path: rp(dest),
        },
        body: Body::Passthrough(rp(origin)),
        merge,
        origin: rp(origin),
        module: rp("src/m"),
    }
}

fn vfs(files: &[(&str, &str)]) -> Mem {
    Mem(files
        .iter()
        .map(|(p, c)| (rp(p), c.as_bytes().to_vec()))
        .collect())
}

fn codes(d: &[Diagnostic]) -> Vec<Code> {
    d.iter().map(|d| d.code).collect()
}

#[test]
fn skip_yields_to_non_skip() {
    let (out, d) = resolve(
        vec![
            item("a.json", "src/a/a.json", Merge::Skip),
            item("a.json", "src/b/a.json", Merge::Error),
        ],
        &Mem::default(),
    );
    assert!(d.is_empty());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].origin, rp("src/b/a.json"));
    assert!(matches!(out[0].body, Body::Passthrough(_)));
    // all Skip: first by origin wins
    let (out, d) = resolve(
        vec![
            item("a.json", "src/z/a.json", Merge::Skip),
            item("a.json", "src/c/a.json", Merge::Skip),
        ],
        &Mem::default(),
    );
    assert!(d.is_empty());
    assert_eq!(out[0].origin, rp("src/c/a.json"));
}

#[test]
fn collisions() {
    let m = Mem::default();
    for (a, b) in [
        (Merge::Error, Merge::Error),
        (Merge::JsonDeep, Merge::LangLines),
        (Merge::JsonDeep, Merge::Error),
    ] {
        let (_, d) = resolve(
            vec![
                item("a.json", "src/a/a.json", a),
                item("a.json", "src/b/a.json", b),
            ],
            &m,
        );
        assert_eq!(codes(&d), [Code::RouteCollision], "{a:?} {b:?}");
    }
}

#[test]
fn case_fold_collision_across_dests() {
    let (out, d) = resolve(
        vec![
            item("items/Sword.json", "src/a/s.json", Merge::Error),
            item("items/sword.json", "src/b/s.json", Merge::Error),
        ],
        &Mem::default(),
    );
    assert_eq!(codes(&d), [Code::CaseFoldCollision]);
    // only the first (by origin) is kept: both would land on one file on Windows/macOS
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].dest.path.as_str(), "items/Sword.json");
    // different pack: fine
    let mut other = item("items/sword.json", "src/b/s.json", Merge::Error);
    other.dest.pack = Pack::Resource;
    let (_, d) = resolve(
        vec![
            item("items/Sword.json", "src/a/s.json", Merge::Error),
            other,
        ],
        &Mem::default(),
    );
    assert!(d.is_empty());
}

#[test]
fn json_deep_merge() {
    let v = vfs(&[
        (
            "src/a/a.json",
            "{ // comment\n \"url\": \"http://x//y\", \"same\": 1, \"obj\": {\"a\": 1}, \"arr\": [1] /* c */ }",
        ),
        (
            "src/b/a.json",
            r#"{"same": 1, "obj": {"b": 2}, "arr": [2, 3]}"#,
        ),
    ]);
    let (out, d) = resolve(
        vec![
            item("a.json", "src/b/a.json", Merge::JsonDeep),
            item("a.json", "src/a/a.json", Merge::JsonDeep),
        ],
        &v,
    );
    assert!(d.is_empty(), "{d:?}");
    assert_eq!(out.len(), 1);
    assert_eq!(
        out[0].body,
        Body::Json(
            json!({"url": "http://x//y", "same": 1, "obj": {"a": 1, "b": 2}, "arr": [1, 2, 3]})
        )
    );
}

#[test]
fn json_deep_conflicting_scalar() {
    let v = vfs(&[
        ("src/a/a.json", r#"{"o": {"k": 1}}"#),
        ("src/b/a.json", r#"{"o": {"k": 2}}"#),
    ]);
    let (_, d) = resolve(
        vec![
            item("a.json", "src/a/a.json", Merge::JsonDeep),
            item("a.json", "src/b/a.json", Merge::JsonDeep),
        ],
        &v,
    );
    assert_eq!(codes(&d), [Code::MergeConflict]);
    assert!(d[0].message.contains("/o/k"));
}

#[test]
fn lang_lines() {
    let v = vfs(&[
        ("src/a/en.lang", "## header\na.b=One\n\n# note\n"),
        ("src/b/en.lang", "c=Two\n"),
        ("src/c/en.lang", "a.b = Dup\n"),
    ]);
    let l = |o| item("texts/en.lang", o, Merge::LangLines);
    let (out, d) = resolve(vec![l("src/b/en.lang"), l("src/a/en.lang")], &v);
    assert!(d.is_empty());
    assert_eq!(
        out[0].body,
        Body::Text("## header\na.b=One\n\n# note\nc=Two\n".into())
    );
    let (_, d) = resolve(vec![l("src/a/en.lang"), l("src/c/en.lang")], &v);
    assert_eq!(codes(&d), [Code::LangDuplicateKey]);
}

#[test]
fn output_sorted_by_dest() {
    let (out, _) = resolve(
        vec![
            item("z.json", "src/a/z.json", Merge::Error),
            item("a.json", "src/a/a.json", Merge::Error),
        ],
        &Mem::default(),
    );
    assert_eq!(out[0].dest.path, rp("a.json"));
}

#[test]
fn bom_and_invalid_inputs() {
    let v = vfs(&[
        ("src/a/a.json", "\u{feff}{\"k\": 1}"),
        ("src/b/a.json", "{\"j\": 2}"),
        ("src/c/a.json", "{not json"),
        ("src/0/a.json", "{not json"),
    ]);
    let j = |o| item("a.json", o, Merge::JsonDeep);
    let (out, d) = resolve(vec![j("src/a/a.json"), j("src/b/a.json")], &v);
    assert!(d.is_empty(), "{d:?}");
    assert_eq!(out[0].body, Body::Json(json!({"k": 1, "j": 2})));
    // bad first item: one diagnostic, first item kept untouched
    let (out, d) = resolve(vec![j("src/0/a.json"), j("src/a/a.json")], &v);
    assert_eq!(codes(&d), [Code::MergeInputInvalid]);
    assert_eq!(out[0].body, Body::Passthrough(rp("src/0/a.json")));
    // bad later item: reported, merge of the rest continues
    let (out, d) = resolve(vec![j("src/a/a.json"), j("src/c/a.json")], &v);
    assert_eq!(codes(&d), [Code::MergeInputInvalid]);
    assert_eq!(out[0].body, Body::Json(json!({"k": 1})));
    // unreadable
    let (_, d) = resolve(vec![j("src/a/a.json"), j("src/zz/a.json")], &v);
    assert_eq!(codes(&d), [Code::MergeInputInvalid]);
}
