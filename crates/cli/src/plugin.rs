//! `beacon plugin update [<id>]` (14.4): pins git-sourced plugins in `beacon.lock.json`.
//! Runs `git`, never project code, so it is allowed in restricted mode; the lock is written
//! only after `--yes` or a TTY confirmation.

use super::Opts;
use beacon_engine::diag::exit;
use beacon_engine::plugin::{PluginUpdate, apply_update, plan_update};
use serde_json::json;
use std::io::{self, IsTerminal};

#[derive(clap::Subcommand)]
pub enum PluginCmd {
    /// Resolve each git-sourced plugin's `ref` to a commit, show the diff and pin it.
    Update {
        /// Plugin id (default: every git-sourced plugin).
        id: Option<String>,
    },
}

fn short(s: &str) -> &str {
    &s[..s.len().min(12)]
}

fn print(u: &PluginUpdate) {
    match &u.from {
        Some(f) => println!(
            "plugin {}: {} -> {}",
            u.id,
            short(&f.commit),
            short(&u.to.commit)
        ),
        None => println!("plugin {}: new pin {}", u.id, short(&u.to.commit)),
    }
    for (m, l) in [("+", &u.added), ("-", &u.removed), ("~", &u.changed)] {
        l.iter().for_each(|f| println!("  {m} {f}"));
    }
}

pub fn run(o: &Opts, cmd: &PluginCmd) -> i32 {
    let PluginCmd::Update { id } = cmd;
    let ids: Vec<String> = id.iter().cloned().collect();
    let updates = match plan_update(&o.project, &ids, None) {
        Ok(u) => u,
        Err(d) => {
            crate::print_diags(&[d]);
            return exit::DIAGNOSTICS;
        }
    };
    let changed = updates.iter().any(|u| u.from.as_ref() != Some(&u.to));
    let confirmed = !changed || o.yes || (!o.json && confirm(&updates));
    let applied = changed && confirmed;
    if o.json {
        println!(
            "{}",
            json!({ "schema": 1, "updates": updates, "applied": applied })
        );
    } else if !changed {
        println!("plugins are up to date");
    } else if o.yes {
        updates.iter().for_each(print);
    }
    if !changed {
        return exit::OK;
    }
    if !confirmed {
        eprintln!("error: not confirmed; pass --yes to write the pins");
        return exit::USAGE;
    }
    if let Err(d) = apply_update(&o.project, &updates) {
        crate::print_diags(&[d]);
        return exit::DIAGNOSTICS;
    }
    if !o.json {
        println!("pinned; the trust key changed, run `beacon trust` to trust the new plugin code");
    }
    exit::OK
}

/// Prints the diff, then asks on a TTY (`false` without one).
fn confirm(updates: &[PluginUpdate]) -> bool {
    updates.iter().for_each(print);
    if !io::stdin().is_terminal() {
        return false;
    }
    eprint!("Write these pins to beacon.lock.json? [y/N] ");
    let mut a = String::new();
    let _ = io::stdin().read_line(&mut a);
    a.trim().eq_ignore_ascii_case("y")
}
