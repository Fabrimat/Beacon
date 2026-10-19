//! `beacon test` (8.5): GameTests on the pinned BDS. Stub: the runner lands in the next brief.

use super::Opts;
use beacon_engine::diag::exit;

pub fn run(_o: &Opts) -> i32 {
    eprintln!("error: `beacon test` is not implemented yet");
    exit::USAGE
}
