//! `evaluate_modular_mc` against the committed real-ModularMC output, plus targeted semantics.

use beacon_engine::diag::Code;
use beacon_engine::migrate::{Applied, LegacyEval, Policy, Template};
use beacon_engine::model::Body;
use beacon_engine::path::RelPath;
use beacon_engine::vfs::Index;
use beacon_jshost::Limits;
use beacon_jshost::legacy::{BasePaths, evaluate_modular_mc, migrate_limits};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/migrate")
}

fn rel(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn eval_at(root: &Path, data: &str, env: &[(&str, &str)], limits: Limits) -> LegacyEval {
    eval_base(root, data, BasePaths::default(), env, limits)
}

fn eval_base(
    root: &Path,
    data: &str,
    base: BasePaths,
    env: &[(&str, &str)],
    limits: Limits,
) -> LegacyEval {
    let (idx, d) = Index::build(root, &[rel(".regolith")], None);
    assert!(d.is_empty(), "{d:?}");
    let env: BTreeMap<_, _> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    evaluate_modular_mc(&idx, &rel(data), &base, &env, limits)
}

fn eval_fixture(env: &[(&str, &str)]) -> LegacyEval {
    let base = BasePaths {
        bp: Some(rel("packs/BP")),
        rp: Some(rel("packs/RP")),
    };
    eval_base(
        &fixtures().join("modular_mc"),
        "packs/data",
        base,
        env,
        migrate_limits(),
    )
}

fn project(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, b) in files {
        let f = d.path().join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, b).unwrap();
    }
    d
}

fn lf(b: &[u8]) -> String {
    String::from_utf8_lossy(b).replace("\r\n", "\n")
}

fn files_under(root: &Path) -> Vec<String> {
    let mut v = vec![];
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                let r = p.strip_prefix(root).unwrap();
                v.push(r.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    v.sort();
    v
}

/// The esbuild bundle is not a map output (only `SCRIPTS` is recorded).
const BUNDLE: &str = "BP/scripts/main.js";

#[test]
fn fixture_equals_expected_tree() {
    let ev = eval_fixture(&[]);
    assert!(ev.diagnostics.is_empty(), "{:?}", ev.diagnostics);
    let exp = fixtures().join("modular_mc.expected");
    let want: Vec<String> = files_under(&exp)
        .into_iter()
        .filter(|p| p != BUNDLE)
        // base files no entry touched are seeded, not outputs
        .filter(|p| {
            let base = fixtures().join("modular_mc/packs").join(p);
            !(base.is_file()
                && std::fs::read(&base).unwrap() == std::fs::read(exp.join(p)).unwrap())
        })
        .collect();
    assert_eq!(ev.outputs.keys().cloned().collect::<Vec<_>>(), want);
    for (path, o) in &ev.outputs {
        let expected = std::fs::read(exp.join(path)).unwrap();
        match &o.body {
            Body::Passthrough(src) => {
                let got = std::fs::read(fixtures().join("modular_mc").join(src.as_str())).unwrap();
                assert_eq!(got, expected, "{path} (bytes) / {}", lf(&got));
            }
            Body::Json(v) => {
                let e: serde_json::Value = serde_json::from_slice(&expected).unwrap();
                // to_string: key order is compared whenever serde_json keeps it
                assert_eq!(v.to_string(), e.to_string(), "{path}");
            }
            Body::Text(t) => assert_eq!(lf(t.as_bytes()), lf(&expected), "{path}"),
            Body::Bytes(_) => panic!("{path}: unexpected bytes"),
        }
    }
}

#[test]
fn fixture_features_are_recorded() {
    let ev = eval_fixture(&[]);
    let a = "packs/data/modular_mc/a_core";
    let b = "packs/data/modular_mc/b_extra";
    assert_eq!(
        ev.scripts,
        [
            format!("{a}/scripts/main.ts"),
            format!("{b}/scripts/extra.ts")
        ]
        .map(|s| rel(&s))
    );
    assert_eq!(ev.env_read["FIXTURE_EDITION"], None);
    assert!(
        ev.logs
            .iter()
            .any(|l| l.contains("Skipped exporting") && l.contains("shared.item.json"))
    );
    let last = |p: &str| ev.outputs[p].contributors.last().unwrap().clone();
    let c = last("BP/items/shared.item.json");
    assert_eq!((c.policy, c.applied), (Policy::Skip, Applied::Skipped));
    assert_eq!(
        ev.outputs["BP/items/shared.item.json"].contributors.len(),
        2
    );
    let c = last("RP/textures/item_texture.json");
    assert_eq!((c.policy, c.applied), (Policy::Merge, Applied::Merged));
    let c = last("BP/recipes/golem.json");
    assert_eq!(
        (c.policy, c.applied),
        (Policy::Overwrite, Applied::Overwrote)
    );
    assert!(matches!(
        ev.outputs["BP/recipes/golem.json"].body,
        Body::Passthrough(_)
    ));
    let c = last("RP/texts/en_US.lang");
    assert_eq!(
        (c.policy, c.applied),
        (Policy::AppendEnd, Applied::Appended)
    );
    assert!(matches!(
        ev.outputs["RP/texts/en_US.lang"].body,
        Body::Text(_)
    ));
    let c = last("BP/items/paint.json");
    let Template::Json(scope) = c.template else {
        panic!()
    };
    assert_eq!(scope["health"], 20);
    assert_eq!(scope["variants"][1]["color"], "blue");
    let c = last("BP/functions/templates/setup.mcfunction");
    assert!(matches!(c.template, Template::Text(_)));
    assert_eq!(c.map, rel(&format!("{a}/_map.ts")));
    // glob expansions share the entry index
    let (r, s) = (
        last("BP/items/ruby.item.json"),
        last("BP/items/sapphire.item.json"),
    );
    assert_eq!((r.entry, s.entry), (0, 0));
    assert_eq!(r.source, rel(&format!("{b}/glob/ruby.item.json")));
}

#[test]
fn env_branch_both_ways() {
    let ev = eval_fixture(&[("FIXTURE_EDITION", "education")]);
    assert_eq!(ev.env_read["FIXTURE_EDITION"].as_deref(), Some("education"));
    let Body::Passthrough(src) = &ev.outputs["BP/edition.json"].body else {
        panic!()
    };
    assert!(src.as_str().ends_with("variants/edition.education.json"));
    let ev = eval_fixture(&[("FIXTURE_EDITION", "bedrock")]);
    let Body::Passthrough(src) = &ev.outputs["BP/edition.json"].body else {
        panic!()
    };
    assert!(src.as_str().ends_with("variants/edition.bedrock.json"));
}

fn one_map(map: &str, extra: &[(&str, &[u8])]) -> tempfile::TempDir {
    let mut f: Vec<(&str, &[u8])> = vec![("data/modular_mc/m/_map.ts", map.as_bytes())];
    f.extend_from_slice(extra);
    project(&f)
}

#[test]
fn deno_reference_is_unsupported() {
    let d = one_map("export const MAP = Deno.readTextFileSync('x');", &[]);
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert_eq!(ev.diagnostics.len(), 1, "{:?}", ev.diagnostics);
    assert_eq!(ev.diagnostics[0].code, Code::MigrateUnsupported);
    assert!(
        ev.diagnostics[0]
            .message
            .contains("data/modular_mc/m/_map.ts")
    );
    assert!(ev.diagnostics[0].message.contains("Deno"));
}

#[test]
fn budget_overrun_is_a_diagnostic() {
    let d = one_map("while (true) {}", &[]);
    let limits = Limits {
        budget: Duration::from_millis(300),
        ..migrate_limits()
    };
    let t = Instant::now();
    let ev = eval_at(d.path(), "data", &[], limits);
    assert!(t.elapsed() < Duration::from_secs(10));
    assert_eq!(ev.diagnostics[0].code, Code::MigrateUnsupported);
    assert!(
        ev.diagnostics[0].message.contains("budget"),
        "{:?}",
        ev.diagnostics
    );
}

#[test]
fn semantics_corners() {
    let map = r#"
console.log("hi", { a: 1 });
const meta = import.meta.main;
export const MAP = [
  { source: "bin.dat", target: "BP/bin.dat" },
  { source: "a.txt", target: "BP/t.txt" },
  { source: "b.txt", target: "BP/t.txt", onConflict: "appendStart" },
  { source: "c.txt", target: "BP/t.txt", onConflict: "appendEnd" },
  { source: "j.json", target: "BP/j.json", jsonTemplate: true, scope: { n: 3, l: ["x", "y"] } },
  { source: "j2.json", target: "BP/j.json", onConflict: "merge" },
  { source: "g/**/?.x", target: "RP/g/", },
  { source: "g/*.x", target: { path: "RP/flat", name: ":auto" } },
];
export const SCRIPTS = [];
console.log("meta", meta);
"#;
    let d = one_map(
        map,
        &[
            ("data/modular_mc/m/bin.dat", &[0xff, 0xfe, 0, 1]),
            ("data/modular_mc/m/a.txt", b"A"),
            ("data/modular_mc/m/b.txt", b"B"),
            ("data/modular_mc/m/c.txt", b"C\n"),
            (
                "data/modular_mc/m/j.json",
                br#"{ // c
                  "list": ["::l"], "s": ["::joinStr('-')", "a", "::n"], "o": {"::K('k')": [1,],} }"#,
            ),
            ("data/modular_mc/m/j2.json", br#"{"list": [9], "o": {"z": 1}}"#),
            ("data/modular_mc/m/g/1.x", b"1"),
            ("data/modular_mc/m/g/d/2.x", b"2"),
            ("data/modular_mc/m/g/d/22.x", b"22"),
            ("data/modular_mc/m/g/3.x", b"3"),
            ("data/modular_mc/auto-map.ts", b"export const AUTO_MAP = { '.x': 'RP/auto' };"),
        ],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert!(ev.diagnostics.is_empty(), "{:?}", ev.diagnostics);
    assert!(ev.logs.contains(&"meta false".to_string()), "{:?}", ev.logs);
    assert!(ev.logs.contains(&r#"hi {"a":1}"#.to_string()));
    assert!(matches!(
        ev.outputs["BP/bin.dat"].body,
        Body::Passthrough(_)
    ));
    // appendStart "B" + eol + "A", then appendEnd "C\n": "B\nA" has no trailing newline
    assert_eq!(ev.outputs["BP/t.txt"].body, Body::Text("B\nA\nC\n".into()));
    assert_eq!(
        ev.outputs["BP/j.json"].body,
        Body::Json(serde_json::json!({
            "list": [["x", "y"], 9], "s": "a-3", "o": {"k": [1], "z": 1}
        }))
    );
    let g: Vec<_> = ev
        .outputs
        .keys()
        .filter(|k| k.starts_with("RP/"))
        .cloned()
        .collect();
    // `**/?.x` matches g/1.x, g/3.x, g/d/2.x (not g/d/22.x); a dir target keeps only the file
    // name; the object target uses the auto-mapped name
    assert_eq!(
        g,
        [
            "RP/flat/1.x",
            "RP/flat/3.x",
            "RP/g/1.x",
            "RP/g/2.x",
            "RP/g/3.x"
        ]
    );
}

#[test]
fn stop_conflict_and_missing_source_are_diagnostics() {
    let d = one_map(
        r#"export const MAP = [{source:"a.txt",target:"BP/a"},{source:"a.txt",target:"BP/a"}];"#,
        &[("data/modular_mc/m/a.txt", b"x")],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert_eq!(ev.diagnostics[0].code, Code::MigrateUnsupported);
    assert!(ev.diagnostics[0].message.contains("already exists"));
    let d = one_map(
        r#"export const MAP = [{source:"nope.txt",target:"BP/a"}];"#,
        &[],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert!(ev.diagnostics[0].message.contains("Missing file"));
}

#[test]
fn excluded_dirs_are_invisible_to_globs_and_imports() {
    let d = one_map(
        r#"export const MAP = [{source:"**/*.txt",target:"BP/x/"}];"#,
        &[
            ("data/modular_mc/m/ok.txt", b"1"),
            ("data/modular_mc/m/node_modules/p/no.txt", b"2"),
            ("data/modular_mc/m/.regolith/no.txt", b"3"),
            ("data/modular_mc/m/.git/no.txt", b"4"),
        ],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert_eq!(ev.outputs.keys().collect::<Vec<_>>(), ["BP/x/ok.txt"]);
}

fn with_base(map: &str, files: &[(&str, &[u8])]) -> LegacyEval {
    let mut f: Vec<(&str, &[u8])> = vec![("data/modular_mc/m/_map.ts", map.as_bytes())];
    f.extend_from_slice(files);
    let d = project(&f);
    let base = BasePaths {
        bp: Some(rel("packs/BP")),
        rp: Some(rel("packs/RP")),
    };
    eval_base(d.path(), "data", base, &[], migrate_limits())
}

const MANIFEST: &[u8] = br#"{"header":{"name":"base"},"modules":[{"type":"data"}]}"#;

#[test]
fn base_files_are_the_conflict_target() {
    let ev = with_base(
        r#"export const MAP = [
          { source: "m.json", target: "BP/manifest.json", onConflict: "merge" },
          { source: "l.lang", target: "RP/texts/en_US.lang", onConflict: "appendEnd" },
          { source: "s.json", target: "BP/keep.json", onConflict: "skip" },
        ];"#,
        &[
            ("packs/BP/manifest.json", MANIFEST),
            ("packs/BP/keep.json", b"{}"),
            ("packs/BP/untouched.json", b"{}"),
            (
                "packs/RP/texts/en_US.lang",
                b"base=1
",
            ),
            (
                "data/modular_mc/m/m.json",
                br#"{"header":{"uuid":"u"},"modules":[{"type":"script"}]}"#,
            ),
            (
                "data/modular_mc/m/l.lang",
                b"new=2
",
            ),
            ("data/modular_mc/m/s.json", b"{\"x\":1}"),
        ],
    );
    assert!(ev.diagnostics.is_empty(), "{:?}", ev.diagnostics);
    assert_eq!(
        ev.outputs["BP/manifest.json"].body,
        Body::Json(serde_json::json!({
            "header": {"name": "base", "uuid": "u"},
            "modules": [{"type": "data"}, {"type": "script"}]
        }))
    );
    assert_eq!(
        ev.outputs["RP/texts/en_US.lang"].body,
        Body::Text(
            "base=1
new=2
"
            .into()
        )
    );
    let k = &ev.outputs["BP/keep.json"];
    assert_eq!(k.contributors[0].applied, Applied::Skipped);
    assert_eq!(k.body, Body::Passthrough(rel("packs/BP/keep.json")));
    // seeded-only files are not outputs
    assert_eq!(ev.outputs.len(), 3, "{:?}", ev.outputs.keys());
}

#[test]
fn stop_against_a_base_file_is_an_error() {
    let ev = with_base(
        r#"export const MAP = [{ source: "m.json", target: "BP/manifest.json" }];"#,
        &[
            ("packs/BP/manifest.json", MANIFEST),
            ("data/modular_mc/m/m.json", b"{}"),
        ],
    );
    assert_eq!(ev.diagnostics[0].code, Code::MigrateUnsupported);
    assert!(ev.diagnostics[0].message.contains("already exists"));
}

/// `caseFix`: a source that only matches case-insensitively is used, with a warning; it
/// never resolves an ambiguous name or a path outside the project.
#[test]
fn case_insensitive_sources_resolve_with_a_warning() {
    let d = one_map(
        r#"export const MAP = [{ source: "SUB/A.TXT", target: "BP/a.txt" }];"#,
        &[("data/modular_mc/m/sub/a.txt", b"x")],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert!(ev.diagnostics.is_empty(), "{:?}", ev.diagnostics);
    assert_eq!(
        ev.outputs["BP/a.txt"].body,
        Body::Passthrough(rel("data/modular_mc/m/sub/a.txt"))
    );
    assert!(
        ev.logs
            .iter()
            .any(|l| l.starts_with("Warning:") && l.contains("case-insensitively")),
        "{:?}",
        ev.logs
    );
    // `..`: never climbs out to find a match
    let d = one_map(
        r#"export const MAP = [{ source: "../../../OUT.TXT", target: "BP/o.txt" }];"#,
        &[("out.txt", b"x")],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    // rejected before any lookup (upstream: "Invalid source path ... (..)")
    assert!(
        ev.outputs.is_empty()
            && ev.diagnostics.iter().any(|x| {
                x.message.contains("parent directory") || x.message.contains("Missing file")
            }),
        "{:?}",
        ev.diagnostics
    );
    // two names differing only in case: ambiguous (only on case-sensitive file systems)
    let d = one_map(
        r#"export const MAP = [{ source: "B.TXT", target: "BP/b.txt" }];"#,
        &[("data/modular_mc/m/b.txt", b"1")],
    );
    std::fs::write(d.path().join("data/modular_mc/m/B.txt"), "2").unwrap();
    let names = std::fs::read_dir(d.path().join("data/modular_mc/m"))
        .unwrap()
        .count();
    if names == 3 {
        let ev = eval_at(d.path(), "data", &[], migrate_limits());
        assert!(
            ev.diagnostics
                .iter()
                .any(|x| x.message.contains("Missing file")),
            "{:?}",
            ev.diagnostics
        );
    } else {
        eprintln!("SKIP ambiguous case: case-insensitive file system");
    }
}

/// `MapTs.apply`: "concurrent" entries (default `stop`, no template) apply before the others,
/// so an earlier `overwrite` entry still wins over a later `stop` entry for the same target.
#[test]
fn stop_entries_apply_before_the_rest_of_their_map() {
    let d = one_map(
        r#"export const MAP = [
          { source: "new.txt", target: "BP/t.txt", onConflict: "overwrite" },
          { source: "old.txt", target: "BP/t.txt" },
        ];"#,
        &[
            ("data/modular_mc/m/new.txt", b"new"),
            ("data/modular_mc/m/old.txt", b"old"),
        ],
    );
    let ev = eval_at(d.path(), "data", &[], migrate_limits());
    assert!(ev.diagnostics.is_empty(), "{:?}", ev.diagnostics);
    let o = &ev.outputs["BP/t.txt"];
    assert_eq!(o.body, Body::Passthrough(rel("data/modular_mc/m/new.txt")));
    let applied: Vec<_> = o.contributors.iter().map(|c| c.applied).collect();
    assert_eq!(applied, [Applied::Created, Applied::Overwrote]);
}
