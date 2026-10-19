//! `beacon watch` (9.3): every change batch is one `Session::build`, re-indexing only the
//! changed paths. The event side (coalescing, stability re-stat, ignores, poll fallback)
//! is `beacon_serve::watch`, shared with the daemon's watcher.
//!
//! - A watcher error or `Rescan` flag makes the next build a full one (`Session::rescan`).
//! - Ctrl-C ends the loop between builds, never in the middle of one.

use super::{BuildArgs, Opts, exit_code, report, session};
use beacon_engine::diag::exit;
use beacon_serve::watch::Watch;

pub fn run(o: &Opts, a: &BuildArgs) -> i32 {
    let mut s = session(o, a);
    let root = s.root().to_path_buf();
    let w = match Watch::start(&root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot watch {}: {e}", root.display());
            return exit::INTERNAL;
        }
    };
    let _ = ctrlc::set_handler(w.quitter());
    let r = s.build();
    report(o, "ready", &r);
    if exit_code(&r.diagnostics) == exit::UNTRUSTED {
        return exit::UNTRUSTED;
    }
    let mut ignored = s.output_dirs();
    while let Some(b) = w.next(&ignored) {
        if b.touches_config(&root) {
            ignored = s.output_dirs(); // pack folders may have moved
        }
        b.apply(&mut s);
        let r = s.build();
        report(o, "built", &r);
    }
    exit::OK
}
