//! `beacon` CLI (6.2). Non-interactive unless a command says otherwise (`trust` prompts on a TTY).
//! The pipeline is `beacon_engine::build::Session`; this crate wires in the QuickJS host and
//! the esbuild bundler.

mod watch;

use beacon_bundler::Esbuild;
use beacon_engine::build::{BuildOptions, BuildOutcome, Session};
use beacon_engine::config::CONFIG_FILE;
use beacon_engine::diag::{Code, Diagnostic, Severity, exit};
use beacon_engine::event::BuildEvent;
use beacon_engine::host::Bundler;
use beacon_engine::{sarif, trust};
use beacon_jshost::{Host, Limits};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::{fs, process};

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

#[derive(clap::Args, Clone)]
struct BuildArgs {
    /// Build one target from `beacon.targets`.
    #[arg(long, value_name = "t", conflicts_with = "all")]
    target: Option<String>,
    /// Build every target.
    #[arg(long)]
    all: bool,
    /// Output directory (replaces packs/.beacon/out, com.mojang for dev targets, dist/ for archives).
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
    /// Validate the project without writing anything; runs no JS unless trusted.
    Check {
        /// Also run generators and the bundler for this run (does not persist).
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
    json: bool,
}

/// `beacon.plugins` straight from `config.json` (the trust key; validation is the engine's job,
/// a missing or broken config just means "no plugins" here).
fn plugins(project: &Path) -> Vec<Value> {
    let cfg: Value = fs::read(project.join(CONFIG_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    cfg["beacon"]["plugins"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn session(o: &Opts, a: &BuildArgs) -> Session {
    let opts = BuildOptions {
        target: a.target.clone(),
        all: a.all,
        out: a.out.clone(),
        frozen: a.frozen,
        trusted: a.trust || trust::is_trusted(&o.project, &plugins(&o.project)),
    };
    // Restricted mode never runs the sidecar (5.5), and `find` itself executes the binary
    // (`--version`), which may be project-shipped (`node_modules`): only look when trusted.
    // A missing esbuild is reported by the session, only if the project has scripts.
    let bundler = opts
        .trusted
        .then(|| Esbuild::find(&o.project).ok())
        .flatten()
        .map(|e| Box::new(e) as Box<dyn Bundler>);
    Session::new(
        &o.project,
        opts,
        Box::new(Host::new(Limits::default())),
        bundler,
    )
}

fn exit_code(diags: &[Diagnostic]) -> i32 {
    let err = |d: &&Diagnostic| d.severity == Severity::Error;
    if diags.iter().filter(err).any(|d| d.code == Code::Untrusted) {
        exit::UNTRUSTED
    } else if diags.iter().filter(err).any(|d| d.code == Code::Internal) {
        exit::INTERNAL
    } else if diags.iter().any(|d| err(&d)) {
        exit::DIAGNOSTICS
    } else {
        exit::OK
    }
}

fn count(diags: &[Diagnostic], s: Severity) -> usize {
    diags.iter().filter(|d| d.severity == s).count()
}

fn print_diags(diags: &[Diagnostic]) {
    eprint!("{}", sarif::to_text(diags));
}

fn build(o: &Opts, a: &BuildArgs) -> i32 {
    let r = session(o, a).build();
    if o.json {
        println!("{}", serde_json::to_string(&r.report).unwrap());
    } else {
        print_diags(&r.diagnostics);
        for a in &r.report.artifacts {
            println!("wrote {a}");
        }
        println!(
            "built in {:.0} ms ({} files written)",
            r.report.ms, r.report.emitted
        );
    }
    exit_code(&r.diagnostics)
}

/// After every build (files flushed): a `BuildEvent` JSON line with `--json`, else the
/// diagnostics and `ready` / `built in N ms`.
fn report(o: &Opts, event: &'static str, r: &BuildOutcome) {
    if o.json {
        let ev = BuildEvent {
            event,
            generation: r.generation,
            ms: r.report.ms,
            emitted: r.report.emitted,
            errors: count(&r.diagnostics, Severity::Error),
            warnings: count(&r.diagnostics, Severity::Warning),
        };
        println!("{}", serde_json::to_string(&ev).unwrap());
    } else {
        print_diags(&r.diagnostics);
        if event == "ready" {
            println!("ready");
        } else {
            println!(
                "built in {:.0} ms ({} files written)",
                r.report.ms, r.report.emitted
            );
        }
    }
    let _ = io::stdout().flush();
}

fn check(o: &Opts, trusted: bool) -> i32 {
    let a = BuildArgs {
        target: None,
        all: false,
        out: None,
        trust: trusted,
        frozen: false,
    };
    let r = session(o, &a).check();
    if o.json {
        println!("{}", serde_json::to_string(&r.report.sarif).unwrap());
    } else {
        print_diags(&r.diagnostics);
    }
    exit_code(&r.diagnostics)
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
    match trust::trust(&o.project, &plugins(&o.project)) {
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
    let o = Opts {
        project: cli.project.unwrap_or_else(|| ".".into()),
        json: cli.json,
    };
    let code = match cli.cmd {
        Cmd::Build(a) => build(&o, &a),
        Cmd::Watch(a) => watch::run(&o, &a),
        Cmd::Check { trust } => check(&o, trust),
        Cmd::Explain { code } => explain(&code, cli.json),
        Cmd::Trust { yes } => trust_cmd(&o, yes),
    };
    process::exit(code);
}
