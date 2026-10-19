//! `beacon fix` (6.3): applies safe fix-its only, atomically. Today the only source is the
//! generated `AGENTS.md` section (8.3); `beacon check` reports the same diagnostics.

use super::Opts;
use beacon_engine::diag::{Diagnostic, exit};
use beacon_engine::{agents, route};
use serde_json::json;
use std::path::Path;

/// Agent-doc staleness (`BCN0090`) for `beacon check`. A broken `config.json` is reported
/// by the session itself, so it yields nothing here.
pub fn agent_diags(root: &Path) -> Vec<Diagnostic> {
    route::load(root)
        .map(|cfg| agents::check(root, &cfg))
        .unwrap_or_default()
}

pub fn run(o: &Opts) -> i32 {
    match agents::apply_safe_fixes(&o.project, &agent_diags(&o.project)) {
        Ok(n) => {
            if o.json {
                println!("{}", json!({ "applied": n }));
            } else {
                println!("applied {n} safe fix(es)");
            }
            exit::OK
        }
        Err(e) => {
            eprintln!("error: {e}");
            exit::DIAGNOSTICS
        }
    }
}
