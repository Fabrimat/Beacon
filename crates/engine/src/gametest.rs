//! Phase-5 contract: `beacon test` (8.5). GameTests (`@minecraft/server-gametest`) run on the
//! pinned BDS from `crates/bds` (same download, EULA and extraction rules as `cargo bds smoke`).
//!
//! ```text
//! beacon test [--target <dev-like target>] [--filter <glob>] [--timeout <s>] [--accept-eula]
//!             [--yes] [--json | --sarif]
//!   build the target as an .mcworld (dev UUIDs, Beta APIs experiment on in level.dat)
//!   + the Beacon runner behavior pack (wraps `register`, prints EVENT_PREFIX lines)
//!   -> BDS (server.properties from crates/bds) -> `gametest runall` on the console
//!   -> parse_console -> TestReport -> exit 0 (all passed) | 1 (failed / timed out / none)
//! ```
//!
//! The runner protocol is Beacon's own: one console line per event, `BEACON_TEST <json>`, so
//! results never depend on the wording of BDS or GameTest messages. Console text is untrusted
//! input: unknown or malformed lines are ignored.

use crate::diag::{Code, Diagnostic};

/// Version of the `beacon test --json` shape.
pub const SCHEMA: u32 = 1;

/// Prefix of a runner line on the BDS console (anywhere in the line: BDS adds its own
/// timestamp and `[Scripting]` tag in front).
pub const EVENT_PREFIX: &str = "BEACON_TEST ";

/// One runner event (the JSON after [`EVENT_PREFIX`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    Start {
        name: String,
    },
    Pass {
        name: String,
    },
    Fail {
        name: String,
        message: String,
    },
    /// All registered tests were scheduled and finished (or reported).
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Passed,
    Failed,
    /// Started but never reported, when the run ended.
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub name: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `beacon test --json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub schema: u32,
    /// BDS version the run used (`crates/bds` pin).
    pub bds_version: String,
    /// Results in the order tests started.
    pub results: Vec<TestResult>,
    /// The runner printed [`Event::Done`].
    pub complete: bool,
}

/// Results from BDS console lines. The last event per test wins; a test that started and never
/// reported is [`Status::TimedOut`]. Returns `(results, complete)`.
pub fn parse_console<S: AsRef<str>>(lines: &[S]) -> (Vec<TestResult>, bool) {
    let mut results: Vec<TestResult> = Vec::new();
    let mut complete = false;
    let mut set = |name: String, status, message| match results.iter_mut().find(|r| r.name == name)
    {
        Some(r) => (r.status, r.message) = (status, message),
        None => results.push(TestResult {
            name,
            status,
            message,
        }),
    };
    for line in lines {
        let line = line.as_ref();
        let Some(at) = line.find(EVENT_PREFIX) else {
            continue;
        };
        match serde_json::from_str::<Event>(&line[at + EVENT_PREFIX.len()..]) {
            Ok(Event::Start { name }) => set(name, Status::TimedOut, None),
            Ok(Event::Pass { name }) => set(name, Status::Passed, None),
            Ok(Event::Fail { name, message }) => set(name, Status::Failed, Some(message)),
            Ok(Event::Done) => complete = true,
            Err(_) => {}
        }
    }
    (results, complete)
}

impl TestReport {
    /// SARIF-ready diagnostics: one per failed or timed-out test, [`Code::GameTestNone`] when
    /// nothing ran, [`Code::GameTestTimeout`] when the runner never finished.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        let mut d: Vec<Diagnostic> = self
            .results
            .iter()
            .filter_map(|r| match r.status {
                Status::Passed => None,
                Status::Failed => Some(Diagnostic::new(
                    Code::GameTestFailed,
                    format!(
                        "GameTest `{}` failed: {}",
                        r.name,
                        r.message.as_deref().unwrap_or("")
                    ),
                )),
                Status::TimedOut => Some(Diagnostic::new(
                    Code::GameTestTimeout,
                    format!("GameTest `{}` did not finish", r.name),
                )),
            })
            .collect();
        if self.results.is_empty() {
            d.push(Diagnostic::new(
                Code::GameTestNone,
                "no GameTest ran (none registered, no `@minecraft/server-gametest` dependency, \
                 or the filter matched nothing)",
            ));
        } else if !self.complete {
            d.push(Diagnostic::new(
                Code::GameTestTimeout,
                "the test run ended before the runner finished",
            ));
        }
        d
    }
}

/// `beacon test` options (CLI flags; the app passes the same).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestOptions {
    /// A `dev`-artifact target; default: the project's first one.
    #[serde(default)]
    pub target: Option<String>,
    /// Glob over test names (`ns:*`).
    #[serde(default)]
    pub filter: Option<String>,
    /// Whole-run limit in seconds, BDS download excluded.
    pub timeout_secs: u64,
    /// Accept the Minecraft EULA for BDS (logged, like `cargo bds smoke --accept-eula`).
    #[serde(default)]
    pub accept_eula: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_events() {
        let lines = [
            "[2026-10-19 12:00:00:000 INFO] Server started.",
            r#"[INFO] [Scripting] BEACON_TEST {"event":"start","name":"demo:a"}"#,
            r#"[INFO] [Scripting] BEACON_TEST {"event":"start","name":"demo:b"}"#,
            r#"[INFO] [Scripting] BEACON_TEST {"event":"start","name":"demo:c"}"#,
            r#"[INFO] [Scripting] BEACON_TEST {"event":"pass","name":"demo:a"}"#,
            r#"[INFO] [Scripting] BEACON_TEST {"event":"fail","name":"demo:b","message":"no pig"}"#,
            "[INFO] [Scripting] BEACON_TEST {not json",
            r#"[INFO] [Scripting] BEACON_TEST {"event":"done"}"#,
        ];
        let (results, complete) = parse_console(&lines);
        assert!(complete);
        let st: Vec<_> = results
            .iter()
            .map(|r| (r.name.as_str(), r.status))
            .collect();
        assert_eq!(
            st,
            [
                ("demo:a", Status::Passed),
                ("demo:b", Status::Failed),
                ("demo:c", Status::TimedOut)
            ]
        );
        let report = TestReport {
            schema: SCHEMA,
            bds_version: "1.26.52.3".into(),
            results,
            complete,
        };
        let codes: Vec<_> = report.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(codes, [Code::GameTestFailed, Code::GameTestTimeout]);
    }

    #[test]
    fn nothing_ran() {
        let (results, complete) = parse_console(&["Server started."]);
        let r = TestReport {
            schema: SCHEMA,
            bds_version: String::new(),
            results,
            complete,
        };
        assert_eq!(r.diagnostics()[0].code, Code::GameTestNone);
    }
}
