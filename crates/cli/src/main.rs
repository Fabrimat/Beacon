//! `beacon` CLI (6.2). Non-interactive unless a command says otherwise (`trust` prompts on a TTY).
//! The pipeline is `beacon_engine::build::Session`; this crate wires in the QuickJS host and
//! the esbuild bundler.

mod add;
mod api;
mod debug;
mod fix;
mod hook;
mod init;
mod migrate;
mod pack;
mod plugin;
mod rename;
mod serve;
mod setup;
mod symbols;
mod test;
mod watch;

use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, BuildOutcome, ReleaseOptions, Session};
use beacon_engine::diag::{Code, Diagnostic, Severity, exit};
use beacon_engine::event::BuildEvent;
use beacon_engine::host::Bundler;
use beacon_engine::{sarif, trust};
use beacon_jshost::{Host, Limits};
use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process;

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
    /// Allow downloads (esbuild) and skip confirmation prompts. `BEACON_DOWNLOAD=yes` allows
    /// downloads only; it never confirms anything else (`trust`).
    #[arg(long, global = true)]
    yes: bool,
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
    Trust,
    /// Release build: bump pack versions from content changes and write the lockfile.
    ///
    /// Without `--target`/`--all` this releases the packs projection (`packs.*`), not an archive.
    /// With `--target`/`--all` it releases the archive targets; dev targets never change versions.
    Pack(pack::PackArgs),
    /// Create a new project (prompts on a TTY; flags, `--yes` or `--from` otherwise).
    Init(init::InitArgs),
    /// Apply safe fixes (e.g. regenerate the AGENTS.md section).
    Fix,
    /// MCP server on stdio: alias of `serve --stdio --mcp` (read tools; write tools need `--allow`).
    Mcp(serve::McpArgs),
    /// The per-project daemon (LSP + MCP), its stdio shim and control (`--stdio`, `--status`, `--stop`).
    Serve(serve::ServeArgs),
    /// Print the resolved minecraft-js debug configuration of a dev target and output.
    DebugConfig(debug::DebugArgs),
    /// Convert a Regolith project (ModularMC, system_template) into a Beacon project, once.
    Migrate(migrate::MigrateArgs),
    /// Add an entity, item, block, or module from the convention templates.
    Add {
        kind: migrate::AddKind,
        name: String,
        /// Module to add the content to.
        #[arg(long, value_name = "m")]
        module: Option<String>,
        /// Add convention files even to a module that uses `packRoots`.
        #[arg(long)]
        force_convention: bool,
    },
    /// Rename an output, keeping its lockfile identities (UUIDs, releases).
    RenameOutput { from: String, to: String },
    /// Rename an id (entity, item, block, ...) and every reference to it; all files or none.
    Rename {
        from: String,
        to: String,
        /// Rename only this kind's definitions (default: every kind `from` is defined as).
        #[arg(long, value_name = "k", value_parser = symbols::parse_kind)]
        kind: Option<beacon_engine::symbols::SymbolKind>,
        /// Print the edits without writing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// List the project's ids with their definitions and reference counts.
    Symbols {
        /// Only names equal to, starting with, or containing this text (in that order).
        #[arg(long, value_name = "x")]
        find: Option<String>,
        /// Only this kind.
        #[arg(long, value_name = "k", value_parser = symbols::parse_kind)]
        kind: Option<beacon_engine::symbols::SymbolKind>,
    },
    /// Pin and update git-sourced plugins.
    Plugin {
        #[command(subcommand)]
        cmd: plugin::PluginCmd,
    },
    /// The Minecraft API index: install, status, lookup; bare `api --json` is the schema-1 summary.
    Api(api::ApiArgs),
    /// Claude Code hook: reads the event JSON on stdin, prints the decision JSON (8.2).
    Hook { event: migrate::HookEvent },
    /// Detect Minecraft and install the pinned tools (esbuild, API index, BDS) you name, with consent.
    Setup(setup::SetupArgs),
    /// Re-check Minecraft detection and every pinned tool; exits 1 when a pin check fails.
    Doctor,
    /// Run the project's GameTests on the pinned BDS (not implemented yet).
    Test,
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
    /// Explicit `--yes` on the command line (confirms `trust`).
    yes: bool,
    /// `--yes` or `BEACON_DOWNLOAD=yes`: downloads are pre-confirmed.
    download: bool,
}

/// The trust key: `beacon.plugins` plus lockfile pins (validation is the engine's job, a missing
/// or broken file just means "none").
fn plugins(project: &Path) -> Vec<Value> {
    beacon_engine::plugin::project_trust_key(project)
}

fn session(o: &Opts, a: &BuildArgs) -> Session {
    session_with(o, a, None)
}

fn session_with(o: &Opts, a: &BuildArgs, release: Option<ReleaseOptions>) -> Session {
    let opts = BuildOptions {
        target: a.target.clone(),
        all: a.all,
        out: a.out.clone(),
        frozen: a.frozen,
        trusted: a.trust || trust::is_trusted(&o.project, &plugins(&o.project)),
        release,
    };
    // Restricted mode never runs the sidecar (5.5), and locating it executes it (`--version`)
    // and may download it: only when trusted. The lookup is lazy (first bundle), so a missing
    // esbuild is only reported when the project has scripts.
    let confirm = if o.download {
        Confirm::Yes
    } else {
        Confirm::Tty
    };
    let bundler = opts
        .trusted
        .then(|| Box::new(LazyEsbuild::new(&o.project, confirm)) as Box<dyn Bundler>);
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
    } else if diags
        .iter()
        .filter(err)
        .any(|d| d.code == Code::DownloadNeedsConfirm)
    {
        exit::USAGE
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
    let mut diags = session(o, &a).check().diagnostics;
    diags.extend(fix::agent_diags(&o.project));
    if o.json {
        println!(
            "{}",
            serde_json::to_string(&sarif::to_sarif(&diags)).unwrap()
        );
    } else {
        print_diags(&diags);
    }
    exit_code(&diags)
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

fn trust_cmd(o: &Opts) -> i32 {
    if !o.yes {
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
        yes: cli.yes,
        download: cli.yes
            || std::env::var("BEACON_DOWNLOAD").is_ok_and(|v| {
                matches!(v.to_ascii_lowercase().as_str(), "yes" | "y" | "true" | "1")
            }),
    };
    let code = match cli.cmd {
        Cmd::Build(a) => build(&o, &a),
        Cmd::Watch(a) => watch::run(&o, &a),
        Cmd::Check { trust } => check(&o, trust),
        Cmd::Explain { code } => explain(&code, cli.json),
        Cmd::Trust => trust_cmd(&o),
        Cmd::Pack(a) => pack::run(&o, &a),
        Cmd::Init(a) => init::run(&o, a),
        Cmd::Fix => fix::run(&o),
        Cmd::Mcp(a) => serve::mcp(&o, &a),
        Cmd::Serve(a) => serve::run(&o, &a),
        Cmd::DebugConfig(a) => debug::run(&o, &a),
        Cmd::Migrate(a) => migrate::run(&o, &a),
        Cmd::Add {
            kind,
            name,
            module,
            force_convention,
        } => add::run(&o, kind, &name, module.as_deref(), force_convention),
        Cmd::RenameOutput { from, to } => rename::run(&o, &from, &to),
        Cmd::Rename {
            from,
            to,
            kind,
            dry_run,
        } => symbols::rename(&o, &from, &to, kind, dry_run),
        Cmd::Symbols { find, kind } => symbols::symbols(&o, find.as_deref(), kind),
        Cmd::Plugin { cmd } => plugin::run(&o, &cmd),
        Cmd::Api(a) => api::run(&o, &a),
        Cmd::Hook { event } => hook::run(event),
        Cmd::Setup(a) => setup::run(&o, &a),
        Cmd::Doctor => setup::doctor(&o),
        Cmd::Test => test::run(&o),
    };
    process::exit(code);
}
