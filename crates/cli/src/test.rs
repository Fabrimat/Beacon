//! `beacon test` (8.5): the project's GameTests on the pinned BDS ([`beacon_engine::gametest`]).
//!
//! EULA gate (no download without it) -> dev build into a temp dir with the runner shim aliased
//! in place of `@minecraft/server-gametest` -> BDS download (consent per `--yes`) -> the dev world
//! and packs into a temp BDS, Beta APIs on -> `Server started.` -> `gametest runset` -> stop when
//! the runner is done, every test has a verdict or ran past `--test-timeout`, or at `--timeout`.

use super::{Opts, exit_code, print_diags};
use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::config::Artifact;
use beacon_engine::diag::{Code, Diagnostic, Severity, exit};
use beacon_engine::gametest::{self, Event, Status, TestOptions, TestReport};
use beacon_engine::host::{BundleRequest, BundleResult, Bundler};
use beacon_engine::{route, sarif, trust};
use beacon_jshost::{Host, Limits};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Set by the Ctrl-C handler: the run stops at its next checkpoint (BDS is told to `stop`, then
/// killed on drop), the temp dir is removed by its own drop, and the exit code is 130.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// After `stop`: time for BDS to flush its last lines and exit before it is killed.
const GRACE: Duration = Duration::from_secs(20);
/// Every test has a verdict (or ran past `--test-timeout`): wait this long for `done`.
const SETTLE: Duration = Duration::from_secs(3);

#[derive(clap::Args)]
pub struct TestArgs {
    /// Dev target to build (default: the first dev target).
    #[arg(long, value_name = "t")]
    target: Option<String>,
    /// Only the tests whose `class:name` matches this glob (`*`, `?`).
    #[arg(long, value_name = "glob")]
    filter: Option<String>,
    /// Whole-run limit in seconds (BDS download excluded).
    #[arg(long, value_name = "s", default_value_t = 300)]
    timeout: u64,
    /// Per-test limit in seconds from the test's start (the game's `maxTicks` also applies).
    #[arg(long, value_name = "s", default_value_t = 60)]
    test_timeout: u64,
    /// Accept the Minecraft EULA (https://minecraft.net/eula) for BDS, or set
    /// `BEACON_BDS_EULA=accept`; logged to `<tools>/bds-eula.log`.
    #[arg(long)]
    accept_eula: bool,
    /// Allow generators, plugins and the bundler for this run only (see `beacon trust`).
    #[arg(long)]
    trust: bool,
    /// SARIF on stdout instead of text (`--json` prints the TestReport).
    #[arg(long)]
    sarif: bool,
}

pub fn run(o: &Opts, a: &TestArgs) -> i32 {
    if o.json && a.sarif {
        eprintln!("error: --json and --sarif are exclusive");
        return exit::USAGE;
    }
    let opts = TestOptions {
        target: a.target.clone(),
        filter: a.filter.clone(),
        timeout_secs: a.timeout,
        test_timeout_secs: a.test_timeout,
        accept_eula: a.accept_eula,
    };
    let _ = ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst));
    let res = test(o, &opts, a.trust);
    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("interrupted");
        return 130;
    }
    let (report, diags) = match res {
        Ok(r) => r,
        Err(d) => {
            print_diags(&d);
            if a.sarif {
                println!("{}", serde_json::to_string(&sarif::to_sarif(&d)).unwrap());
            }
            let usage = [Code::BdsEulaRequired, Code::DownloadNeedsConfirm];
            return if d.iter().any(|d| usage.contains(&d.code)) {
                exit::USAGE
            } else if d.iter().any(|d| d.code == Code::BdsFailed) {
                exit::DIAGNOSTICS
            } else {
                exit_code(&d)
            };
        }
    };
    if o.json {
        println!("{}", serde_json::to_string(&report).unwrap());
    } else if a.sarif {
        println!(
            "{}",
            serde_json::to_string(&sarif::to_sarif(&diags)).unwrap()
        );
    } else {
        for r in &report.results {
            let m = r
                .message
                .as_deref()
                .map_or(String::new(), |m| format!(": {m}"));
            match r.status {
                Status::Passed => println!("PASS    {}", r.name),
                Status::Failed => println!("FAIL    {}{m}", r.name),
                Status::TimedOut => println!("TIMEOUT {}{m}", r.name),
            }
        }
    }
    print_diags(&diags); // stderr: never mixes with --json / --sarif
    let passed = report.complete
        && !report.results.is_empty()
        && report.results.iter().all(|r| r.status == Status::Passed)
        && !diags.iter().any(|d| d.severity == Severity::Error);
    if passed { exit::OK } else { exit::DIAGNOSTICS }
}

fn fail(code: Code, msg: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic::new(code, msg)]
}

/// The run: its report plus the SARIF diagnostics (the report's, and `BdsFailed` when the packs'
/// scripts did not load). `Err` = no report (EULA, consent, build or BDS start failure).
fn test(
    o: &Opts,
    opts: &TestOptions,
    trust_flag: bool,
) -> Result<(TestReport, Vec<Diagnostic>), Vec<Diagnostic>> {
    // EULA first: nothing is built, downloaded or written without it
    let method = beacon_bds::eula_method(
        opts.accept_eula,
        std::env::var("BEACON_BDS_EULA").ok().as_deref(),
    )
    .ok_or_else(|| {
        fail(
            Code::BdsEulaRequired,
            format!(
                "`beacon test` runs the Minecraft Bedrock Dedicated Server, which needs the \
                 Minecraft EULA ({}) accepted: pass --accept-eula or set BEACON_BDS_EULA=accept",
                beacon_bds::EULA_URL
            ),
        )
    })?;
    let cfg = route::load(&o.project)?;
    let target = match &opts.target {
        Some(t) => t.clone(),
        None => cfg
            .beacon
            .targets
            .iter()
            .find(|(_, t)| t.artifact == Artifact::Dev)
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                fail(
                    Code::ConfigField,
                    "`beacon test` needs a `dev` target (beacon.targets)",
                )
            })?,
    };
    if cfg
        .beacon
        .targets
        .get(&target)
        .is_some_and(|t| t.artifact != Artifact::Dev)
    {
        return Err(fail(
            Code::ConfigField,
            format!("target {target:?} is not a `dev` target"),
        ));
    }

    let tmp = tempfile::Builder::new()
        .prefix("beacon-test-")
        .tempdir()
        .map_err(|e| fail(Code::Internal, e.to_string()))?;
    let out = tmp.path().join("out");
    build(o, opts, &target, &out, trust_flag)?;
    checkpoint()?;

    let tools = beacon_fetch::default_dir()
        .ok_or_else(|| fail(Code::BdsFailed, "no tools dir (set BEACON_HOME)"))?;
    beacon_bds::log_eula(&tools, method)
        .map_err(|e| fail(Code::BdsFailed, format!("cannot write the EULA log: {e}")))?;
    eprintln!(
        "Minecraft EULA ({}) accepted via {method}; logged to {}",
        beacon_bds::EULA_URL,
        tools.join("bds-eula.log").display()
    );
    let confirm = if o.download {
        Confirm::Yes
    } else {
        Confirm::Tty
    };
    let zip = beacon_bds::fetch(&tools, confirm).map_err(|e| match e {
        beacon_bds::FetchError::NeedsConfirm => fail(
            Code::DownloadNeedsConfirm,
            format!(
                "BDS {} must be downloaded from minecraft.net (blake3-pinned); pass --yes or set \
                 BEACON_DOWNLOAD=yes",
                beacon_bds::VERSION
            ),
        ),
        e => fail(Code::BdsFailed, format!("BDS download: {e}")),
    })?;

    checkpoint()?;
    let srv = tmp.path().join("srv");
    let world =
        beacon_bds::install(&zip, &srv, "test", false).map_err(|e| fail(Code::BdsFailed, e))?;
    place(&out, &world).map_err(|e| fail(Code::BdsFailed, e))?;

    checkpoint()?;
    let lines = console(&srv, opts)?;
    checkpoint()?;
    let _ = std::fs::create_dir_all(o.project.join(".beacon"));
    let _ = std::fs::write(
        o.project.join(".beacon").join("test-bds.log"),
        lines.join("\n") + "\n",
    );
    if !lines.iter().any(|l| l.contains("Server started.")) {
        return Err(fail(
            Code::BdsFailed,
            format!("BDS exited before `Server started.`:\n{}", tail(&lines, 20)),
        ));
    }
    let (results, complete) = gametest::parse_console(&lines);
    let report = TestReport {
        schema: gametest::SCHEMA,
        bds_version: beacon_bds::VERSION.into(),
        results,
        complete,
    };
    let mut diags = report.diagnostics();
    // a pack that requests a module version this BDS lacks never registers anything
    let script_errors: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains(" ERROR] [Scripting]"))
        .collect();
    if report.results.is_empty() && !script_errors.is_empty() {
        let errs: Vec<&str> = script_errors.iter().take(10).map(|s| s.as_str()).collect();
        diags.push(Diagnostic::new(
            Code::BdsFailed,
            format!("the packs' scripts did not load:\n{}", errs.join("\n")),
        ));
    }
    Ok((report, diags))
}

/// `Err` (no diagnostics; `run` reports the interrupt) once Ctrl-C was pressed.
fn checkpoint() -> Result<(), Vec<Diagnostic>> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        Err(Vec::new())
    } else {
        Ok(())
    }
}

fn tail(lines: &[String], n: usize) -> String {
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// The dev target into `out`, its scripts bundled with the runner shim.
fn build(
    o: &Opts,
    opts: &TestOptions,
    target: &str,
    out: &Path,
    trust_flag: bool,
) -> Result<(), Vec<Diagnostic>> {
    let trusted = trust_flag
        || trust::is_trusted(
            &o.project,
            &beacon_engine::plugin::project_trust_key(&o.project),
        );
    // absolute: esbuild resolves a relative alias as a package name
    let shim = std::path::absolute(o.project.join(".beacon").join("gametest-shim.js"))
        .map_err(|e| fail(Code::Internal, e.to_string()))?;
    std::fs::create_dir_all(shim.parent().unwrap())
        .and_then(|()| std::fs::write(&shim, gametest::shim(opts.filter.as_deref())))
        .map_err(|e| fail(Code::Internal, format!("{}: {e}", shim.display())))?;
    let confirm = if o.download {
        Confirm::Yes
    } else {
        Confirm::Tty
    };
    let bundler = trusted.then(|| {
        Box::new(Shimmed(
            LazyEsbuild::new(&o.project, confirm).with_alias(gametest::GAMETEST_MODULE, &shim),
        )) as Box<dyn Bundler>
    });
    let r = Session::new(
        &o.project,
        BuildOptions {
            target: Some(target.to_owned()),
            all: false,
            out: Some(out.to_path_buf()),
            frozen: false,
            trusted,
            release: None,
        },
        Box::new(Host::new(Limits::default())),
        bundler,
    )
    .build();
    if exit_code(&r.diagnostics) != exit::OK {
        return Err(r.diagnostics);
    }
    if !o.json {
        print_diags(&r.diagnostics);
    }
    Ok(())
}

/// Bundles with the shim: its placeholder import of the real module stays external and is
/// renamed back afterwards.
struct Shimmed(LazyEsbuild);

impl Bundler for Shimmed {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        let mut ext = req.externals.to_vec();
        ext.extend([gametest::SHIM_REAL.into(), "@minecraft/server".into()]);
        let mut r = self.0.bundle(&BundleRequest {
            externals: &ext,
            ..*req
        });
        let (from, to) = (
            format!("\"{}\"", gametest::SHIM_REAL),
            format!("\"{}\"", gametest::GAMETEST_MODULE),
        );
        r.code = r.code.map(|c| c.replace(&from, &to));
        r
    }

    fn version(&self) -> Option<String> {
        self.0.version()
    }
}

/// Moves the dev build's world to `world` and its packs into the world's own pack folders
/// (`world_*_packs.json` already lists them), then turns Beta APIs on.
fn place(out: &Path, world: &Path) -> Result<(), String> {
    let dirs = |d: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    let [src] = &dirs(&out.join("minecraftWorlds"))[..] else {
        return Err("the dev build wrote no world (`\"world\": false`?)".into());
    };
    std::fs::remove_dir(world).map_err(|e| e.to_string())?;
    std::fs::rename(src, world).map_err(|e| format!("{}: {e}", src.display()))?;
    for (dev, sub) in [
        ("development_behavior_packs", "behavior_packs"),
        ("development_resource_packs", "resource_packs"),
    ] {
        for p in dirs(&out.join(dev)) {
            let to = world.join(sub).join(p.file_name().unwrap());
            std::fs::create_dir_all(world.join(sub)).map_err(|e| e.to_string())?;
            std::fs::rename(&p, &to).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let ld = world.join("level.dat");
    let level = std::fs::read(&ld).map_err(|e| format!("{}: {e}", ld.display()))?;
    std::fs::write(&ld, gametest::beta_apis(&level)?).map_err(|e| e.to_string())
}

/// Every registered test has a verdict or ran past `per`, counted from its `Start` line, or from
/// `Server started.` / the run command (`up_at`) when it never started.
fn all_settled(
    registered: &[String],
    verdict: &HashSet<String>,
    started: &HashMap<String, Instant>,
    up_at: Option<Instant>,
    per: Duration,
) -> bool {
    registered.iter().all(|n| {
        verdict.contains(n)
            || started
                .get(n)
                .copied()
                .or(up_at)
                .is_some_and(|t| t.elapsed() > per)
    })
}

/// Runs BDS and drives the tests; returns the whole console.
fn console(srv: &Path, opts: &TestOptions) -> Result<Vec<String>, Vec<Diagnostic>> {
    let (limit, per) = (
        Duration::from_secs(opts.timeout_secs),
        Duration::from_secs(opts.test_timeout_secs),
    );
    let t0 = Instant::now();
    let mut registered: Vec<String> = Vec::new();
    let mut started: HashMap<String, Instant> = HashMap::new();
    let mut verdict: HashSet<String> = HashSet::new();
    let (mut up, mut done, mut stopping) = (false, false, false);
    let mut settled_at: Option<Instant> = None;
    let mut up_at: Option<Instant> = None;
    let run = beacon_bds::run(srv, limit + GRACE, |l, stdin| {
        if let Some(l) = l {
            match gametest::parse_line(l) {
                Some(Event::Registered { name }) => registered.push(name),
                Some(Event::Start { name }) => {
                    started.entry(name).or_insert_with(Instant::now);
                }
                Some(Event::Pass { name } | Event::Fail { name, .. }) => {
                    verdict.insert(name);
                }
                Some(Event::Done) => done = true,
                None => {}
            }
            if !up && l.contains("Server started.") {
                up = true;
                up_at = Some(Instant::now());
                if !registered.is_empty() {
                    let _ = writeln!(stdin, "{}", gametest::RUN_COMMAND);
                }
            }
        }
        if stopping {
            return;
        }
        let settled = up && all_settled(&registered, &verdict, &started, up_at, per);
        // The shim's `done` prints a tick after the last verdict: give it a beat before `stop`.
        if settled && settled_at.is_none() {
            settled_at = Some(Instant::now());
        }
        if INTERRUPTED.load(Ordering::SeqCst)
            || done
            || settled_at.is_some_and(|t| t.elapsed() > SETTLE)
            || t0.elapsed() > limit
        {
            stopping = true;
            let _ = writeln!(stdin, "stop");
        }
    })
    .map_err(|e| fail(Code::BdsFailed, e))?;
    Ok(run.lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_but_never_started_times_out_from_server_up() {
        let per = Duration::from_secs(10);
        let old = Instant::now().checked_sub(Duration::from_secs(60)).unwrap();
        let reg = vec!["a:b".to_owned()];
        let (v, st) = (HashSet::new(), HashMap::new());
        assert!(!all_settled(&reg, &v, &st, Some(Instant::now()), per));
        assert!(all_settled(&reg, &v, &st, Some(old), per));
        assert!(!all_settled(&reg, &v, &st, None, per));
        let v2: HashSet<String> = ["a:b".to_owned()].into();
        assert!(all_settled(&reg, &v2, &st, None, per));
        // a started test counts from its own start, not from server-up
        let st2 = HashMap::from([("a:b".to_owned(), Instant::now())]);
        assert!(!all_settled(&reg, &v, &st2, Some(old), per));
    }
}
