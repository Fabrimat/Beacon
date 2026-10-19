//! `beacon pack`: a release build (4.2.1): bumps pack versions from content changes and
//! records them in `beacon.lock.json` once every artifact is written.

use super::{BuildArgs, Opts, exit_code, print_diags, session_with};
use beacon_engine::build::{Bump, ReleaseOptions};
use beacon_engine::diag::{Code, exit};
use clap::ValueEnum;

#[derive(clap::Args)]
pub struct PackArgs {
    #[command(flatten)]
    build: BuildArgs,
    /// Bump the packs (all, or `--pack`) even if their content did not change.
    #[arg(long, value_enum, value_name = "minor|major")]
    bump: Option<BumpArg>,
    /// Release this version (`versionPolicy: manual`); must not be lower than the released one.
    #[arg(long, value_name = "x.y.z")]
    set_version: Option<String>,
    /// Only this pack (repeatable): `<output>/BP`, `<output>/RP`, an output id, `world:<dir>` or `skins:<dir>`.
    #[arg(long, value_name = "id")]
    pack: Vec<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum BumpArg {
    Minor,
    Major,
}

fn parse_version(s: &str) -> Option<[u32; 3]> {
    let mut p = s.split('.').map(|n| n.parse().ok());
    let v = [p.next()??, p.next()??, p.next()??];
    p.next().is_none().then_some(v)
}

pub fn run(o: &Opts, a: &PackArgs) -> i32 {
    let set_version = match a.set_version.as_deref().map(|s| (s, parse_version(s))) {
        Some((s, None)) => {
            eprintln!("error: --set-version {s:?} is not x.y.z");
            return exit::USAGE;
        }
        v => v.and_then(|v| v.1),
    };
    let release = ReleaseOptions {
        bump: a.bump.map(|b| match b {
            BumpArg::Minor => Bump::Minor,
            BumpArg::Major => Bump::Major,
        }),
        set_version,
        only: a.pack.clone(),
    };
    let r = session_with(o, &a.build, Some(release)).build();
    // an unknown `--pack` id is a usage error
    let usage = r
        .diagnostics
        .iter()
        .any(|d| d.code == Code::ConfigField && d.message.starts_with("--pack "));
    if o.json {
        println!("{}", serde_json::to_string(&r.report).unwrap());
    } else {
        print_diags(&r.diagnostics);
        for p in &r.report.packs {
            let [x, y, z] = p.version;
            println!(
                "{} ({}) {x}.{y}.{z}{}",
                p.pack,
                format!("{:?}", p.edition).to_lowercase(),
                if p.bumped { " (bumped)" } else { "" }
            );
        }
        for f in &r.report.artifacts {
            println!("wrote {f}");
        }
    }
    if usage {
        exit::USAGE
    } else {
        exit_code(&r.diagnostics)
    }
}
