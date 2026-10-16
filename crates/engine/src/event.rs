//! Contract: machine output of `build --json` and `watch --json` (6.2).
//!
//! `watch --json` prints one [`BuildEvent`] per line on stdout after every completed build,
//! i.e. after the output files are flushed **and** the ledger is written (the build call
//! returns first; the ledger is tiny, so the gap is negligible). The perf harness waits
//! for these lines. The first line is `"event":"ready"` after the initial build.

#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildEvent {
    /// `ready` (initial build done, watching) | `built` (a rebuild finished).
    pub event: &'static str,
    /// Increments per build, starting at 1.
    pub generation: u64,
    pub ms: f64,
    pub emitted: usize,
    pub errors: usize,
    pub warnings: usize,
}

/// `build --json` stdout: one JSON object.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BuildReport {
    pub ms: f64,
    /// Files written (unchanged files are not counted).
    pub emitted: usize,
    /// Archives written.
    pub artifacts: Vec<String>,
    /// SARIF 2.1 log (see `sarif`).
    pub sarif: serde_json::Value,
}
