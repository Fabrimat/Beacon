//! P5-2C: setup wizard core and the esbuild first-launch copy.
//!
//! `harness = false`: this binary is also the fake `beacon` sidecar (`setup ... --json`): it
//! logs its argv to `$FAKE_BEACON_LOG` and prints a canned `SetupReport`. No network.
//! `BEACON_HOME` is a temp dir for the whole run; tests run one after the other.

use beacon_engine::setup::{SCHEMA, SetupReport, Step, StepStatus, Tool};
use beacon_serve::app::AppCore;
use std::path::Path;

fn step(tool: Tool) -> Step {
    Step {
        tool,
        version: "1".into(),
        bytes: 1,
        status: StepStatus::Missing,
        default_selected: tool != Tool::Bds,
        needs_eula: tool == Tool::Bds,
        diagnostic: None,
    }
}

fn fake_main(args: &[String]) {
    if let Ok(log) = std::env::var("FAKE_BEACON_LOG") {
        let mut l = std::fs::read_to_string(&log).unwrap_or_default();
        l.push_str(&args.join(" "));
        l.push('\n');
        std::fs::write(log, l).unwrap();
    }
    let r = SetupReport {
        schema: SCHEMA,
        installs: vec![],
        steps: vec![step(Tool::Esbuild), step(Tool::ApiIndex), step(Tool::Bds)],
    };
    println!("{}", serde_json::to_string(&r).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("setup") {
        return fake_main(&args);
    }
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded here; nothing has started yet.
    unsafe { std::env::set_var("BEACON_HOME", home.path()) };
    let filter = args.iter().find(|a| !a.starts_with('-'));
    let tests: &[(&str, fn())] = &[
        (
            "esbuild_copy_installs_verified_when_the_hash_matches",
            esbuild_copy_installs_verified_when_the_hash_matches,
        ),
        (
            "esbuild_copy_pin_matches_the_real_binary",
            esbuild_copy_pin_matches_the_real_binary,
        ),
        (
            "tampered_shipped_esbuild_is_refused_and_nothing_is_written",
            tampered_shipped_esbuild_is_refused_and_nothing_is_written,
        ),
        (
            "esbuild_copy_second_run_is_a_noop",
            esbuild_copy_second_run_is_a_noop,
        ),
        ("status_never_downloads", status_never_downloads),
        (
            "run_passes_only_the_accepted_tools",
            run_passes_only_the_accepted_tools,
        ),
        (
            "bds_without_the_eula_is_not_requested",
            bds_without_the_eula_is_not_requested,
        ),
        (
            "bds_with_the_eula_is_requested",
            bds_with_the_eula_is_requested,
        ),
        ("run_reports_progress", run_reports_progress),
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

const P: &str = "win32-x64";

fn hash(b: &[u8]) -> String {
    blake3::hash(b).to_hex().to_string()
}

fn copy(shipped: &Path, tools: &Path, want: &str) -> Result<Option<std::path::PathBuf>, String> {
    AppCore::install_shipped_esbuild_with(shipped, tools, P, want)
}

fn esbuild_copy_installs_verified_when_the_hash_matches() {
    let t = tempfile::tempdir().unwrap();
    let (shipped, tools) = (t.path().join("esbuild.exe"), t.path().join("tools"));
    std::fs::write(&shipped, b"fake esbuild").unwrap();
    let p = copy(&shipped, &tools, &hash(b"fake esbuild"))
        .unwrap()
        .expect("installed");
    assert_eq!(beacon_bundler::verified_install(&tools, P), Some(p.clone()));
    assert_eq!(std::fs::read(p).unwrap(), b"fake esbuild");
    // binary + sidecar only: no temp files left behind
    assert_eq!(std::fs::read_dir(&tools).unwrap().count(), 2);
}

/// The compiled-in hash is the real extracted binary's (needs the esbuild the other suites use).
fn esbuild_copy_pin_matches_the_real_binary() {
    let Some(real) = std::env::var_os("BEACON_ESBUILD").filter(|_| beacon_bundler::platform() == P)
    else {
        println!("  (skipped: BEACON_ESBUILD unset or not win32-x64)");
        return;
    };
    let t = tempfile::tempdir().unwrap();
    // `esbuild.exe` next to `beacon.exe`: the installed layout
    std::fs::copy(&real, t.path().join("esbuild.exe")).unwrap();
    std::fs::write(t.path().join("beacon.exe"), b"").unwrap();
    let tools = beacon_fetch::default_dir().unwrap();
    AppCore::new(t.path().join("beacon.exe")).first_launch();
    assert!(beacon_bundler::verified_install(&tools, P).is_some());
    std::fs::remove_dir_all(&tools).unwrap();
}

fn tampered_shipped_esbuild_is_refused_and_nothing_is_written() {
    let t = tempfile::tempdir().unwrap();
    let (shipped, tools) = (t.path().join("esbuild.exe"), t.path().join("tools"));
    std::fs::write(&shipped, b"tampered").unwrap();
    let e = copy(&shipped, &tools, &hash(b"genuine")).unwrap_err();
    assert!(e.contains(&hash(b"tampered")), "{e}");
    assert!(!tools.exists(), "nothing written, not even the dir");
    // not shipped at all: nothing either
    assert_eq!(copy(&t.path().join("nope"), &tools, "x"), Ok(None));
    assert!(!tools.exists());
}

fn esbuild_copy_second_run_is_a_noop() {
    let t = tempfile::tempdir().unwrap();
    let (shipped, tools) = (t.path().join("esbuild.exe"), t.path().join("tools"));
    std::fs::write(&shipped, b"fake esbuild").unwrap();
    let want = hash(b"fake esbuild");
    let dest = copy(&shipped, &tools, &want).unwrap().unwrap();
    let before = std::fs::metadata(&dest).unwrap().modified().unwrap();
    // already installed and verified: the shipped file is not even consulted
    std::fs::write(&shipped, b"other").unwrap();
    assert_eq!(copy(&shipped, &tools, &want), Ok(Some(dest.clone())));
    assert_eq!(
        std::fs::metadata(&dest).unwrap().modified().unwrap(),
        before
    );
    assert_eq!(std::fs::read(dest).unwrap(), b"fake esbuild");
}

/// A core whose `beacon` is this binary, with the fake's argv log.
fn fake() -> (AppCore, std::path::PathBuf, tempfile::TempDir) {
    let t = tempfile::tempdir().unwrap();
    let log = t.path().join("argv.log");
    // SAFETY: tests run one after the other on the main thread.
    unsafe { std::env::set_var("FAKE_BEACON_LOG", &log) };
    (AppCore::new(std::env::current_exe().unwrap()), log, t)
}

fn calls(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn status_never_downloads() {
    let (c, log, _t) = fake();
    let r = c.setup_status().unwrap();
    assert_eq!(r.steps.len(), 3);
    assert_eq!(calls(&log), ["setup --dry-run --json"]);
}

fn run_passes_only_the_accepted_tools() {
    let (c, log, _t) = fake();
    c.setup_run(&[Tool::ApiIndex], false).unwrap();
    assert_eq!(calls(&log), ["setup api-index --yes --json"]);
}

fn bds_without_the_eula_is_not_requested() {
    let (c, log, _t) = fake();
    c.setup_run(&[Tool::Bds], false).unwrap();
    // nothing left to install: only the dry-run status ran
    assert_eq!(calls(&log), ["setup --dry-run --json"]);
    std::fs::remove_file(&log).unwrap();
    c.setup_run(&[Tool::Esbuild, Tool::Bds], false).unwrap();
    assert_eq!(calls(&log), ["setup esbuild --yes --json"]);
}

fn bds_with_the_eula_is_requested() {
    let (c, log, _t) = fake();
    c.setup_run(&[Tool::Bds], true).unwrap();
    assert_eq!(calls(&log), ["setup bds --yes --accept-eula --json"]);
}

fn run_reports_progress() {
    let (c, _log, _t) = fake();
    let mut ev = vec![];
    c.setup_run_with(&[Tool::Esbuild, Tool::ApiIndex], false, &mut |p| {
        ev.push((p.task, p.step, p.percent))
    })
    .unwrap();
    let steps: Vec<_> = ev.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(steps, ["esbuild", "api-index", "done"]);
    assert!(ev.iter().all(|e| e.0 == "setup") && ev.last().unwrap().2 == Some(100));
}
