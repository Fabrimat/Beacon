//! `Beacon.exe --smoke [--out <file>] [--dir <project dir>]`: the automatable half of exit
//! criterion 1 (`docs/phase5-plan.md` 3B), headless (no window, no webview). It runs what a
//! first session does through the same `AppCore` the commands use: `first_launch`,
//! `init_create` in a temp dir, Play into a temp com.mojang (`BEACON_COM_MOJANG`, so the game
//! is never launched and the machine's real packs are never touched), then asserts the dev
//! packs landed. One JSON object per step goes to stdout and, if `--out` is given, to that
//! file too (a release build has the GUI subsystem: stdout may not be connected). Exit 0 / 1.
//! The project dir and the daemon (60 s idle) are left behind on purpose: the caller removes
//! the dir, and the install smoke uses the live daemon to test uninstall/reinstall.

use crate::beacon_exe;
use beacon_serve::app::AppCore;
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Log {
    t0: Instant,
    out: Option<std::fs::File>,
}

impl Log {
    fn step(&mut self, step: &str, ok: bool, extra: Value) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let mut v = json!({
            "step": step,
            "ok": ok,
            "unixMs": now as u64,
            "elapsedMs": self.t0.elapsed().as_millis() as u64,
        });
        if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        let line = v.to_string();
        println!("{line}");
        if let Some(f) = &mut self.out {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn packs_in(com_mojang: &Path) -> usize {
    ["development_behavior_packs", "development_resource_packs"]
        .iter()
        .map(|d| std::fs::read_dir(com_mojang.join(d)).map_or(0, |r| r.flatten().count()))
        .sum()
}

pub fn run(args: &[String]) -> i32 {
    let val = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1))
    };
    let mut log = Log {
        t0: Instant::now(),
        out: val("--out").and_then(|p| std::fs::File::create(p).ok()),
    };
    let base = val("--dir").map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("beacon-smoke-{}", std::process::id()))
    });
    let mojang = base.join("com.mojang");
    let project = base.join("project");
    let fail = |log: &mut Log, step: &str, why: String| {
        log.step(step, false, json!({ "error": why }));
        log.step("done", false, json!({}));
        1
    };
    if let Err(e) = std::fs::create_dir_all(&mojang) {
        return fail(&mut log, "setup", e.to_string());
    }
    // SAFETY: single-threaded here (before any thread is started by this mode).
    unsafe { std::env::set_var("BEACON_COM_MOJANG", &mojang) };

    let core = AppCore::new(beacon_exe()).first_launch();
    log.step("first_launch", true, json!({}));

    let created =
        core.init_create(json!({ "folder": project, "name": "Smoke", "agentFiles": false }));
    let summary = match created {
        Ok(s) => s,
        Err(e) => return fail(&mut log, "init_create", format!("{e:?}")),
    };
    log.step("init_create", true, json!({ "project": summary.root }));

    let report = match core.play(project, None) {
        Ok(r) => r,
        Err(e) => return fail(&mut log, "play", format!("{e:?}")),
    };
    if report.deployed_to.is_none() {
        let d = serde_json::to_string(&report.diagnostics).unwrap_or_default();
        return fail(&mut log, "play", format!("nothing deployed: {d}"));
    }
    log.step("play", true, json!({ "launchRequested": report.launched }));

    let n = packs_in(&mojang);
    if n == 0 {
        return fail(
            &mut log,
            "packs_deployed",
            format!("no dev packs in {}", mojang.display()),
        );
    }
    log.step(
        "packs_deployed",
        true,
        json!({ "packs": n, "comMojang": mojang }),
    );
    log.step("done", true, json!({ "dir": base }));
    0
}
