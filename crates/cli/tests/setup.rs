//! `beacon setup` / `beacon doctor` with `BEACON_HOME` in a temp dir and no network. The esbuild
//! pin's tarball cannot be forged, so "installed" is seeded the way `install` leaves it (the
//! binary plus its `.blake3` sidecar); every other tool stays missing.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn beacon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_beacon"))
        .args(args)
        .env("BEACON_HOME", home)
        .env_remove("BEACON_DOWNLOAD")
        .env_remove("BEACON_BDS_EULA")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)))
}

fn step<'a>(v: &'a Value, tool: &str) -> &'a Value {
    v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["tool"] == tool)
        .unwrap_or_else(|| panic!("no step {tool}"))
}

fn esbuild_exe(home: &Path) -> PathBuf {
    beacon_bundler::installed_path(&home.join("tools"), &beacon_bundler::platform())
}

fn seed_esbuild(home: &Path) -> PathBuf {
    let exe = esbuild_exe(home);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"fake esbuild").unwrap();
    let mut side = exe.clone().into_os_string();
    side.push(".blake3");
    std::fs::write(side, blake3::hash(b"fake esbuild").to_hex().as_str()).unwrap();
    exe
}

/// Nothing was downloaded: no file other than what the test seeded is in the tools dir.
fn tools_files(home: &Path) -> usize {
    std::fs::read_dir(home.join("tools")).map_or(0, |d| d.count())
}

#[test]
fn dry_run_json_shape() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(
        h.path(),
        &[
            "setup",
            "--json",
            "--dry-run",
            "esbuild",
            "api-index",
            "bds",
            "--yes",
        ],
    );
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(v["schema"], 1);
    assert!(v["installs"].is_array() && v["diagnostics"].is_array());
    let tools: Vec<_> = v["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["tool"])
        .collect();
    assert_eq!(tools, ["esbuild", "apiIndex", "bds"]);
    for s in v["steps"].as_array().unwrap() {
        assert_eq!(s["status"], "missing");
        assert!(s["bytes"].as_u64().unwrap() > 0 && !s["version"].as_str().unwrap().is_empty());
    }
    assert_eq!(step(&v, "esbuild")["defaultSelected"], true);
    assert_eq!(step(&v, "bds")["defaultSelected"], false);
    assert_eq!(step(&v, "bds")["needsEula"], true);
    assert_eq!(tools_files(h.path()), 0, "dry run downloads nothing");
}

#[test]
fn named_tool_only_and_no_names_installs_nothing() {
    let h = tempfile::tempdir().unwrap();
    seed_esbuild(h.path());
    // no tool names: report only
    let v = json(&beacon(h.path(), &["setup", "--json", "--yes"]));
    assert_eq!(step(&v, "esbuild")["status"], "installed");
    assert_eq!(step(&v, "apiIndex")["status"], "missing");
    assert_eq!(step(&v, "bds")["status"], "missing");
    // esbuild named and present: stays installed, the others are untouched (no download)
    let o = beacon(h.path(), &["setup", "esbuild", "--json", "--yes"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    assert_eq!(step(&v, "esbuild")["status"], "installed");
    assert_eq!(step(&v, "apiIndex")["status"], "missing");
    assert_eq!(
        tools_files(h.path()),
        2,
        "only the seeded binary and sidecar"
    );
}

#[test]
fn unconfirmed_download_is_declined() {
    let h = tempfile::tempdir().unwrap();
    // no --yes, no BEACON_DOWNLOAD, stdin is not a TTY: the prompt is a refusal
    let o = beacon(h.path(), &["setup", "esbuild", "api-index", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v = json(&o);
    for t in ["esbuild", "apiIndex"] {
        assert_eq!(step(&v, t)["status"], "declined", "{t}");
        assert_eq!(step(&v, t)["diagnostic"]["code"], code_needs_confirm());
    }
    assert_eq!(step(&v, "bds")["status"], "missing");
    assert_eq!(tools_files(h.path()), 0);
}

fn code_needs_confirm() -> String {
    beacon_engine::diag::Code::DownloadNeedsConfirm
        .id()
        .to_owned()
}

#[test]
fn bds_without_eula_is_bcn0193_and_downloads_nothing() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["setup", "bds", "--json", "--yes"]);
    assert_eq!(o.status.code(), Some(2));
    let v = json(&o);
    let s = step(&v, "bds");
    assert_eq!(s["status"], "declined");
    assert_eq!(s["diagnostic"]["code"], "BCN0193");
    assert!(
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "BCN0193")
    );
    assert_eq!(tools_files(h.path()), 0, "no download, no EULA log");
}

#[test]
fn doctor_reports_and_fails_on_a_corrupted_pin() {
    let h = tempfile::tempdir().unwrap();
    let o = beacon(h.path(), &["doctor", "--json"]);
    assert_eq!(o.status.code(), Some(0), "missing tools are not a failure");
    let v = json(&o);
    assert_eq!(v["schema"], 1);
    assert_eq!(v["steps"].as_array().unwrap().len(), 3);

    let exe = seed_esbuild(h.path());
    let o = beacon(h.path(), &["doctor", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(step(&json(&o), "esbuild")["status"], "installed");

    std::fs::write(&exe, b"tampered").unwrap();
    let o = beacon(h.path(), &["doctor", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v = json(&o);
    assert_eq!(step(&v, "esbuild")["status"], "failed");
    assert_eq!(
        step(&v, "esbuild")["diagnostic"]["code"],
        code_hash_mismatch()
    );
}

fn code_hash_mismatch() -> String {
    beacon_engine::diag::Code::DownloadHashMismatch
        .id()
        .to_owned()
}

#[test]
fn bcn0200_fix_text() {
    let doc = beacon_engine::diag::Code::MinecraftNotFound.doc();
    assert!(doc.contains("Start Minecraft once, sign in and reach the main menu, then retry."));
}
