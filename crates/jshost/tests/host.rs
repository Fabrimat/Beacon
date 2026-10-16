use beacon_jshost::{Evaluator, Files, Limits, QuickJs};
use std::time::{Duration, Instant};

fn files(entry: &str, src: &str) -> Files {
    Files::from([(entry.to_string(), src.to_string())])
}

#[test]
fn items_come_back_as_json() {
    let mut f = files(
        "a.gen.ts",
        r#"import { sound, assets, entity } from "beacon:runtime";
           import t from "./t.json";
           import { k } from "./lib";
           const n: number = 2;
           for (const p of assets("sounds/**/*.ogg")) sound(p, { n });
           entity("demo:car", { ...t, k });"#,
    );
    f.insert("t.json".into(), r#"{"a":1}"#.into());
    f.insert("lib.ts".into(), "export const k: string = 'v';".into());
    let assets = ["sounds/a/x.ogg", "sounds/y.ogg", "sounds/z.wav"].map(String::from);
    let out = QuickJs::new(Limits::default())
        .unwrap()
        .eval("a.gen.ts", &f, &assets)
        .unwrap();
    assert_eq!(
        serde_json::Value::Array(out.items),
        serde_json::json!([
            {"kind":"sound","id":"sounds/a/x.ogg","n":2},
            {"kind":"sound","id":"sounds/y.ogg","n":2},
            {"kind":"entity","id":"demo:car","a":1,"k":"v"}
        ])
    );
    assert!(!out.uncacheable);
}

#[test]
fn nondeterminism_is_flagged() {
    let f = files("a.js", "Math.random();");
    let out = QuickJs::new(Limits::default())
        .unwrap()
        .eval("a.js", &f, &[])
        .unwrap();
    assert!(out.uncacheable);
}

#[test]
fn runaway_loop_is_interrupted() {
    let limits = Limits {
        budget: Duration::from_millis(200),
        ..Limits::default()
    };
    let js = QuickJs::new(limits).unwrap();
    let t = Instant::now();
    let r = js.eval("a.js", &files("a.js", "while (true) {}"), &[]);
    assert!(r.is_err());
    assert!(t.elapsed() < Duration::from_secs(2));
    // Host stays usable afterwards.
    assert!(js.eval("b.js", &files("b.js", "1"), &[]).is_ok());
}

#[test]
fn memory_limit_is_an_error() {
    let limits = Limits {
        memory: 4 << 20,
        ..Limits::default()
    };
    let js = QuickJs::new(limits).unwrap();
    let r = js.eval(
        "a.js",
        &files(
            "a.js",
            "const a = []; while (true) a.push(new Array(1000).fill(1));",
        ),
        &[],
    );
    assert!(r.is_err());
}

#[test]
fn contexts_do_not_share_globals() {
    let js = QuickJs::new(Limits::default()).unwrap();
    js.eval(
        "a.js",
        &files(
            "a.js",
            "globalThis.leak = 1; Object.prototype.polluted = 1;",
        ),
        &[],
    )
    .unwrap();
    let out = js
        .eval(
            "b.js",
            &files("b.js", "import { generate } from 'beacon:runtime'; generate({ leak: typeof globalThis.leak, polluted: typeof ({}).polluted });"),
            &[],
        )
        .unwrap();
    assert_eq!(out.items[0]["leak"], "undefined");
    assert_eq!(out.items[0]["polluted"], "undefined");
}
