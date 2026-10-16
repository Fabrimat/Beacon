//! `beacon` CLI (6.2). Non-interactive unless a command says otherwise (`trust` prompts on a TTY).
//!
//! The build pipeline is workstream B1; until then `run_build` / `run_watch` are phase-0
//! copy placeholders behind `// B1:` seams.

use beacon_engine::config::{CONFIG_FILE, STATE_DIR};
use beacon_engine::diag::{Code, Diagnostic, Severity, exit};
use beacon_engine::event::{BuildEvent, BuildReport};
use beacon_engine::{sarif, trust};
use clap::{CommandFactory, Parser, Subcommand};
use notify::{EventKind, RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::{fs, process, sync::mpsc, time::Instant};

#[derive(Parser)]
#[command(
    name = "beacon",
    version,
    about = "Beacon: Minecraft add-on build tool"
)]
struct Cli {
    /// Machine-readable output (`--help --json` describes the CLI as JSON).
    #[arg(long, global = true)]
    json: bool,
    /// Project directory (default: current directory).
    #[arg(long, global = true, value_name = "dir")]
    project: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

// B1: target/all/frozen are consumed by the real pipeline.
#[allow(dead_code)]
#[derive(clap::Args, Clone)]
struct BuildArgs {
    /// Build one target from `beacon.targets`.
    #[arg(long, value_name = "t", conflicts_with = "all")]
    target: Option<String>,
    /// Build every target.
    #[arg(long)]
    all: bool,
    /// Output directory.
    #[arg(long, value_name = "dir")]
    out: Option<PathBuf>,
    /// Allow generators and plugins for this run only (does not persist; see `beacon trust`).
    #[arg(long)]
    trust: bool,
    /// Fail if `beacon.lock.json` would change.
    #[arg(long)]
    frozen: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build the project once.
    Build(BuildArgs),
    /// Build, then rebuild on every change.
    Watch(BuildArgs),
    /// Validate the project without running any JS (restricted mode).
    Check {
        /// Accepted for symmetry with `build`; check never runs JS.
        #[arg(long)]
        trust: bool,
    },
    /// Explain a diagnostic code, e.g. `BCN0011`.
    Explain { code: String },
    /// Remember that you trust this project's generators and plugins.
    Trust {
        /// Skip the confirmation prompt (required without a TTY).
        #[arg(long)]
        yes: bool,
    },
}

/// `beacon --help --json`: the clap command tree, versioned.
fn describe(c: &clap::Command) -> Value {
    let args = c.get_arguments().filter(|a| a.get_id() != "help").map(|a| {
        json!({
            "id": a.get_id().as_str(),
            "long": a.get_long(),
            "short": a.get_short().map(String::from),
            "help": a.get_help().map(|h| h.to_string()),
            "positional": a.is_positional(),
            "required": a.is_required_set(),
            "global": a.is_global_set(),
            "takesValue": a.get_action().takes_values(),
            "default": a.get_default_values().iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),
        })
    });
    json!({
        "name": c.get_name(),
        "about": c.get_about().map(|h| h.to_string()),
        "args": args.collect::<Vec<_>>(),
        "commands": c.get_subcommands().filter(|s| s.get_name() != "help").map(describe).collect::<Vec<_>>(),
    })
}

struct Opts {
    project: PathBuf,
    out: PathBuf,
    json: bool,
}

/// What the trust gate needs: raw plugin list and whether any code would run.
struct Found {
    plugins: Vec<Value>,
    has_code: bool,
}

fn has_gen_ts(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    // file_type() does not follow symlinks/junctions, so they are neither walked nor matched.
    rd.flatten().any(|e| {
        let Ok(ft) = e.file_type() else {
            return false;
        };
        let name = e.file_name();
        let name = name.to_string_lossy();
        if ft.is_dir() {
            name != "node_modules" && has_gen_ts(&e.path())
        } else {
            ft.is_file() && name.ends_with(".gen.ts")
        }
    })
}

/// Reads `beacon.plugins` / `beacon.sources` straight from `config.json` (validation is the
/// engine's job; a missing or broken config just means "no code" here).
fn code_in(project: &Path) -> Found {
    let cfg: Value = fs::read(project.join(CONFIG_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let plugins = cfg["beacon"]["plugins"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let sources: Vec<String> = cfg["beacon"]["sources"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| vec!["./src".into()]);
    let has_code = !plugins.is_empty() || sources.iter().any(|s| has_gen_ts(&project.join(s)));
    Found { plugins, has_code }
}

fn untrusted(project: &Path, allow: bool) -> Option<Diagnostic> {
    let c = code_in(project);
    (c.has_code && !allow && !trust::is_trusted(project, &c.plugins)).then(|| {
        Diagnostic::new(
            Code::Untrusted,
            "project has generators or plugins and is not trusted; run `beacon trust` or pass --trust",
        )
    })
}

fn internal(e: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::new(Code::Internal, e.to_string())
}

fn exit_code(diags: &[Diagnostic]) -> i32 {
    let has = |c: Code| diags.iter().any(|d| d.code == c);
    if has(Code::Untrusted) {
        exit::UNTRUSTED
    } else if has(Code::Internal) {
        exit::INTERNAL
    } else if diags.iter().any(|d| d.severity == Severity::Error) {
        exit::DIAGNOSTICS
    } else {
        exit::OK
    }
}

// ---- B1: placeholder pipeline (phase-0 copy of <project>/src to <out>) ----

fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let ft = e.file_type()?;
        if ft.is_dir() {
            walk(&e.path(), files)?;
        } else if ft.is_file() {
            files.push(e.path()); // symlinks and junctions are skipped
        }
    }
    Ok(())
}

/// Copy `src` to `dst` unless dst has the same size and is not older. True when written.
fn copy_file(src: &Path, dst: &Path) -> io::Result<bool> {
    let (s, d) = (fs::metadata(src)?, fs::metadata(dst));
    if let Ok(d) = d
        && d.len() == s.len()
        && d.modified()? >= s.modified()?
    {
        return Ok(false);
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, dst)?;
    Ok(true)
}

/// B1: replace with the real pipeline (config -> index -> route -> emit).
fn run_build(o: &Opts) -> (BuildReport, Vec<Diagnostic>) {
    let t = Instant::now();
    let src = o.project.join("src");
    let mut diags = Vec::new();
    let mut emitted = 0;
    let mut files = Vec::new();
    match walk(&src, &mut files) {
        Ok(()) => {
            for f in files {
                match copy_file(&f, &o.out.join(f.strip_prefix(&src).unwrap())) {
                    Ok(w) => emitted += w as usize,
                    Err(e) => diags.push(internal(format!("{}: {e}", f.display()))),
                }
            }
        }
        Err(e) => diags.push(internal(format!("{}: {e}", src.display()))),
    }
    let report = BuildReport {
        ms: t.elapsed().as_secs_f64() * 1000.0,
        emitted,
        artifacts: vec![],
        sarif: sarif::to_sarif(&diags),
    };
    (report, diags)
}

/// B1: replace with incremental rebuilds. Calls `on_build` for the initial build (`ready`) and
/// after every rebuild that wrote something (`built`).
fn run_watch(
    o: &Opts,
    mut on_build: impl FnMut(&str, &BuildReport, &[Diagnostic]),
) -> notify::Result<()> {
    let src = fs::canonicalize(o.project.join("src"))?;
    let (report, diags) = run_build(o);
    let (tx, rx) = mpsc::channel();
    let mut w = notify::recommended_watcher(tx)?;
    w.watch(&src, RecursiveMode::Recursive)?;
    on_build("ready", &report, &diags);
    for ev in rx {
        let ev = ev?;
        if !matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_)) {
            continue;
        }
        let t = Instant::now();
        let mut emitted = 0;
        let mut diags = Vec::new();
        for p in ev.paths.iter().filter(|p| p.is_file()) {
            let dst = o.out.join(p.strip_prefix(&src).unwrap());
            // Retry once: the writer may still hold the file.
            let r = copy_file(p, &dst).or_else(|_| {
                std::thread::sleep(std::time::Duration::from_millis(5));
                copy_file(p, &dst)
            });
            match r {
                Ok(w) => emitted += w as usize,
                Err(e) => diags.push(internal(format!("{}: {e}", p.display()))),
            }
        }
        if emitted > 0 || !diags.is_empty() {
            let report = BuildReport {
                ms: t.elapsed().as_secs_f64() * 1000.0,
                emitted,
                artifacts: vec![],
                sarif: sarif::to_sarif(&diags),
            };
            on_build("built", &report, &diags);
        }
    }
    Ok(())
}

// ---- commands ----

fn print_diags(diags: &[Diagnostic]) {
    eprint!("{}", sarif::to_text(diags));
}

fn build(o: &Opts, allow: bool) -> i32 {
    if let Some(d) = untrusted(&o.project, allow) {
        return refuse(o, vec![d]);
    }
    let (report, diags) = run_build(o);
    if o.json {
        println!("{}", serde_json::to_string(&report).unwrap());
    } else {
        print_diags(&diags);
        println!("built in {:.0} ms ({} files)", report.ms, report.emitted);
    }
    exit_code(&diags)
}

fn refuse(o: &Opts, diags: Vec<Diagnostic>) -> i32 {
    if o.json {
        let r = BuildReport {
            ms: 0.0,
            emitted: 0,
            artifacts: vec![],
            sarif: sarif::to_sarif(&diags),
        };
        println!("{}", serde_json::to_string(&r).unwrap());
    } else {
        print_diags(&diags);
    }
    exit_code(&diags)
}

fn watch(o: &Opts, allow: bool) -> i32 {
    if let Some(d) = untrusted(&o.project, allow) {
        return refuse(o, vec![d]);
    }
    let mut generation = 0;
    let r = run_watch(o, |event, rep, diags| {
        generation += 1;
        if o.json {
            let ev = BuildEvent {
                event: if event == "ready" { "ready" } else { "built" },
                generation,
                ms: rep.ms,
                emitted: rep.emitted,
                errors: diags
                    .iter()
                    .filter(|d| d.severity == Severity::Error)
                    .count(),
                warnings: diags
                    .iter()
                    .filter(|d| d.severity == Severity::Warning)
                    .count(),
            };
            println!("{}", serde_json::to_string(&ev).unwrap());
        } else {
            print_diags(diags);
            if event == "ready" {
                println!("ready");
            } else {
                println!("built in {:.0} ms", rep.ms);
            }
        }
        let _ = io::stdout().flush();
    });
    match r {
        Ok(()) => exit::OK,
        Err(e) => {
            eprintln!("error: {e}");
            exit::INTERNAL
        }
    }
}

/// B1: restricted-mode validation (config, index, route, merge) without JS.
fn check(o: &Opts) -> i32 {
    let mut diags = Vec::new();
    if code_in(&o.project).has_code && !trust::is_trusted(&o.project, &code_in(&o.project).plugins)
    {
        let mut d = Diagnostic::new(
            Code::Untrusted,
            "project has generators or plugins; checked in restricted mode (no JS was run)",
        );
        d.severity = Severity::Note;
        diags.push(d);
    }
    if o.json {
        println!(
            "{}",
            serde_json::to_string(&sarif::to_sarif(&diags)).unwrap()
        );
    } else {
        print_diags(&diags);
    }
    // A note is not a failure.
    if diags.iter().any(|d| d.severity == Severity::Error) {
        exit::DIAGNOSTICS
    } else {
        exit::OK
    }
}

fn explain(code: &str, json: bool) -> i32 {
    let Some(c) = Code::from_id(code) else {
        eprintln!("unknown diagnostic code `{code}`");
        return exit::USAGE;
    };
    if json {
        let sev = serde_json::to_value(c.default_severity()).unwrap();
        println!(
            "{}",
            json!({ "code": c.id(), "severity": sev, "doc": c.doc().trim() })
        );
    } else {
        println!("{}\n\n{}", c.id(), c.doc().trim());
    }
    exit::OK
}

fn trust_cmd(o: &Opts, yes: bool) -> i32 {
    if !yes {
        if !io::stdin().is_terminal() {
            eprintln!("error: not a terminal; pass --yes to trust this project");
            return exit::USAGE;
        }
        eprint!(
            "Trust generators and plugins in {}? [y/N] ",
            o.project.display()
        );
        let mut a = String::new();
        let _ = io::stdin().read_line(&mut a);
        if !a.trim().eq_ignore_ascii_case("y") {
            return exit::USAGE;
        }
    }
    match trust::trust(&o.project, &code_in(&o.project).plugins) {
        Ok(()) => {
            println!("trusted {}", o.project.display());
            exit::OK
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit::INTERNAL
        }
    }
}

fn main() {
    // `--help --json` must bypass clap's own help action.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") && args.iter().any(|a| a == "--json") {
        let mut c = Cli::command();
        c.build();
        println!(
            "{}",
            json!({ "schema": 1, "version": env!("CARGO_PKG_VERSION"), "command": describe(&c) })
        );
        return;
    }
    let cli = Cli::parse();
    let project = cli.project.unwrap_or_else(|| ".".into());
    let mk = |out: Option<PathBuf>| Opts {
        out: out.unwrap_or_else(|| project.join(STATE_DIR).join("out")),
        project: project.clone(),
        json: cli.json,
    };
    let code = match cli.cmd {
        Cmd::Build(a) => build(&mk(a.out), a.trust),
        Cmd::Watch(a) => watch(&mk(a.out), a.trust),
        Cmd::Check { .. } => check(&mk(None)),
        Cmd::Explain { code } => explain(&code, cli.json),
        Cmd::Trust { yes } => trust_cmd(&mk(None), yes),
    };
    process::exit(code);
}
