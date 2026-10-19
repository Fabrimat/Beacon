//! `beacon debug-config` (6.6): prints the resolved `minecraft-js` attach configuration of a
//! dev target/output, for editors other than VS Code (whose extension resolves it itself).

use super::{Opts, print_diags};
use beacon_engine::config::Artifact;
use beacon_engine::debug::{self, Mode};
use beacon_engine::diag::{Diagnostic, exit};
use beacon_engine::lock::Lockfile;
use beacon_engine::route;

#[derive(clap::Args)]
pub struct DebugArgs {
    /// Dev target (default: the first dev target).
    #[arg(long)]
    target: Option<String>,
    /// Output (default: the first output, else `main`).
    #[arg(long)]
    output: Option<String>,
    /// `connect` (the editor connects to the listening game) or `listen` (default `connect`).
    #[arg(long, default_value = "connect", value_parser = ["connect", "listen"])]
    mode: String,
    /// Roots as absolute paths instead of `${workspaceFolder}/...`.
    #[arg(long)]
    absolute: bool,
}

pub fn run(o: &Opts, a: &DebugArgs) -> i32 {
    match resolve(o, a) {
        Ok(c) => {
            // the config is JSON either way; `--json` is accepted for symmetry
            println!("{}", serde_json::to_string_pretty(&c).expect("json"));
            exit::OK
        }
        Err(d) => {
            print_diags(&d);
            exit::USAGE
        }
    }
}

fn resolve(o: &Opts, a: &DebugArgs) -> Result<debug::LaunchConfig, Vec<Diagnostic>> {
    let cfg = route::load(&o.project)?;
    let target = a
        .target
        .clone()
        .or_else(|| {
            cfg.beacon
                .targets
                .iter()
                .find(|(_, t)| t.artifact == Artifact::Dev)
                .map(|(id, _)| id.clone())
        })
        .unwrap_or_else(|| "dev".into());
    let output = a.output.clone().unwrap_or_else(|| {
        cfg.beacon
            .outputs
            .first()
            .map_or_else(|| "main".into(), |o| o.id.clone())
    });
    // frozen: reading the lockfile must never write it
    let lock = Lockfile::ensure(
        &o.project,
        &cfg,
        std::slice::from_ref(&output),
        &[],
        &[],
        true,
    )
    .map_err(|d| vec![d])?;
    let mode = if a.mode == "listen" {
        Mode::Listen
    } else {
        Mode::Connect
    };
    let mut c = debug::launch_config(&o.project, &cfg, &lock, &target, &output, mode)
        .map_err(|d| vec![d])?;
    if a.absolute {
        c.absolutize(&o.project);
    }
    Ok(c)
}
