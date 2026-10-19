//! Phase 5 app budgets (`perf app`): `installer_size_mb` and `app_idle_rss_mb`.
//!
//! ```text
//! perf app --app <installed Beacon app exe> [--installer <setup.exe>] [--idle-secs 90]
//!          [--json out.json] [--enforce]
//! ```
//!
//! The app (the GUI exe, its `beacon.exe` sibling is the daemon binary) is started the way a
//! user does (window + WebView2 tree). A project is then created and Played into a temp
//! com.mojang by `<app> --smoke` (the same `AppCore::play`, so the daemon is the one the app
//! spawns), and:
//! - (a) **budgeted** `app_idle_rss_mb`: the app's process tree (WebView2 children included)
//!   plus the daemon (pid from `beacon serve --status`), sampled right after the Play, inside
//!   the daemon's idle window (`BEACON_SERVE_IDLE` is pinned to 60 s);
//! - (b) **reported only**: the same `--idle-secs` (90) later, after the daemon idled out.
//!
//! `BEACON_HOME` isolates discovery and tools from the user's own; nothing real is touched.

use super::{Guard, Opts, report, tree_rss_mb};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use std::{env, fs, thread};
use sysinfo::System;

/// Time for the window and the WebView2 processes to come up and settle before sampling.
const SETTLE: Duration = Duration::from_secs(10);

pub fn run(o: &Opts, app: &Path, installer: Option<&Path>) -> Result<bool, String> {
    let mut m = vec![];
    if let Some(i) = installer {
        let mb = fs::metadata(i)
            .map_err(|e| format!("{}: {e}", i.display()))?
            .len() as f64
            / 1048576.0;
        m.push(("installer_size_mb", mb));
    }
    let work = env::temp_dir().join(format!("beacon-perf-app-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let r = rss(o, app, &work);
    let _ = fs::remove_dir_all(&work);
    let (a, b) = r?;
    println!(
        "app idle RSS (b, reported only): {b:.1} MB, {}s after the Play (daemon idled out)",
        o.idle.as_secs()
    );
    m.push(("app_idle_rss_mb", a));
    let opts = Opts {
        beacon: o.beacon.clone(),
        runs: 1,
        idle: o.idle,
        json: o.json.clone(),
        enforce: o.enforce,
        phase: 5,
    };
    Ok(report(&opts, m)? || !o.enforce)
}

/// (a, b) in MB, see the module doc.
fn rss(o: &Opts, app: &Path, work: &Path) -> Result<(f64, f64), String> {
    let beacon = app.with_file_name(if cfg!(windows) {
        "beacon.exe"
    } else {
        "beacon"
    });
    let home = work.join("home");
    let smoke = work.join("smoke");
    let env = |c: &mut Command| {
        c.env("BEACON_HOME", &home).env("BEACON_SERVE_IDLE", "60");
        c.env_remove("BEACON_COM_MOJANG");
    };
    let mut c = Command::new(app);
    env(&mut c);
    let gui = Guard(
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{}: {e}", app.display()))?,
    );
    thread::sleep(SETTLE);

    let mut c = Command::new(app);
    env(&mut c);
    let st = c
        .args(["--smoke", "--dir"])
        .arg(&smoke)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(format!("--smoke failed: {st}"));
    }
    let stop = |args: &[&str]| {
        let mut c = Command::new(&beacon);
        c.arg("serve")
            .args(args)
            .arg("--project")
            .arg(smoke.join("project"));
        env(&mut c);
        c.stdin(Stdio::null()).stderr(Stdio::null());
        c
    };
    let r = (|| {
        let out = stop(&["--status"]).output().map_err(|e| e.to_string())?;
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(|e| format!("serve --status: {e}"))?;
        let pid = v["pid"].as_u64().ok_or("serve --status: no daemon pid")? as u32;
        let mut sys = System::new();
        let a = tree_rss_mb(&mut sys, gui.0.id()) + tree_rss_mb(&mut sys, pid);
        thread::sleep(o.idle);
        // the daemon is gone by now (60 s idle < 90 s); if not, it still counts
        Ok((
            a,
            tree_rss_mb(&mut sys, gui.0.id()) + tree_rss_mb(&mut sys, pid),
        ))
    })();
    let _ = stop(&["--stop", "--force"]).stdout(Stdio::null()).status();
    r
}
