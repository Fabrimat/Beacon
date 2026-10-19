//! `.mcfunction` templates: reference parity (fixtures), every diagnostic class, caps, and the
//! build stage (untrusted, collisions, ledger, determinism).

mod b1_common;
use b1_common::{errors, has, opts, project, rp, session, write};
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::template::{MAX_LOOP, expand};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/template")
}

/// Every file below `dir`, `/`-separated relative path -> text.
fn tree(dir: &Path) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                m.insert(rel, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    m
}

#[test]
fn fixtures_match_the_reference() {
    for case in ["init", "warp", "story", "xp_bar"] {
        let dir = fixtures().join(case);
        let scope: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("scope.json")).unwrap())
                .unwrap();
        let mut got = BTreeMap::new();
        for (name, text) in tree(&dir.join("functions")) {
            let src = rp(&format!("src/{case}/functions/{name}"));
            let x = expand(
                &text,
                &scope,
                &src,
                &rp(&format!("functions/{case}/{name}")),
            )
            .unwrap_or_else(|d| panic!("{case}: {d:?}"));
            assert!(x.modified, "{case}");
            for f in x.files {
                assert_eq!(f.line_map.len(), f.text.split('\n').count(), "{}", f.dest);
                let rel = f
                    .dest
                    .as_str()
                    .strip_prefix(&format!("functions/{case}/"))
                    .unwrap();
                got.insert(rel.to_owned(), f.text);
            }
        }
        let want = tree(&fixtures().join(format!("{case}.expected")));
        assert_eq!(got, want, "{case}");
    }
}

fn run(text: &str, scope: Value) -> Result<Vec<(String, String)>, Diagnostic> {
    let x = expand(
        text,
        &scope,
        &rp("src/t/functions/x.mcfunction"),
        &rp("functions/t/x.mcfunction"),
    )?;
    Ok(x.files
        .into_iter()
        .map(|f| (f.dest.as_str().to_owned(), f.text))
        .collect())
}

/// The diagnostic of a failing template: its code and (line, column) in the source.
fn fail(text: &str, scope: Value) -> (Code, u32, u32, String) {
    let d = run(text, scope).expect_err(text);
    let s = d.span.expect("a span");
    assert_eq!(
        s.path.as_str(),
        "src/t/functions/x.mcfunction",
        "never an output path"
    );
    (d.code, s.line.unwrap(), s.column.unwrap(), d.message)
}

#[test]
fn syntax_diagnostics() {
    let syn = |t: &str| {
        let (c, l, col, _) = fail(t, json!({}));
        assert_eq!(c, Code::TemplateSyntax, "{t}");
        (l, col)
    };
    assert_eq!(syn("say a\n    say b\n  say c"), (3, 3)); // indent fixed by the first child
    assert_eq!(syn("say a\n  say b"), (1, 1)); // command with a block
    assert_eq!(syn("say a\nUNPACK:SUBFUNCTION"), (2, 1));
    assert_eq!(syn("UNPACK:SUBFUNCTION\nsay a"), (2, 1));
    assert_eq!(syn("UNPACK:SUBFUNCTION\n# a comment"), (2, 1));
    assert_eq!(syn("UNPACK:SUBFUNCTION\nfunction <a>:\n  say a"), (2, 1));
    assert_eq!(syn("for <i 0..2>:\n  function <same>:\n    say a"), (2, 3)); // defined twice
    assert_eq!(syn("var a = 1\n  say a"), (1, 1));
    for unsupported in [
        "if <x>:\n  say a",
        "functiontree <f><@s s 0..4>:\n  say a",
        "UNPACK:HERE",
        "> print(1)",
        "assert 1",
    ] {
        let (_, _, _, m) = fail(unsupported, json!({}));
        assert!(m.contains("not supported"), "{m}");
    }
    // a header the reference would not match, with a block
    assert_eq!(syn("say a\nfor <i 0..x>:\n  say a"), (2, 1));
    // a plain command ending in `>:` is not a template
    let x = expand(
        "say find the <key>:\n",
        &json!({}),
        &rp("x.mcfunction"),
        &rp("functions/x.mcfunction"),
    )
    .unwrap();
    assert!(!x.modified && x.files.is_empty());
}

#[test]
fn expression_diagnostics() {
    let expr = |t: &str, s: Value| {
        let (c, l, col, m) = fail(t, s);
        assert_eq!(c, Code::TemplateExpr, "{t}: {m}");
        (l, col)
    };
    assert_eq!(expr("# c\n  # d\nsay `eval:nope`", json!({})), (3, 11));
    assert_eq!(expr("say `eval: f'{a}'`", json!({"a": 1})), (1, 12));
    assert_eq!(expr("say `eval:a.upper()`", json!({"a": "x"})), (1, 12));
    assert_eq!(expr("say `eval:a / 2`", json!({"a": 2})), (1, 13));
    assert_eq!(expr("say `eval:m['k']`", json!({"m": {}})), (1, 12));
    assert_eq!(expr("say `eval:l`", json!({"l": [1]})), (1, 11));
    assert_eq!(
        expr("foreach <i v m>:\n  say a", json!({"m": {"a": 1}})),
        (1, 14)
    );
    assert_eq!(expr("for <i 0..3 0>:\n  say a", json!({})), (1, 1));
    assert_eq!(expr("var a = 1 if b else 2", json!({})).0, 1);
}

#[test]
fn caps_and_name_injection() {
    let limit = |t: &str| {
        let (c, l, _, m) = fail(t, json!({}));
        assert_eq!(c, Code::TemplateLimit, "{m}");
        l
    };
    // iteration bombs: one loop, and nested loops that emit nothing
    assert_eq!(
        limit(&format!("say a\nfor <i 0..{}>:\n  say a", MAX_LOOP + 1)),
        2
    );
    assert_eq!(
        limit("for <i 0..10000>:\n for <j 0..10000>:\n  var x = 1"),
        3
    );
    // depth bomb (block nesting) and expression nesting
    let deep: String = (0..40)
        .map(|i| format!("{}# {i}\n", " ".repeat(i)))
        .collect();
    assert_eq!(limit(&deep), 33);
    let parens = format!("say `eval:{}1{}`", "(".repeat(64), ")".repeat(64));
    assert_eq!(limit(&parens), 1);
    // output lines
    assert_eq!(
        limit("for <i 0..10000>:\n for <j 0..10000>:\n  say `eval:i`"),
        3
    );
    // names: `..`, absolute, reserved, and the same through an eval hole
    let syn = |t: &str, s: Value| {
        let (c, l, _, m) = fail(t, s);
        assert_eq!(c, Code::TemplateSyntax, "{t}: {m}");
        l
    };
    assert_eq!(syn("function <../x>:\n  say a", json!({})), 1);
    assert_eq!(syn("function </x>:\n  say a", json!({})), 1);
    assert_eq!(syn("definefunction <con>:\n  say a", json!({})), 1);
    assert_eq!(
        syn("say a\nfunction <`eval:p`>:\n  say a", json!({"p": "../x"})),
        2
    );
    assert_eq!(
        syn("function <`eval:p`>:\n  say a", json!({"p": "/abs"})),
        1
    );
    assert_eq!(
        syn("function <`eval:p`>:\n  say a", json!({"p": "a/../../b"})),
        1
    );
    // without a block it is only a command (as in the reference): no file is created
    assert_eq!(
        run("say `eval:p`\nfunction <`eval:p`>:", json!({"p": "../x"})).unwrap(),
        [(
            "functions/t/x.mcfunction".to_owned(),
            "say ../x\nfunction <../x>:".to_owned()
        )]
    );
}

#[test]
fn deterministic() {
    let text =
        std::fs::read_to_string(fixtures().join("story/functions/story.mcfunction")).unwrap();
    let scope: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("story/scope.json")).unwrap(),
    )
    .unwrap();
    let a = expand(
        &text,
        &scope,
        &rp("s.mcfunction"),
        &rp("functions/s.mcfunction"),
    )
    .unwrap();
    let b = expand(
        &text,
        &scope,
        &rp("s.mcfunction"),
        &rp("functions/s.mcfunction"),
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(
        a.scope_reads.into_iter().collect::<Vec<_>>(),
        ["area", "flag", "game_state", "items", "str_part"]
    );
}

const WARP: &str = "UNPACK:SUBFUNCTION\ndefinefunction <setup>:\n    for <i 0..`eval:n`>:\n        function <f`eval:i`>:\n            say `eval:i`\n";

fn bp(root: &Path) -> PathBuf {
    root.join("out/main/BP/functions/t")
}

#[test]
fn untrusted_build_expands_and_the_ledger_drops_stale_subfunctions() {
    let p = project(&[
        ("src/t/functions/warp.mcfunction", WARP),
        ("src/t/scope.json", "// jsonc\n{\"n\": 3}"),
        ("src/t/functions/plain.mcfunction", "say <not a template>\n"),
    ]);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut o = opts(p.path(), None);
    o.trusted = false;
    let mut s = session(o, p.path(), &runs);
    let r = s.build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(!has(&r, Code::Untrusted), "templates are data, not code");
    let t = tree(&bp(p.path()));
    assert_eq!(
        t.keys().collect::<Vec<_>>(),
        [
            "plain.mcfunction",
            "warp/setup.mcfunction",
            "warp/setup/f0.mcfunction",
            "warp/setup/f1.mcfunction",
            "warp/setup/f2.mcfunction"
        ]
    );
    assert_eq!(
        t["warp/setup.mcfunction"],
        "function t/warp/setup/f0\nfunction t/warp/setup/f1\nfunction t/warp/setup/f2"
    );
    assert_eq!(t["plain.mcfunction"], "say <not a template>\n");

    // determinism: the same bytes again, in a fresh session
    let again = session(opts(p.path(), None), p.path(), &runs).build();
    assert!(errors(&again).is_empty());
    assert_eq!(tree(&bp(p.path())), t);

    // the loop shrinks: the ledger deletes the stale subfunctions
    write(p.path(), "src/t/scope.json", "{\"n\": 1}");
    let r = s.build();
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        tree(&bp(p.path())).into_keys().collect::<Vec<_>>(),
        [
            "plain.mcfunction",
            "warp/setup.mcfunction",
            "warp/setup/f0.mcfunction"
        ]
    );

    // a broken template is reported at its source line and ships nothing
    write(
        p.path(),
        "src/t/functions/warp.mcfunction",
        "for <i 0..2>:\n    say `eval:missing`\n",
    );
    let r = s.check();
    let d = errors(&r);
    assert_eq!(d.len(), 1, "{d:?}");
    let span = d[0].span.as_ref().unwrap();
    assert_eq!(
        (d[0].code, span.path.as_str(), span.line),
        (
            Code::TemplateExpr,
            "src/t/functions/warp.mcfunction",
            Some(2)
        )
    );
}

#[test]
fn a_hand_written_subfunction_collides() {
    let p = project(&[
        ("src/t/functions/warp.mcfunction", WARP),
        ("src/t/scope.json", "{\"n\": 1}"),
        ("src/t/functions/warp/setup.mcfunction", "say by hand\n"),
    ]);
    let runs = Arc::new(AtomicUsize::new(0));
    let r = session(opts(p.path(), None), p.path(), &runs).check();
    let c: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::RouteCollision)
        .collect();
    assert_eq!(c.len(), 1, "{:?}", r.diagnostics);
    let mut paths = vec![c[0].span.as_ref().unwrap().path.as_str()];
    paths.extend(c[0].related.iter().map(|s| s.path.as_str()));
    paths.sort();
    assert_eq!(
        paths,
        [
            "src/t/functions/warp.mcfunction",
            "src/t/functions/warp/setup.mcfunction"
        ]
    );
}

/// `f` fails with `TemplateLimit` well under the wall-clock bound.
fn limit_fast(text: &str, scope: Value) {
    let t = std::time::Instant::now();
    let (c, _, _, m) = fail(text, scope);
    assert_eq!(c, Code::TemplateLimit, "{m}");
    // unoptimized builds scan ~50x slower; the bound that matters is the release one
    let bound = if cfg!(debug_assertions) { 30 } else { 5 };
    assert!(t.elapsed().as_secs() < bound, "{:?}", t.elapsed());
}

#[test]
fn loop_iterations_cost_steps() {
    limit_fast(
        "for <a 0..100>:\n  for <b 0..10000>:\n    for <c 0..10000>:",
        json!({}),
    );
    let big: Vec<u32> = (0..10_000).collect();
    limit_fast(
        "foreach <i v big>:\n  for <b 0..10000>:\n    for <c 0..10000>:",
        json!({ "big": big }),
    );
}

#[test]
fn large_values_cost_steps() {
    // doubling a string stops at MAX_VALUE_BYTES
    let doubling = format!("var s = 'ab'\n{}", "var s = s + s\n".repeat(30));
    limit_fast(&doubling, json!({}));
    // copying a large value over and over runs out of steps
    let copies = format!(
        "var s = 'ab'\n{}{}",
        "var s = s + s\n".repeat(16),
        "var t = s + ''\n".repeat(100_000)
    );
    limit_fast(&copies, json!({}));
    let copies = "var t = big\n".repeat(100_000);
    limit_fast(&copies, json!({ "big": "x".repeat(200_000) }));
    // subscripts borrow: a big list read 10k times is cheap
    let big: Vec<String> = (0..10_000).map(|i| format!("{i:0>100}")).collect();
    let t = std::time::Instant::now();
    let out = run(
        "for <i 0..10000>:\n  say `eval:big[i]`",
        json!({ "big": big }),
    )
    .unwrap();
    assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
    assert_eq!(out[0].1.lines().count(), 10_000);
}

#[test]
fn long_lines_cost_steps() {
    let mb = "x".repeat(1 << 20);
    limit_fast(
        &format!("for <a 0..10000>:\n  for <b 0..100>:\n    var x = '{mb}'"),
        json!({}),
    );
    limit_fast(
        &format!(
            "for <a 0..10000>:\n  for <b 0..100>:\n{}## {mb}",
            " ".repeat(1 << 20)
        ),
        json!({}),
    );
}
