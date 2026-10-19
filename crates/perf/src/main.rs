//! Beacon perf harness. Measures every phase-1 metric from ARCHITECTURE section 12.
//!
//! ```text
//! cargo perf run   --beacon <exe> --project <dir> [--runs 50] [--idle-secs 30] [--json out.json]
//!                  [--enforce] [--phase 1]
//! cargo perf app   --app <installed app exe> [--installer <setup.exe>] [--idle-secs 90] [--json out.json]
//!                  [--enforce]      (phase 5: app_idle_rss_mb, installer_size_mb; see `app.rs`)
//! cargo perf local <path-to-real-project> [same flags except --project; --beacon defaults to
//!                  target/release/beacon[.exe]]
//! ```
//!
//! RSS is summed over the process tree. Windows: private working set (pages in the working set
//! that are not `Shared`, via `K32QueryWorkingSet`). Elsewhere (Linux/macOS): sysinfo resident
//! set. Peak is the max of tree samples taken while the build runs (a very short build can
//! finish between samples). Both `run` and `local` work on a temp copy of the project; the
//! original is never written. Later-phase metrics are added to budgets.json when measurable.
//! Drives the real `beacon` (always `--trust`; `BEACON_ESBUILD` is inherited). Cold build = no
//! `.beacon/` and no `packs/`; the OS file cache stays warm. Watch latency = byte-changing edit
//! of an `*.item.json` / `scripts/*.ts` -> timestamp of the next `built` event of `watch --json`.

mod app;

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, process, sync::mpsc, thread};
use sysinfo::{Pid, ProcessesToUpdate, System};

const BUDGETS: &str = include_str!("../../../perf/budgets.json");

#[derive(Deserialize)]
struct Budget {
    id: String,
    label: String,
    budget: f64,
    unit: String,
    phase: u32,
    tolerance_pct: f64,
}

#[derive(Serialize)]
struct Row {
    id: String,
    label: String,
    value: f64,
    budget: f64,
    unit: String,
    phase: u32,
    enforced: bool,
    pass: bool,
}

/// Nearest-rank percentile (`p` in 0..=100) of unsorted samples.
fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let rank = ((p / 100.0) * s.len() as f64).ceil() as usize;
    s[rank.clamp(1, s.len()) - 1]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// A spawned process that is killed and reaped on drop (also on panic/error paths).
struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn tree_rss_mb(sys: &mut System, root: u32) -> f64 {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut tree = vec![Pid::from_u32(root)];
    let mut total = 0;
    let mut i = 0;
    while i < tree.len() {
        let pid = tree[i];
        i += 1;
        total += proc_mem(pid.as_u32(), sys);
        tree.extend(
            sys.processes()
                .iter()
                .filter(|(_, p)| p.parent() == Some(pid))
                .map(|(k, _)| *k),
        );
    }
    total as f64 / 1048576.0
}

/// Private working set of one process in bytes (pages not marked Shared); 0 if inaccessible.
#[cfg(windows)]
fn proc_mem(pid: u32, _sys: &System) -> u64 {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::K32QueryWorkingSet;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };
    // SAFETY: plain Win32 calls; the buffer is a Vec<usize> (NumberOfEntries, then one usize
    // PSAPI_WORKING_SET_BLOCK per page) whose byte length is passed to the API.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            return 0;
        }
        let mut buf = vec![0usize; 1 << 16];
        let mut pages = 0;
        // On ERROR_BAD_LENGTH buf[0] still holds the needed entry count: grow and retry.
        for _ in 0..4 {
            if K32QueryWorkingSet(h, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32) != 0 {
                // Flags bit 8 = Shared.
                pages = buf[1..=buf[0]].iter().filter(|b| *b & 0x100 == 0).count() as u64;
                break;
            }
            let need = buf[0] + 1024;
            buf = vec![0usize; need + 1];
        }
        CloseHandle(h);
        pages * 4096
    }
}
#[cfg(not(windows))]
fn proc_mem(pid: u32, sys: &System) -> u64 {
    sys.process(Pid::from_u32(pid)).map_or(0, |p| p.memory())
}

struct Ctx<'a> {
    beacon: &'a Path,
    project: &'a Path,
}

impl Ctx<'_> {
    /// `beacon <sub> --trust --project <copy>`; `BEACON_ESBUILD` is inherited from the env.
    fn cmd(&self, sub: &str) -> Command {
        let mut c = Command::new(self.beacon);
        c.args([sub, "--trust", "--project"]).arg(self.project);
        c.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        c
    }

    /// Cold = no `.beacon/` cache and no `packs/` output (the OS file cache stays warm).
    fn clean(&self) {
        for d in [".beacon", "packs"] {
            let _ = fs::remove_dir_all(self.project.join(d));
        }
    }

    /// Run a build; returns (wall ms, peak RSS MB sampled over the tree).
    fn build(&self, sample: bool) -> Result<(f64, f64), String> {
        let t = Instant::now();
        let mut g = Guard(self.cmd("build").spawn().map_err(|e| e.to_string())?);
        let mut sys = System::new();
        let mut peak = 0f64;
        let status = loop {
            if let Some(s) = g.0.try_wait().map_err(|e| e.to_string())? {
                break s;
            }
            if sample {
                // Whole tree every poll, so a short-lived esbuild child is not missed.
                peak = peak.max(tree_rss_mb(&mut sys, g.0.id()));
                thread::sleep(Duration::from_millis(5));
            }
        };
        let wall = ms(t.elapsed());
        if !status.success() {
            return Err(format!("beacon build failed: {status}"));
        }
        Ok((wall, peak))
    }

    /// Start `beacon watch --json`, wait for the `ready` event. Events arrive timestamped.
    fn watch(&self) -> Result<(Guard, mpsc::Receiver<(Instant, String)>), String> {
        let mut c = self.cmd("watch");
        c.arg("--json").stdout(Stdio::piped());
        let mut g = Guard(c.spawn().map_err(|e| e.to_string())?);
        let out = g.0.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        // Drains stdout for the child's lifetime (it can never block on a full pipe); only
        // JSON-object lines with an `event` field are forwarded, anything else is ignored.
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                let at = Instant::now();
                if let Some(ev) = event_of(&line) {
                    let _ = tx.send((at, ev));
                }
            }
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok((_, e)) if e == "ready" => return Ok((g, rx)),
                Ok(_) => {}
                Err(_) => return Err("beacon watch never emitted a `ready` event".into()),
            }
        }
    }
}

/// The `event` name of a `BuildEvent` JSON line; `None` for any other output.
fn event_of(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    Some(v.get("event")?.as_str()?.to_string())
}

fn find_src(dir: &Path, want: &dyn Fn(&Path) -> bool) -> Option<PathBuf> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    if let Some(f) = entries.iter().find(|p| p.is_file() && want(p)) {
        return Some(f.clone());
    }
    entries
        .iter()
        .filter(|p| p.is_dir())
        .find_map(|p| find_src(p, want))
}

/// Puts the edited file back on every exit path.
struct Restore(PathBuf, String);
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, &self.1);
    }
}

fn name_ends(p: &Path, suffix: &str) -> bool {
    p.file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(suffix))
}

/// Edit `file` `runs` times under a running watcher; latency = edit -> next `built` event.
fn watch_latency(ctx: &Ctx, ext: &str, runs: usize) -> Result<Vec<f64>, String> {
    let src = ctx.project.join("src");
    let file = if ext == "json" {
        find_src(&src, &|p| name_ends(p, ".item.json"))
    } else {
        find_src(&src, &|p| {
            name_ends(p, ".ts")
                && !name_ends(p, ".gen.ts")
                && p.parent()
                    .is_some_and(|d| d.file_name().is_some_and(|n| n == "scripts"))
        })
    }
    .ok_or(format!("no matching .{ext} file under {}", src.display()))?;
    let orig = fs::read_to_string(&file).map_err(|e| e.to_string())?;
    let _restore = Restore(file.clone(), orig.clone());
    let (_w, rx) = ctx.watch()?;
    let mut lat = Vec::new();
    for i in 0..runs {
        // Every edit changes the bytes and keeps the file valid.
        let body = if ext == "json" {
            format!("{orig}{}", " ".repeat(i + 1))
        } else {
            format!(
                "{orig}
export const beaconPerf{i} = {i};
"
            )
        };
        thread::sleep(Duration::from_millis(50));
        while rx.try_recv().is_ok() {} // trailing events of the previous edit
        fs::write(&file, &body).map_err(|e| e.to_string())?;
        let t = Instant::now();
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok((at, e)) if e == "built" => {
                    lat.push(ms(at.saturating_duration_since(t)));
                    break;
                }
                Ok(_) => {}
                Err(_) => return Err(format!("no `built` event after editing {}", file.display())),
            }
        }
    }
    Ok(lat)
}

/// Idle RSS of `beacon serve --stdio --mcp --trust`: a trusted MCP `check` populates the
/// cached session, then `idle` passes. Daemon (detached, found by `serve --status` pid) tree
/// plus the shim tree. `BEACON_HOME` isolates discovery; the daemon is stopped with `--force`.
fn serve_idle_rss(ctx: &Ctx, idle: Duration) -> Result<f64, String> {
    use std::io::Write;
    let home = env::temp_dir().join(format!("beacon-perf-home-{}", process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let serve = |args: &[&str]| {
        let mut c = Command::new(ctx.beacon);
        c.arg("serve").args(args).arg("--project").arg(ctx.project);
        c.env("BEACON_HOME", &home).env("BEACON_SERVE_IDLE", "3600");
        c.stdin(Stdio::null()).stderr(Stdio::inherit());
        c
    };
    let r = (|| {
        let mut c = serve(&["--stdio", "--mcp", "--trust"]);
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut shim = Guard(c.spawn().map_err(|e| e.to_string())?);
        let mut stdin = shim.0.stdin.take().unwrap();
        let mut out = BufReader::new(shim.0.stdout.take().unwrap());
        for req in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"perf","version":"0"}}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"check","arguments":{}}}"#,
        ] {
            writeln!(stdin, "{req}").map_err(|e| e.to_string())?;
            let mut line = String::new();
            out.read_line(&mut line).map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                return Err("serve shim closed without answering".into());
            }
            if line.contains("\"error\"") || line.contains("\"isError\":true") {
                return Err(format!("serve request failed: {}", line.trim()));
            }
        }
        thread::sleep(idle);
        let st = serve(&["--status"])
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| e.to_string())?;
        let v: serde_json::Value =
            serde_json::from_slice(&st.stdout).map_err(|e| format!("serve --status: {e}"))?;
        let pid = v["pid"].as_u64().ok_or("serve --status: no pid")? as u32;
        let mut sys = System::new();
        Ok(tree_rss_mb(&mut sys, pid) + tree_rss_mb(&mut sys, shim.0.id()))
    })();
    let _ = serve(&["--stop", "--force"]).stdout(Stdio::null()).status();
    let _ = fs::remove_dir_all(&home);
    r
}

struct Opts {
    beacon: PathBuf,
    runs: usize,
    idle: Duration,
    json: Option<PathBuf>,
    enforce: bool,
    phase: u32,
}

fn measure(o: &Opts, project: &Path) -> Result<Vec<(&'static str, f64)>, String> {
    let ctx = Ctx {
        beacon: &o.beacon,
        project,
    };
    let mut m = Vec::new();

    let (mut cold, mut warm) = (vec![], vec![]);
    for _ in 0..o.runs {
        ctx.clean();
        let (t, _) = ctx.build(false)?;
        cold.push(t);
        let (t, _) = ctx.build(false)?;
        warm.push(t);
    }
    // Peak RSS: sampling slows the loop, so it gets its own cold build (not timed).
    ctx.clean();
    let peak = ctx.build(true)?.1;
    m.push(("build_cold_ms", percentile(&cold, 50.0)));
    m.push(("build_warm_ms", percentile(&warm, 50.0)));
    m.push(("peak_rss_build_mb", peak));

    m.push((
        "watch_json_p95_ms",
        percentile(&watch_latency(&ctx, "json", o.runs)?, 95.0),
    ));
    m.push((
        "watch_ts_p95_ms",
        percentile(&watch_latency(&ctx, "ts", o.runs)?, 95.0),
    ));

    // Idle: wait for `ready` (initial build done), settle, then sample the whole tree.
    let (w, _rx) = ctx.watch()?;
    thread::sleep(o.idle);
    m.push((
        "idle_rss_watch_mb",
        tree_rss_mb(&mut System::new(), w.0.id()),
    ));
    drop(w);

    if o.phase >= 4 {
        m.push(("idle_rss_serve_mb", serve_idle_rss(&ctx, o.idle)?));
    }

    let mut start = vec![];
    for _ in 0..o.runs.max(5) {
        let t = Instant::now();
        let r = Command::new(&o.beacon)
            .arg("--version")
            .output()
            .map_err(|e| e.to_string())?;
        if r.stdout.is_empty() {
            return Err("beacon --version printed nothing".into());
        }
        start.push(ms(t.elapsed()));
    }
    m.push(("cli_start_p50_ms", percentile(&start, 50.0)));

    let size = fs::metadata(&o.beacon).map_err(|e| e.to_string())?.len();
    m.push(("binary_size_mb", size as f64 / 1048576.0));
    if let Some(e) = env::var_os("BEACON_ESBUILD").and_then(|e| fs::metadata(e).ok()) {
        println!(
            "note: esbuild sidecar {:.1} MB (separate from binary_size_mb)",
            e.len() as f64 / 1048576.0
        );
    }
    Ok(m)
}

fn report(o: &Opts, measured: Vec<(&'static str, f64)>) -> Result<bool, String> {
    let budgets: Vec<Budget> = serde_json::from_str(BUDGETS).map_err(|e| e.to_string())?;
    let mut rows = vec![];
    for b in budgets {
        let value = measured.iter().find(|(id, _)| *id == b.id).map(|m| m.1);
        let Some(value) = value else { continue };
        let enforced = b.phase <= o.phase;
        rows.push(Row {
            pass: value <= b.budget * (1.0 + b.tolerance_pct / 100.0),
            id: b.id,
            label: b.label,
            value,
            budget: b.budget,
            unit: b.unit,
            phase: b.phase,
            enforced,
        });
    }
    println!(
        "{:<52} {:>10} {:>10}  {:>5}  status",
        "metric", "value", "budget", "phase"
    );
    for r in &rows {
        let status = match (r.pass, r.enforced) {
            (true, _) => "ok",
            (false, true) => "FAIL",
            (false, false) => "over (not enforced)",
        };
        println!(
            "{:<52} {:>7.1} {:<2} {:>7.1} {:<2}  {:>5}  {status}",
            r.label, r.value, r.unit, r.budget, r.unit, r.phase
        );
    }
    if let Some(p) = &o.json {
        let s = serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?;
        fs::write(p, s).map_err(|e| e.to_string())?;
    }
    Ok(!rows.iter().any(|r| r.enforced && !r.pass))
}

/// Full harness run against a temp copy of `project` in `work`; the original is never written.
/// Returns whether all enforced metrics pass.
fn run_harness(o: &Opts, project: &Path, work: &Path) -> Result<bool, String> {
    let copy = work.join("project");
    let r = copy_tree(project, &copy)
        .map_err(|e| format!("copy project: {e}"))
        .and_then(|()| measure(o, &copy))
        .and_then(|m| report(o, m));
    let _ = fs::remove_dir_all(work);
    Ok(r? || !o.enforce)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (p, t) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy_tree(&p, &t)?;
        } else {
            fs::copy(&p, &t)?;
        }
    }
    Ok(())
}

fn real_main() -> Result<bool, String> {
    let args: Vec<String> = env::args().skip(1).collect();
    let val = |n: &str| {
        args.iter()
            .position(|a| a == n)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let num = |n: &str, d: u64| {
        val(n).map_or(Ok(d), |v| v.parse::<u64>().map_err(|e| format!("{n}: {e}")))
    };
    let o = Opts {
        beacon: val("--beacon").map(PathBuf::from).unwrap_or_else(|| {
            let exe = if cfg!(windows) {
                "beacon.exe"
            } else {
                "beacon"
            };
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/release")
                .join(exe)
        }),
        runs: num("--runs", 50)? as usize,
        idle: Duration::from_secs(num(
            "--idle-secs",
            if args.first().is_some_and(|a| a == "app") {
                90
            } else {
                30
            },
        )?),
        json: val("--json").map(PathBuf::from),
        enforce: args.iter().any(|a| a == "--enforce"),
        phase: num("--phase", 1)? as u32,
    };
    if o.runs == 0 {
        return Err("--runs must be >= 1".into());
    }
    let tmp = env::temp_dir().join(format!("beacon-perf-{}", process::id()));
    if args.first().map(String::as_str) == Some("app") {
        let app = val("--app").ok_or("--app required")?;
        return app::run(
            &o,
            Path::new(&app),
            val("--installer").as_deref().map(Path::new),
        );
    }
    match args.first().map(String::as_str) {
        Some(cmd @ ("run" | "local")) => {
            let src = if cmd == "run" {
                val("--project").ok_or("--project required")?
            } else {
                args.get(1)
                    .filter(|a| !a.starts_with("--"))
                    .ok_or("usage: local <project>")?
                    .clone()
            };
            let src = fs::canonicalize(&src).map_err(|e| format!("{src}: {e}"))?;
            if let Some(j) = &o.json {
                // Reject up front: a report written into the project would be a write to it.
                let name = j.file_name().ok_or("--json: not a file path")?;
                let dir = j.parent().filter(|d| !d.as_os_str().is_empty());
                let dir = fs::canonicalize(dir.unwrap_or(Path::new(".")))
                    .map_err(|e| format!("--json: {e}"))?;
                if dir.join(name).starts_with(&src) {
                    return Err("--json must not be inside the project".into());
                }
            }
            let r = run_harness(&o, &src, &tmp);
            if cmd == "local" {
                println!("migrate / equivalence checks: skipped (phase 3)");
            }
            r
        }
        _ => Err("usage: perf <run|local> ...".into()),
    }
}

fn main() {
    match real_main() {
        Ok(true) => {}
        Ok(false) => process::exit(1),
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let v: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        assert_eq!(percentile(&v, 50.0), 50.0);
        assert_eq!(percentile(&v, 95.0), 95.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 100.0), 3.0);
    }

    #[test]
    fn end_to_end_against_minimal_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if env::var_os("BEACON_ESBUILD").is_none() {
            let sub = if cfg!(windows) {
                "win32-x64/esbuild.exe"
            } else {
                "darwin-arm64/bin/esbuild"
            };
            let dir = root.join("target/esbuild-test");
            let exe = dir.join("node_modules/@esbuild").join(sub);
            if !exe.is_file() {
                // Same install as crates/bundler/tests.
                let mut c = if cfg!(windows) {
                    let mut c = Command::new("cmd");
                    c.args(["/C", "npm"]);
                    c
                } else {
                    Command::new("npm")
                };
                let _ = c
                    .args(["i", "--no-audit", "--no-fund", "--prefix"])
                    .arg(&dir)
                    .arg("esbuild@0.28.2")
                    .status();
            }
            if !exe.is_file() {
                eprintln!("SKIP SKIP SKIP: no esbuild (set BEACON_ESBUILD); e2e test NOT run");
                return;
            }
            // SAFETY: no other test in this crate reads or writes the environment concurrently.
            unsafe { env::set_var("BEACON_ESBUILD", exe.canonicalize().unwrap()) };
        }
        let cargo = env::var("CARGO").unwrap_or("cargo".into());
        let st = Command::new(cargo)
            .args(["build", "-p", "beacon-cli"])
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(st.success());
        let exe = if cfg!(windows) {
            "beacon.exe"
        } else {
            "beacon"
        };
        let tmp = env::temp_dir().join(format!("beacon-perf-test-{}", process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let project = tmp.join("project");
        copy_tree(&root.join("fixtures/minimal"), &project).unwrap();
        let json = tmp.join("report.json");
        let o = Opts {
            beacon: root.join("target/debug").join(exe),
            runs: 3,
            idle: Duration::from_millis(200),
            json: Some(json.clone()),
            enforce: false,
            phase: 1,
        };
        run_harness(&o, &project, &tmp.join("work")).unwrap();
        let rows: Vec<serde_json::Value> =
            serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
        let budgets: Vec<Budget> = serde_json::from_str(BUDGETS).unwrap();
        for b in budgets.iter().filter(|b| b.phase == 1) {
            let row = rows
                .iter()
                .find(|r| r["id"] == b.id.as_str())
                .unwrap_or_else(|| panic!("missing {}", b.id));
            assert!(row["value"].is_number(), "{} not numeric", b.id);
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn event_lines_only() {
        assert_eq!(
            event_of(r#"{"event":"built","ms":1}"#).as_deref(),
            Some("built")
        );
        assert_eq!(event_of("wrote x"), None);
        assert_eq!(event_of(r#"{"a":1}"#), None);
    }
}
