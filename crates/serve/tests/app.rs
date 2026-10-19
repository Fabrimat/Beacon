//! P5-1C: `AppCore` (trust prompt/grant, init, sanitize) and the per-call daemon client.
//!
//! `harness = false`: this binary is also the daemon the client starts (`serve --project
//! <root>`, standing in for the bundled `beacon`). `BEACON_HOME` and `BEACON_SERVE_IDLE=0`
//! are set once for the whole run; tests run one after the other.

use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use beacon_engine::{plugin, trust};
use beacon_serve::app::client::{self, MAX_GRANTS};
use beacon_serve::app::{AppCore, COMMANDS, MAX_TEXT, TrustState, sanitize};
use beacon_serve::{Grant, daemon, discovery};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

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
        Box::new(|root, o: BuildOptions| Session::new(root, o, Box::new(NoHost), None)),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `serve --project <root>`: what the client starts
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
            "init_create_makes_a_trusted_project",
            init_create_makes_a_trusted_project,
        ),
        (
            "trust_grant_with_a_stale_list_records_nothing",
            trust_grant_with_a_stale_list_records_nothing,
        ),
        (
            "project_text_is_sanitized_and_capped",
            project_text_is_sanitized_and_capped,
        ),
        ("sanitize_caps_and_strips", sanitize_caps_and_strips),
        (
            "shell_allows_exactly_the_commands",
            shell_allows_exactly_the_commands,
        ),
        (
            "client_round_trips_a_check_call",
            client_round_trips_a_check_call,
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

fn create(dir: &Path) -> beacon_serve::app::ProjectSummary {
    core()
        .init_create(json!({ "folder": dir, "name": "Demo", "agentFiles": false }))
        .unwrap_or_else(|e| panic!("init_create: {e:?}"))
}

/// Rewrites `config.json`'s plugin list (a plugin changes the trust key, 5.5).
fn set_plugins(dir: &Path, plugins: Value) {
    let p = dir.join("config.json");
    let mut c: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    c["beacon"]["plugins"] = plugins;
    std::fs::write(p, serde_json::to_vec_pretty(&c).unwrap()).unwrap();
}

fn init_create_makes_a_trusted_project() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    let s = create(&dir);
    assert_eq!(s.name, "Demo");
    assert_eq!(s.trust, TrustState::Trusted);
    assert!(dir.join("config.json").is_file());
    assert!(trust::is_trusted(&dir, &plugin::project_trust_key(&dir)));
    assert!(s.play_target.is_some(), "the template has a dev target");
    // not empty any more: a second create is refused, with a BCN code
    let e = core()
        .init_create(json!({ "folder": dir }))
        .expect_err("non-empty folder");
    assert!(e.code.starts_with("BCN"), "{e:?}");
}

fn trust_grant_with_a_stale_list_records_nothing() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    create(&dir);
    set_plugins(
        &dir,
        json!([{ "legacyFilter": { "id": "a", "cmd": ["one"] } }]),
    );
    let c = core();
    assert_eq!(
        c.project_open(dir.clone()).unwrap().trust,
        TrustState::Untrusted
    );
    let shown = c.trust_prompt(dir.clone()).unwrap();
    let stale = shown.key.clone();
    assert_eq!(
        shown.runs.len(),
        2,
        "scripts + the plugin: {:?}",
        shown.runs
    );
    // the project changes between the prompt and the click
    set_plugins(
        &dir,
        json!([{ "legacyFilter": { "id": "a", "cmd": ["two"] } }]),
    );
    assert_eq!(
        c.trust_grant(dir.clone(), shown.key.clone()).unwrap(),
        TrustState::Untrusted
    );
    assert!(!trust::is_trusted(&dir, &plugin::project_trust_key(&dir)));
    assert_eq!(
        c.project_open(dir.clone()).unwrap().trust,
        TrustState::Untrusted
    );
    // a fresh prompt for the current project is honoured
    let shown = c.trust_prompt(dir.clone()).unwrap();
    assert_eq!(
        c.trust_grant(dir.clone(), shown.key).unwrap(),
        TrustState::Trusted
    );
    assert_eq!(
        c.project_open(dir.clone()).unwrap().trust,
        TrustState::Trusted
    );
    // a stale click on an already trusted project writes nothing and leaves it trusted
    assert_eq!(c.trust_grant(dir, stale).unwrap(), TrustState::Trusted);
}

fn sanitize_caps_and_strips() {
    assert_eq!(
        sanitize("\u{1b}[31mred\u{1b}[0m \u{1b}]0;title\u{7}ok\r\n\ttab\u{0}\u{7f}"),
        "red 0;titleok\n\ttab"
    );
    let long = "é".repeat(MAX_TEXT);
    let s = sanitize(&long);
    assert!(s.len() <= MAX_TEXT && s.chars().all(|c| c == 'é'));
}

/// Project-derived text reaches the webview only sanitized: errors that quote `config.json`
/// and the trust prompt's runs and plugin details.
fn project_text_is_sanitized_and_capped() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    create(&dir);
    let evil = format!("\u{1b}[31mX\u{7}{}", "y".repeat(2 * MAX_TEXT));
    set_plugins(
        &dir,
        json!([{ "legacyFilter": { "id": evil, "cmd": [evil] } }]),
    );
    let p = core().trust_prompt(dir.clone()).unwrap();
    let shown = serde_json::to_string(&(&p.runs, &p.plugins)).unwrap();
    assert!(!shown.contains("\\u001b") && !shown.contains("\\u0007"));
    assert!(p.runs.iter().all(|r| r.len() <= MAX_TEXT));
    assert!(shown.len() < 4 * MAX_TEXT, "capped: {}", shown.len());
    // ... while the key stays verbatim, so it can still be granted exactly
    assert!(serde_json::to_string(&p.key).unwrap().contains("\\u001b"));
    assert_eq!(
        core().trust_grant(dir.clone(), p.key).unwrap(),
        TrustState::Trusted
    );
    // an error quoting the project
    let c = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(c.contains("\"namespace\": \"demo\""), "{c}");
    std::fs::write(
        dir.join("config.json"),
        c.replace(
            "\"namespace\": \"demo\"",
            "\"namespace\": \"\\u001b[31mBAD\\u0007\"",
        ),
    )
    .unwrap();
    let e = core().project_open(dir).expect_err("bad namespace");
    assert!(
        e.code.starts_with("BCN") && !e.message.contains('\u{1b}'),
        "{e:?}"
    );
    assert!(e.message.contains("BAD"), "the text is kept: {e:?}");
}

fn client_round_trips_a_check_call() {
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    let s = create(&dir);
    let exe = std::env::current_exe().unwrap();
    // a read tool needs no grant; the daemon is started from `exe serve --project <root>`
    let out = client::call_tool(&exe, &dir, None, "check", json!({})).expect("check");
    let sarif = out.json().expect("SARIF in the first block");
    assert_eq!(sarif["version"], "2.1.0", "{sarif}");
    assert!(sarif["runs"][0]["results"].is_array());
    // trust is the stored decision (init trusted the project), never the client's say-so
    let info = client::call_tool(&exe, &s.root, None, "project_info", json!({}))
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(info["trust"]["trusted"], true, "{info}");
    // the app never asks for build or release
    assert_eq!(MAX_GRANTS, [Grant::Edit, Grant::Deploy]);
    let e = client::call_tool(&exe, &dir, Some(Grant::Build), "check", json!({}))
        .expect_err("build grant refused");
    assert_eq!(e.code, "BCN0161");
    // a real write without the grant names it (BCN0162)
    let e = client::call_tool(
        &exe,
        &dir,
        Some(Grant::Edit),
        "deploy",
        json!({ "dryRun": false }),
    )
    .expect_err("deploy needs its grant");
    assert_eq!(e.code, "BCN0162", "{e:?}");
    wait_gone(&dir);
}

/// BEACON_SERVE_IDLE=0: the daemon leaves with its last client.
fn wait_gone(dir: &Path) {
    let run = discovery::run_dir().unwrap();
    let f = discovery::discovery_path(&run, &discovery::project_key(dir));
    let t0 = Instant::now();
    while f.exists() && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    // a leaked client connection keeps the daemon (and its discovery file) alive
    assert!(!f.exists(), "the daemon did not leave with its last client");
    std::thread::sleep(Duration::from_millis(200));
}

/// The Tauri shell (apps/desktop, outside this workspace) exposes exactly COMMANDS: the
/// capability allows one permission per command, and each is a handler forwarding to AppCore.
fn shell_allows_exactly_the_commands() {
    let shell = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/src-tauri");
    let cap: Value =
        serde_json::from_slice(&std::fs::read(shell.join("capabilities/default.json")).unwrap())
            .unwrap();
    let mut allowed: Vec<String> = cap["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        // the one non-command permission: the webview listens to (and unlistens from) `beacon://*` events
        .filter(|p| p != "core:event:allow-listen" && p != "core:event:allow-unlisten")
        .collect();
    let mut want: Vec<String> = COMMANDS
        .iter()
        .map(|c| format!("allow-{}", c.replace('_', "-")))
        .collect();
    allowed.sort();
    want.sort();
    assert_eq!(allowed, want, "capabilities/default.json vs app::COMMANDS");
    let main = std::fs::read_to_string(shell.join("src/main.rs")).unwrap();
    let handler = main.split("generate_handler![").nth(1).unwrap();
    let handler = handler.split(']').next().unwrap();
    let mut listed: Vec<&str> = handler
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect();
    let mut cmds = COMMANDS.to_vec();
    listed.sort();
    cmds.sort();
    assert_eq!(listed, cmds, "generate_handler! vs app::COMMANDS");
    for c in COMMANDS {
        assert!(main.contains(&format!("fn {c}(")), "no command fn {c}");
    }
}
