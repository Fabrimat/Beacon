//! P5-4B: `module.json` forms through the daemon's `write_settings` (grant `edit`).
//!
//! `harness = false`: this binary is also the daemon the client starts (`serve --project
//! <root>`); tests run one after the other.

use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::host::{Bundler, GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use beacon_serve::app::{AppCore, SettingsFile};
use beacon_serve::shim::{self, ShimOptions};
use beacon_serve::{Channel, daemon, discovery};
use lsp_server::{Message, Notification, Request};
use serde_json::{Value, json};
use std::io::BufReader;
use std::path::{Path, PathBuf};

struct NoHost;

impl GeneratorHost for NoHost {
    fn run(&self, _: &Generator, _: &GenCtx, _: &dyn Vfs) -> GenResult {
        GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        }
    }
    fn version(&self) -> String {
        "none".into()
    }
}

fn daemon_main(project: &Path) -> i32 {
    let opts = daemon::DaemonOptions::new(project).expect("run dir");
    daemon::run(
        &opts,
        Box::new(|root, o: BuildOptions| {
            let bundler = o
                .trusted
                .then(|| Box::new(LazyEsbuild::new(root, Confirm::No)) as Box<dyn Bundler>);
            Session::new(root, o, Box::new(NoHost), bundler)
        }),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        std::process::exit(daemon_main(Path::new(&args[2])));
    }
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded here; nothing has started yet.
    unsafe {
        std::env::set_var("BEACON_HOME", home.path());
        std::env::set_var("BEACON_SERVE_IDLE", "0");
    }
    let filter = args.iter().find(|a| !a.starts_with('-'));
    let tests: &[(&str, fn())] = &[
        (
            "one_value_changes_and_key_order_and_indent_stay",
            one_value_changes_and_key_order_and_indent_stay,
        ),
        (
            "a_stale_base_is_bcn0164_and_writes_nothing",
            a_stale_base_is_bcn0164_and_writes_nothing,
        ),
        (
            "an_invalid_value_is_bcn0003_and_writes_nothing",
            an_invalid_value_is_bcn0003_and_writes_nothing,
        ),
        (
            "paths_outside_the_modules_are_refused",
            paths_outside_the_modules_are_refused,
        ),
        (
            "a_module_json_open_in_lsp_is_bcn0163",
            a_module_json_open_in_lsp_is_bcn0163,
        ),
    ];
    let mut failed = vec![];
    for (name, t) in tests {
        if filter.is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let ok = std::panic::catch_unwind(t).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if !failed.is_empty() {
        eprintln!("FAILED: {failed:?}");
        std::process::exit(1);
    }
    println!("test result: ok");
}

fn core() -> AppCore {
    AppCore::new(std::env::current_exe().unwrap())
}

const MODULE: &str = "{\n    \"output\": \"main\",\n    \"autoTextures\": true,\n    \"paths\": {\n        \"z/*\": \"RP/z\",\n        \"a/*\": \"RP/a\"\n    }\n}\n";

/// A fresh project whose `src/main/module.json` is [`MODULE`]; returns (guard, root).
fn project() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    core()
        .init_create(json!({ "folder": dir, "name": "Demo", "agentFiles": false }))
        .unwrap_or_else(|e| panic!("init_create: {e:?}"));
    std::fs::write(dir.join("src/main/module.json"), MODULE).unwrap();
    (t, dir)
}

fn main_module() -> SettingsFile {
    SettingsFile::Module {
        module: "src/main".into(),
    }
}

fn on_disk(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("src/main/module.json")).unwrap()
}

fn one_value_changes_and_key_order_and_indent_stay() {
    let (_t, dir) = project();
    let c = core();
    assert!(
        c.settings_list(dir.clone())
            .unwrap()
            .contains(&"src/main".to_string())
    );
    let doc = c.settings_read(dir.clone(), main_module()).unwrap();
    assert_eq!(doc.value["autoTextures"], true);
    assert_eq!(doc.schema["title"], "Beacon module.json");
    let mut v = doc.value.clone();
    v["autoTextures"] = json!(false);
    let out = c
        .settings_write(dir.clone(), main_module(), doc.base.clone(), v)
        .unwrap_or_else(|e| panic!("write: {e:?}"));
    assert_eq!(on_disk(&dir), MODULE.replace("true", "false"));
    assert_ne!(out.base, doc.base);
    assert_eq!(out.value["autoTextures"], false);
    // the new base is current: a second edit on it goes through
    let mut v = out.value.clone();
    v["output"] = json!("other");
    c.settings_write(dir.clone(), main_module(), out.base, v)
        .unwrap();
    assert_eq!(
        on_disk(&dir),
        MODULE
            .replace("true", "false")
            .replace("\"main\"", "\"other\"")
    );
}

fn a_stale_base_is_bcn0164_and_writes_nothing() {
    let (_t, dir) = project();
    let c = core();
    let doc = c.settings_read(dir.clone(), main_module()).unwrap();
    // someone else edits the file after the read
    let edited = MODULE.replace("main", "edited");
    std::fs::write(dir.join("src/main/module.json"), &edited).unwrap();
    let e = c
        .settings_write(
            dir.clone(),
            main_module(),
            doc.base,
            json!({ "output": "x" }),
        )
        .expect_err("stale");
    assert_eq!(e.code, "BCN0164", "{e:?}");
    assert_eq!(on_disk(&dir), edited);
}

fn an_invalid_value_is_bcn0003_and_writes_nothing() {
    let (_t, dir) = project();
    let c = core();
    let doc = c.settings_read(dir.clone(), main_module()).unwrap();
    for bad in [json!({ "onConflict": "replace" }), json!({ "nope": 1 })] {
        let e = c
            .settings_write(dir.clone(), main_module(), doc.base.clone(), bad)
            .expect_err("invalid");
        assert_eq!(e.code, "BCN0003", "{e:?}");
    }
    assert_eq!(on_disk(&dir), MODULE);
}

fn paths_outside_the_modules_are_refused() {
    let (_t, dir) = project();
    std::fs::write(dir.join("secret.json"), "{}").unwrap();
    let c = core();
    let base = c.settings_read(dir.clone(), main_module()).unwrap().base;
    for module in [
        "../outside",
        "src/ghost",
        ".",
        "src/main/deeper",
        "src\\main",
    ] {
        let f = SettingsFile::Module {
            module: module.into(),
        };
        assert!(c.settings_read(dir.clone(), f.clone()).is_err(), "{module}");
        assert!(
            c.settings_write(dir.clone(), f, base.clone(), json!({}))
                .is_err(),
            "{module}"
        );
    }
    assert!(c.settings_read(dir.clone(), SettingsFile::Config).is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join("secret.json")).unwrap(),
        "{}"
    );
    assert_eq!(on_disk(&dir), MODULE);
}

fn a_module_json_open_in_lsp_is_bcn0163() {
    let (_t, dir) = project();
    let c = core();
    let doc = c.settings_read(dir.clone(), main_module()).unwrap();
    // an editor opens the file over the LSP channel
    let mut s = shim::connect(&ShimOptions {
        project: dir.clone(),
        channel: Channel::Lsp,
        trust: false,
        allow: vec![],
        run: discovery::run_dir().unwrap(),
        daemon: vec![
            std::env::current_exe().unwrap().into(),
            "serve".into(),
            "--project".into(),
            dir.clone().into(),
        ],
    })
    .expect("lsp connect");
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut call =
        |s: &mut beacon_serve::transport::Stream, id: i32, method: &str, params: Value| {
            Message::from(Request::new(id.into(), method.into(), params))
                .write(s)
                .unwrap();
            while let Some(m) = Message::read(&mut r).unwrap() {
                if let Message::Response(x) = m
                    && x.id == id.into()
                {
                    return;
                }
            }
            panic!("no response to {method}");
        };
    call(
        &mut s,
        1,
        "initialize",
        json!({ "capabilities": {}, "processId": null, "rootUri": null }),
    );
    Message::from(Notification::new("initialized".into(), json!({})))
        .write(&mut s)
        .unwrap();
    let uri = format!(
        "file:///{}",
        dir.join("src/main/module.json")
            .to_string_lossy()
            .replace('\\', "/")
    );
    Message::from(Notification::new(
        "textDocument/didOpen".into(),
        json!({ "textDocument": { "uri": uri, "languageId": "json", "version": 1, "text": "{ }" } }),
    ))
    .write(&mut s)
    .unwrap();
    // requests are answered in order: once this one is, the open has been handled
    call(&mut s, 2, "workspace/nope", json!({}));
    let e = c
        .settings_write(
            dir.clone(),
            main_module(),
            doc.base.clone(),
            json!({ "output": "x" }),
        )
        .expect_err("dirty");
    assert_eq!(e.code, "BCN0163", "{e:?}");
    assert_eq!(on_disk(&dir), MODULE);
    // closed: the same write goes through
    Message::from(Notification::new(
        "textDocument/didClose".into(),
        json!({ "textDocument": { "uri": uri } }),
    ))
    .write(&mut s)
    .unwrap();
    call(&mut s, 3, "workspace/nope", json!({}));
    c.settings_write(
        dir.clone(),
        main_module(),
        doc.base,
        json!({ "output": "x" }),
    )
    .unwrap_or_else(|e| panic!("after close: {e:?}"));
}
