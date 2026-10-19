//! `beacon add entity|item|block|module <name>` (6.2): convention scaffolds, never overwriting.

use super::Opts;
use super::migrate::AddKind;
use beacon_engine::diag::exit;
use beacon_engine::scaffold::{self, Kind};
use serde_json::json;

pub fn run(o: &Opts, kind: AddKind, name: &str, module: Option<&str>, force: bool) -> i32 {
    let kind = match kind {
        AddKind::Entity => Kind::Entity,
        AddKind::Item => Kind::Item,
        AddKind::Block => Kind::Block,
        AddKind::Module => Kind::Module,
    };
    match scaffold::add(&o.project, kind, name, module, force) {
        Ok(a) => {
            if o.json {
                println!(
                    "{}",
                    json!({ "module": a.module, "files": a.files, "note": a.note })
                );
            } else {
                if let Some(n) = &a.note {
                    eprintln!("note: {n}");
                }
                a.files.iter().for_each(|f| println!("created {f}"));
            }
            exit::OK
        }
        Err((code, msg)) => {
            eprintln!("error: {}", msg.trim_end());
            code
        }
    }
}
