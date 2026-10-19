//! `beacon rename-output <from> <to>` (6.2): config, `module.json` and lockfile, identities kept.

use super::Opts;
use beacon_engine::diag::exit;
use beacon_engine::scaffold;
use serde_json::json;

pub fn run(o: &Opts, from: &str, to: &str) -> i32 {
    match scaffold::rename_output(&o.project, from, to) {
        Ok(files) => {
            if o.json {
                println!("{}", json!({ "from": from, "to": to, "files": files }));
            } else {
                files.iter().for_each(|f| println!("updated {f}"));
                println!("renamed output `{from}` to `{to}`; run `beacon build` to emit it");
            }
            exit::OK
        }
        Err((code, msg)) => {
            eprintln!("error: {}", msg.trim_end());
            code
        }
    }
}
