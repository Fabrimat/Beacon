//! Templates in the symbol table: symbols are scanned from the expansion and mapped back to
//! the template source (partial ids, loops collapsing to one range, computed ids, function
//! definitions, `scope.json` invalidation, overlays), through a real build session.

mod b1_common;
use b1_common::{opts, project, rp, session, write};
use beacon_engine::build::Session;
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::symbols::{self, SymbolKind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const ENTITY: &str = r#"{"minecraft:entity":{"description":{"identifier":"demo:x"}}}"#;
const LOOP: &str = "src/t/functions/loop.mcfunction";

fn checked(root: &Path) -> (Session, Vec<Diagnostic>) {
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = session(opts(root, None), root, &runs);
    let d = s.check().diagnostics;
    (s, d)
}

fn unresolved(d: &[Diagnostic]) -> Vec<&Diagnostic> {
    d.iter()
        .filter(|d| d.code == Code::SymbolUnresolved)
        .collect()
}

#[test]
fn partial_ids_literal_loops_and_computed_ids() {
    let src = "for <i 0..3>:\n    summon demo:npc_`eval:i` ~ ~ ~\n    execute as @e[type=demo:x] run say hi `eval:i`\n";
    let p = project(&[
        (LOOP, src),
        ("src/t/entities/x.entity.json", ENTITY),
        (
            "src/t/entities/npc.entity.json",
            r#"{"minecraft:entity":{"description":{"identifier":"demo:npc_1"}}}"#,
        ),
    ]);
    let (s, d) = checked(p.path());
    let t = s.symbols().unwrap();
    // the backtick no longer cuts the id: no `demo:npc_`, the concrete ones instead
    assert!(t.references(SymbolKind::Entity, "demo:npc_").is_empty());
    let bad: Vec<_> = unresolved(&d)
        .iter()
        .map(|d| {
            let sp = d.span.as_ref().unwrap();
            (d.message.clone(), sp.path.as_str().to_owned(), sp.line)
        })
        .collect();
    // npc_0 and npc_2 are not defined; the span is the source command, never an output path
    assert_eq!(bad.len(), 2, "{d:?}");
    assert!(bad[0].0.contains("demo:npc_0") && bad[1].0.contains("demo:npc_2"));
    assert!(bad.iter().all(|b| b.1 == LOOP && b.2 == Some(2)), "{bad:?}");

    // computed: resolves (npc_1 is defined), cannot be renamed
    let r = t.references(SymbolKind::Entity, "demo:npc_1");
    assert_eq!(r.len(), 1);
    assert!(r[0].occ.range.is_none() && !r[0].editable);
    let e = symbols::rename(t, None, "demo:npc_1", "demo:foo").unwrap_err();
    assert_eq!(e.code, Code::RenameBlocked, "{e:?}");

    // literal in a loop: three iterations, one edit, at the source range
    let fixes = symbols::rename(t, None, "demo:x", "demo:y").unwrap();
    let hits: Vec<_> = fixes.iter().filter(|f| f.path.as_str() == LOOP).collect();
    assert_eq!(hits.len(), 1, "{fixes:?}");
    assert_eq!(&src[hits[0].start..hits[0].end], "demo:x");
    assert!(src[..hits[0].start].ends_with("type="));
}

#[test]
fn functions_from_headers_and_generators_define_functions() {
    let tpl = "definefunction <helper>:\n    say hi\nfor <i 0..2>:\n    execute as @a run function <step_`eval:i`>:\n        say `eval:i`\n";
    let p = project(&[
        ("src/t/functions/fns.mcfunction", tpl),
        (
            "src/t/functions/caller.mcfunction",
            "function t/fns/helper\nfunction t/fns/step_1\nfunction t/fns/nope\nfunction t/gen\n",
        ),
        // the fake host emits `functions/t/gen.mcfunction` as generated text
        ("src/t/gen.gen.ts", ""),
    ]);
    let (s, d) = checked(p.path());
    let u = unresolved(&d);
    assert_eq!(u.len(), 1, "{d:?}");
    assert!(u[0].message.contains("t/fns/nope"));
    let t = s.symbols().unwrap();
    for name in [
        "t/fns",
        "t/fns/helper",
        "t/fns/step_0",
        "t/fns/step_1",
        "t/gen",
    ] {
        assert_eq!(t.definitions(SymbolKind::Function, name).len(), 1, "{name}");
    }
    // the literal header names its function; a computed one has no range
    let lit = &t.definitions(SymbolKind::Function, "t/fns/helper")[0];
    let r = lit.occ.range.unwrap();
    assert_eq!(&tpl[r.start..r.end], "helper");
    let comp = &t.definitions(SymbolKind::Function, "t/fns/step_1")[0];
    assert!(comp.occ.range.is_none());
}

#[test]
fn scope_json_edit_rescans_the_templates_of_its_module() {
    let p = project(&[
        (LOOP, "for <i 0..1>:\n    summon demo:`eval:n`\n"),
        ("src/t/scope.json", r#"{"n": "a"}"#),
    ]);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut s = session(opts(p.path(), None), p.path(), &runs);
    s.check();
    let n = |s: &Session, id: &str| {
        s.symbols()
            .unwrap()
            .references(SymbolKind::Entity, id)
            .len()
    };
    assert_eq!((n(&s, "demo:a"), n(&s, "demo:b")), (1, 0));
    write(p.path(), "src/t/scope.json", r#"{"n": "b"}"#);
    s.check();
    assert_eq!((n(&s, "demo:a"), n(&s, "demo:b")), (0, 1));
}

#[test]
fn overlay_reexpands_and_a_broken_buffer_recovers() {
    let p = project(&[(LOOP, "for <i 0..1>:\n    summon demo:a\n")]);
    let (mut s, d) = checked(p.path());
    assert!(!d.iter().any(|d| d.code == Code::TemplateSyntax));
    let f = rp(LOOP);
    let n = |s: &Session, id: &str| {
        s.symbols()
            .unwrap()
            .references(SymbolKind::Entity, id)
            .len()
    };

    s.symbols_mut()
        .unwrap()
        .overlay(&f, "for <i 0..4>:\n    summon demo:b\n");
    assert_eq!((n(&s, "demo:a"), n(&s, "demo:b")), (0, 1));

    // broken (bad indent): no panic, no symbols, the template's diagnostic from the buffer
    s.symbols_mut()
        .unwrap()
        .overlay(&f, "for <i 0..4>:\n    summon demo:b\n  say x\n");
    assert_eq!((n(&s, "demo:a"), n(&s, "demo:b")), (0, 0));
    let d = s.check().diagnostics;
    let e: Vec<_> = d
        .iter()
        .filter(|d| d.code == Code::TemplateSyntax)
        .collect();
    assert_eq!(e.len(), 1, "{d:?}");
    let sp = e[0].span.as_ref().unwrap();
    assert_eq!((sp.path.as_str(), sp.line), (LOOP, Some(3)));

    // fixed: the diagnostic and the symbols come back
    s.symbols_mut()
        .unwrap()
        .overlay(&f, "for <i 0..4>:\n    summon demo:b\n");
    assert_eq!(n(&s, "demo:b"), 1);
    let d = s.check().diagnostics;
    assert!(!d.iter().any(|d| d.code == Code::TemplateSyntax), "{d:?}");
}

#[test]
fn occurrences_after_an_eval_hole_have_no_range() {
    // the hole's value is longer than its source: later output offsets are not source offsets,
    // whatever bytes happen to sit there (a coincidence must not become an edit)
    let src = "say `eval:p` demo:x demo:x demo:x demo:xy\n";
    let ent =
        |id: &str| format!(r#"{{"minecraft:entity":{{"description":{{"identifier":"{id}"}}}}}}"#);
    let p = project(&[
        (LOOP, src),
        ("src/t/scope.json", r#"{"p": "0123456789abcde"}"#),
        ("src/t/entities/x.entity.json", &ent("demo:x")),
        ("src/t/entities/xy.entity.json", &ent("demo:xy")),
    ]);
    let (s, d) = checked(p.path());
    assert!(unresolved(&d).is_empty(), "{d:?}");
    let t = s.symbols().unwrap();
    assert!(
        t.references(SymbolKind::Entity, "demo:x")
            .iter()
            .all(|l| l.path.as_str() != LOOP || l.occ.range.is_none())
    );
    for (from, to) in [("demo:x", "demo:yy"), ("demo:xy", "demo:z")] {
        let e = symbols::rename(t, None, from, to).unwrap_err();
        assert_eq!(e.code, Code::RenameBlocked, "{from}: {e:?}");
    }
}
