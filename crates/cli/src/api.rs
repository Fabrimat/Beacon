//! `beacon api [install|status|lookup]` (14.4): the Minecraft API index (8.4a) cache.
//! `beacon api --json` is the input of ctxtruth-bedrock (`schema: 1`, see ARCHITECTURE 14.4).

use super::Opts;
use beacon_api::PINS;
use beacon_bundler::Confirm;
use beacon_engine::api_index::{ApiIndex, cache_path, shared};
use beacon_engine::diag::{Code, Diagnostic, exit};
use serde_json::json;
use std::collections::BTreeMap;

#[derive(clap::Args)]
pub struct ApiArgs {
    /// Game version (default: the newest pinned version not above the project's `targetVersion`).
    #[arg(long, global = true, value_name = "ver")]
    game: Option<String>,
    #[command(subcommand)]
    cmd: Option<ApiCmd>,
}

#[derive(clap::Subcommand)]
enum ApiCmd {
    /// Download and index the pinned API packages (needs `--yes`, `BEACON_DOWNLOAD=yes` or a TTY).
    Install,
    /// Which game versions are pinned and which indexes are cached.
    Status,
    /// Search the cached index for an export, member, vanilla id or component.
    Lookup { query: String },
}

fn triple(v: &str) -> Option<[u64; 3]> {
    let mut it = v.split(['.', '-']).map(|n| n.parse().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

/// The newest pinned game version not above the project's highest target version, else the
/// nearest one above it (the same rule as `api_index::resolve` over installed indexes).
fn project_game_version(o: &Opts) -> Option<String> {
    let cfg = beacon_engine::route::load(&o.project).ok()?;
    let top = std::iter::once(&cfg.target_version)
        .chain(
            cfg.beacon
                .targets
                .values()
                .filter_map(|t| t.target_version.as_ref()),
        )
        .filter_map(|v| triple(v))
        .max()?;
    let below = PINS
        .iter()
        .filter(|p| triple(p.game_version).is_some_and(|t| t <= top))
        .max_by_key(|p| triple(p.game_version));
    let above = || {
        PINS.iter()
            .filter(|p| triple(p.game_version).is_some_and(|t| t > top))
            .min_by_key(|p| triple(p.game_version))
    };
    below.or_else(above).map(|p| p.game_version.to_owned())
}

fn fail(d: Diagnostic) -> i32 {
    crate::print_diags(std::slice::from_ref(&d));
    // a missing index is only a Note in `check`, but here it is the command's failure
    match crate::exit_code(&[d]) {
        exit::OK => exit::DIAGNOSTICS,
        c => c,
    }
}

pub fn run(o: &Opts, a: &ApiArgs) -> i32 {
    let game = a.game.clone().or_else(|| project_game_version(o));
    let need = || {
        game.clone().ok_or_else(|| {
            Diagnostic::new(
                Code::ApiIndexMissing,
                "no pinned game version for this project; pass --game",
            )
        })
    };
    match &a.cmd {
        Some(ApiCmd::Install) => {
            let g = match need() {
                Ok(g) => g,
                Err(d) => return fail(d),
            };
            let confirm = if o.download {
                Confirm::Yes
            } else {
                Confirm::Tty
            };
            match beacon_api::ensure(&g, confirm) {
                Ok(p) => {
                    if o.json {
                        println!("{}", json!({ "schema": 1, "gameVersion": g, "path": p }));
                    } else {
                        println!("API index for {g} ready at {}", p.display());
                    }
                    exit::OK
                }
                Err(d) => fail(d),
            }
        }
        Some(ApiCmd::Status) => {
            let rows: Vec<_> = PINS
                .iter()
                .map(|p| {
                    json!({
                        "gameVersion": p.game_version,
                        "installed": ApiIndex::load(p.game_version).is_ok(),
                        "path": cache_path(p.game_version),
                    })
                })
                .collect();
            if o.json {
                println!("{}", json!({ "schema": 1, "versions": rows }));
            } else {
                for r in &rows {
                    let inst = if r["installed"] == true {
                        "installed"
                    } else {
                        "not installed"
                    };
                    println!("{}: {inst}", r["gameVersion"].as_str().unwrap_or(""));
                }
            }
            exit::OK
        }
        Some(ApiCmd::Lookup { query }) => {
            let g = match need() {
                Ok(g) => g,
                Err(d) => return fail(d),
            };
            let ix = match shared(&g) {
                Ok(ix) => ix,
                Err(d) => return fail(d),
            };
            let hits = ix.lookup(query, 20);
            if o.json {
                println!("{}", json!({ "schema": 1, "gameVersion": g, "hits": hits }));
            } else {
                for h in &hits {
                    let dep = if h.deprecated { " (deprecated)" } else { "" };
                    println!("{}{dep}", h.path);
                    if let Some(s) = &h.signature {
                        println!("    {s}");
                    }
                    if let Some(d) = &h.doc {
                        println!("    {d}");
                    }
                }
                if hits.is_empty() {
                    println!("no match for `{query}` in game version {g}");
                }
            }
            exit::OK
        }
        None => summary(o, game.as_deref()),
    }
}

/// `beacon api [--json]`: `{schema, gameVersion, installed, modules: {name: [versions]},
/// vanilla: {kind: idCount}}`. Never fails for a missing index (`installed: false`).
fn summary(o: &Opts, game: Option<&str>) -> i32 {
    let ix = game.and_then(|g| shared(g).ok());
    let mut modules: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut vanilla: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(ix) = &ix {
        for (n, v) in &ix.script_modules {
            modules.insert(n.clone(), v.iter().map(|m| m.version.clone()).collect());
        }
        for (k, ids) in &ix.vanilla {
            vanilla.insert(k.clone(), ids.len());
        }
    }
    if o.json {
        println!(
            "{}",
            json!({ "schema": 1, "gameVersion": game, "installed": ix.is_some(),
                    "modules": modules, "vanilla": vanilla })
        );
    } else if ix.is_none() {
        println!(
            "no API index for {}; run `beacon api install`",
            game.unwrap_or("this project")
        );
    } else {
        println!("game version {}", game.unwrap_or(""));
        for (n, v) in &modules {
            println!("  {n}: {}", v.join(", "));
        }
        for (k, c) in &vanilla {
            println!("  vanilla {k}: {c} ids");
        }
    }
    exit::OK
}
