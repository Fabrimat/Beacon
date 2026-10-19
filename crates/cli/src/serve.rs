//! `beacon serve` (3, 6.4, 8.1): the per-project daemon, its stdio shim for editors and MCP
//! clients, and its control commands. `beacon mcp` is the shim with `--mcp`.
//!
//! - `serve [--idle N]`: the daemon itself (what the shim starts detached); exits with the
//!   code `daemon::run` returns;
//! - `serve --stdio (--lsp|--mcp) [--trust] [--allow edit,build,release,deploy]`: the shim.
//!   `--trust` and `--allow` are the operator's: they sit on the command line the editor or
//!   agent is configured with, never in a protocol message (5.5, 8.1);
//! - `serve --status | --stop [--force]`: control of the running daemon.

use super::Opts;
use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::diag::exit;
use beacon_engine::host::Bundler;
use beacon_jshost::{Host, Limits};
use beacon_serve::{Channel, ControlRequest, Grant, daemon, discovery, shim};
use std::time::Duration;

#[derive(clap::Args)]
pub struct ServeArgs {
    /// Relay stdio to the project's daemon (started when missing); needs --lsp or --mcp.
    #[arg(long, requires = "channel")]
    stdio: bool,
    /// The language server channel (editors).
    #[arg(long, requires = "stdio", group = "channel")]
    lsp: bool,
    /// The MCP channel (agents).
    #[arg(long, requires = "stdio", group = "channel")]
    mcp: bool,
    /// Treat the project as trusted for this client (operator-chosen; protocol messages cannot set it).
    #[arg(long, requires = "stdio")]
    trust: bool,
    /// MCP write grants for this client: a comma list of edit, build, release, deploy.
    #[arg(long, requires = "stdio", value_name = "grants")]
    allow: Option<String>,
    /// Daemon: exit this many seconds after the last client (default 60, `BEACON_SERVE_IDLE`).
    #[arg(long, value_name = "seconds", conflicts_with_all = ["stdio", "status", "stop"])]
    idle: Option<u64>,
    /// Print the running daemon's status as JSON.
    #[arg(long, conflicts_with_all = ["stdio", "stop"])]
    status: bool,
    /// Stop the running daemon (refused while clients are attached, unless --force).
    #[arg(long, conflicts_with = "stdio")]
    stop: bool,
    #[arg(long, requires = "stop")]
    force: bool,
}

/// `beacon mcp`: the MCP shim (`serve --stdio --mcp`), kept so existing client configs work.
#[derive(clap::Args)]
pub struct McpArgs {
    /// Treat the project as trusted for this server's lifetime (operator-chosen; clients cannot set it).
    #[arg(long)]
    trust: bool,
    /// Write grants for this client: a comma list of edit, build, release, deploy.
    #[arg(long, value_name = "grants")]
    allow: Option<String>,
}

pub fn run(o: &Opts, a: &ServeArgs) -> i32 {
    if a.stdio {
        let channel = if a.lsp { Channel::Lsp } else { Channel::Mcp };
        shim(o, channel, a.trust, a.allow.as_deref())
    } else if a.status {
        control(o, &ControlRequest::Status)
    } else if a.stop {
        control(o, &ControlRequest::Stop { force: a.force })
    } else {
        daemon(o, a.idle)
    }
}

pub fn mcp(o: &Opts, a: &McpArgs) -> i32 {
    shim(o, Channel::Mcp, a.trust, a.allow.as_deref())
}

/// The daemon's sessions: restricted or trusted by `opts.trusted`. A daemon never downloads
/// and never prompts (`Confirm::No`): a missing esbuild is a diagnostic.
fn factory() -> daemon::SessionFactory {
    Box::new(|root, opts: BuildOptions| {
        let bundler = opts
            .trusted
            .then(|| Box::new(LazyEsbuild::new(root, Confirm::No)) as Box<dyn Bundler>);
        Session::new(root, opts, Box::new(Host::new(Limits::default())), bundler)
    })
}

fn no_run_dir() -> i32 {
    eprintln!("error: no per-user runtime directory (set BEACON_HOME)");
    exit::DIAGNOSTICS
}

fn daemon(o: &Opts, idle: Option<u64>) -> i32 {
    let Some(mut d) = daemon::DaemonOptions::new(&o.project) else {
        return no_run_dir();
    };
    if let Some(s) = idle {
        d.idle = Duration::from_secs(s);
    }
    daemon::run(&d, factory())
}

fn shim(o: &Opts, channel: Channel, trust: bool, allow: Option<&str>) -> i32 {
    let allow = match allow.map(Grant::parse_list).transpose() {
        Ok(g) => g.unwrap_or_default(),
        Err(e) => {
            eprintln!("error: --allow: {e}");
            return exit::USAGE;
        }
    };
    let (Some(run), Ok(exe)) = (discovery::run_dir(), std::env::current_exe()) else {
        return no_run_dir();
    };
    let root = discovery::canonical_root(&o.project);
    shim::run(&shim::ShimOptions {
        project: root.clone(),
        channel,
        trust,
        allow,
        run,
        daemon: vec![exe.into(), "serve".into(), "--project".into(), root.into()],
    })
}

fn control(o: &Opts, req: &ControlRequest) -> i32 {
    let Some(run) = discovery::run_dir() else {
        return no_run_dir();
    };
    let stop = matches!(req, ControlRequest::Stop { .. });
    match shim::control(&run, &o.project, req) {
        Ok(v) if stop && v["ok"] == false => {
            eprintln!(
                "error: {} client(s) attached; close them or pass --force",
                v["clients"]
            );
            exit::DIAGNOSTICS
        }
        Ok(v) => {
            println!("{v}");
            exit::OK
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("no beacon serve daemon is running for this project");
            if stop { exit::OK } else { exit::DIAGNOSTICS }
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit::DIAGNOSTICS
        }
    }
}
