//! Phase-5 contract: `beacon test` (8.5). GameTests (`@minecraft/server-gametest`) run on the
//! pinned BDS from `crates/bds` (same download, EULA and extraction rules as `cargo bds smoke`).
//!
//! ```text
//! beacon test [--target <dev target>] [--filter <glob>] [--timeout <s>] [--test-timeout <s>]
//!             [--accept-eula] [--yes] [--trust] [--json | --sarif]
//!   build the dev target into a temp dir, its BP bundled with the runner shim aliased in place
//!   of `@minecraft/server-gametest` (so it sees the pack's own registrations)
//!   -> the dev world + packs into a temp BDS (crates/bds), Beta APIs on (`beta_apis`)
//!   -> `Server started.` -> RUN_COMMAND -> parse_console -> TestReport
//!   -> exit 0 (all passed) | 1 (failed / timed out / none / BDS failed) | 2 (usage, EULA)
//! ```
//!
//! The runner protocol is Beacon's own: one console line per event, `BEACON_TEST <json>`.
//! Spike on BDS 1.26.52.3: the API only exposes `Test.isCompleted()`, never the verdict, so the
//! shim reports `registered` / `start` / `done`, and the verdicts come from the game's own console
//! lines (`onTestPassed: <name>`, `onTestFailed: <name> - <message>`), mapped into the same
//! events ([`parse_line`]). There is no `gametest runall`: the shim tags the selected tests
//! [`RUN_TAG`] and Beacon sends `gametest runset <tag>`. Console text is untrusted input: unknown
//! or malformed lines are ignored.

use crate::diag::{Code, Diagnostic};
use crate::nbt::{self, Tag};

/// Version of the `beacon test --json` shape.
pub const SCHEMA: u32 = 1;

/// Prefix of a runner line on the BDS console (anywhere in the line: BDS adds its own
/// timestamp and `[Scripting]` tag in front).
pub const EVENT_PREFIX: &str = "BEACON_TEST ";

/// The runner shim, bundled in place of `@minecraft/server-gametest` (see [`shim`]).
const SHIM: &str = include_str!("gametest_shim.js");
/// Placeholder specifier the shim imports the real module under: an esbuild external that the
/// bundle step renames back to [`GAMETEST_MODULE`] (the alias would also catch the shim's own
/// import of the real module).
pub const SHIM_REAL: &str = "beacon-real-server-gametest";
/// The module the shim stands in for.
pub const GAMETEST_MODULE: &str = "@minecraft/server-gametest";
/// Tag the shim gives every selected test.
pub const RUN_TAG: &str = "beacon:run";
/// Console command that runs the selected tests (BDS 1.26.52.3 has no `gametest runall`).
pub const RUN_COMMAND: &str = "gametest runset beacon:run";
/// The game's verdict for a test that ran out of its tick budget (`maxTicks`), BDS 1.26.52.3.
const GAME_TIMEOUT: &str = "Didn't succeed or fail within";

/// The shim source with the test filter (a glob over `class:name`, `*` and `?`; `None` = every
/// test) baked in as a JSON string literal.
pub fn shim(filter: Option<&str>) -> String {
    SHIM.replace(
        "__BEACON_FILTER__",
        &serde_json::to_string(&filter).expect("json"),
    )
}

/// Turns the Beta APIs experiment (`experiments.gametest`) on in a `level.dat`; GameTest does not
/// load without it. Run after the build: the Bedrock edition transform clears the experiment
/// bookkeeping flags.
pub fn beta_apis(level_dat: &[u8]) -> Result<Vec<u8>, String> {
    let (version, mut root) = nbt::parse_level_dat(level_dat)?;
    for k in [
        "gametest",
        "experiments_ever_used",
        "saved_with_toggled_experiments",
    ] {
        nbt::set(&mut root, &format!("experiments.{k}"), Tag::Byte(1))?;
    }
    Ok(nbt::write_level_dat(version, &root))
}

/// One runner event (the JSON after [`EVENT_PREFIX`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event {
    /// The shim selected this test (at script load, before `Server started.`).
    Registered {
        name: String,
    },
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
    /// Every selected test completed.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Passed,
    Failed,
    /// Selected or started but never reported when the run ended, or the game's own tick
    /// budget (`maxTicks`) ran out.
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
    /// Results in the order tests were selected (or started).
    pub results: Vec<TestResult>,
    /// The runner printed [`Event::Done`].
    pub complete: bool,
}

/// One console line as a runner event: an [`EVENT_PREFIX`] line (never a verdict), else one of the game's own
/// GameTest lines (`onTestStructureLoaded: <name>` = start, `onTestPassed: <name>`,
/// `onTestFailed: <name> - <message>`). `None` for anything else, malformed JSON included.
pub fn parse_line(line: &str) -> Option<Event> {
    if let Some(at) = line.find(EVENT_PREFIX) {
        // Verdicts come only from the game's own lines: a pack script can print this prefix too.
        return serde_json::from_str(&line[at + EVENT_PREFIX.len()..])
            .ok()
            .filter(|e| !matches!(e, Event::Pass { .. } | Event::Fail { .. }));
    }
    let name = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    if let Some(n) = line.strip_prefix("onTestStructureLoaded: ") {
        Some(Event::Start { name: name(n)? })
    } else if let Some(n) = line.strip_prefix("onTestPassed: ") {
        Some(Event::Pass { name: name(n)? })
    } else if let Some(r) = line.strip_prefix("onTestFailed: ") {
        let (n, m) = r.split_once(" - ").unwrap_or((r, ""));
        Some(Event::Fail {
            name: name(n)?,
            message: m.to_owned(),
        })
    } else {
        None
    }
}

/// Results from BDS console lines. The last verdict per test wins and a later `start` keeps it
/// (the shim and the game both report starts); a test selected or started without a verdict is
/// [`Status::TimedOut`], as is a game failure for running out of ticks. Returns
/// `(results, complete)`.
pub fn parse_console<S: AsRef<str>>(lines: &[S]) -> (Vec<TestResult>, bool) {
    let mut results: Vec<TestResult> = Vec::new();
    let mut complete = false;
    let mut set = |name: String, verdict: Option<(Status, Option<String>)>| {
        let i = match results.iter().position(|r| r.name == name) {
            Some(i) => i,
            None => {
                results.push(TestResult {
                    name,
                    status: Status::TimedOut,
                    message: None,
                });
                results.len() - 1
            }
        };
        if let Some((status, message)) = verdict {
            (results[i].status, results[i].message) = (status, message);
        }
    };
    for line in lines {
        match parse_line(line.as_ref()) {
            Some(Event::Registered { name } | Event::Start { name }) => set(name, None),
            Some(Event::Pass { name }) => set(name, Some((Status::Passed, None))),
            Some(Event::Fail { name, message }) => {
                let status = if message.starts_with(GAME_TIMEOUT) {
                    Status::TimedOut
                } else {
                    Status::Failed
                };
                set(name, Some((status, Some(message))))
            }
            Some(Event::Done) => complete = true,
            None => {}
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
                    match &r.message {
                        Some(m) => format!("GameTest `{}` timed out: {m}", r.name),
                        None => format!("GameTest `{}` did not finish", r.name),
                    },
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
    /// Per-test limit in seconds from the test's start (the game's `maxTicks` also applies).
    #[serde(default = "per_test")]
    pub test_timeout_secs: u64,
    /// Accept the Minecraft EULA for BDS (logged, like `cargo bds smoke --accept-eula`).
    #[serde(default)]
    pub accept_eula: bool,
}

fn per_test() -> u64 {
    60
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
            r#"[INFO] [Scripting] BEACON_TEST {"event":"pass","name":"demo:c"}"#,
            "onTestPassed: demo:a",
            "onTestFailed: demo:b - no pig",
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

    /// Lines as BDS 1.26.52.3 printed them in the spike: the shim's (`[Scripting]`, timestamped)
    /// interleaved with the game's bare `onTest*` lines.
    #[test]
    fn shim_and_game_lines() {
        let lines = [
            r#"[2026-10-19 20:04:26:176 WARN] [Scripting] BEACON_TEST {"event":"registered","name":"beacon:passes"}"#,
            r#"[2026-10-19 20:04:26:176 WARN] [Scripting] BEACON_TEST {"event":"registered","name":"beacon:fails"}"#,
            r#"[2026-10-19 20:04:26:176 WARN] [Scripting] BEACON_TEST {"event":"registered","name":"beacon:hangs"}"#,
            r#"[2026-10-19 20:04:26:176 WARN] [Scripting] BEACON_TEST {"event":"registered","name":"beacon:never"}"#,
            "[2026-10-19 20:04:29:000 INFO] Running 4 tests with tag 'beacon:run'...",
            "onTestStructureLoaded: beacon:passes",
            "onTestStructureLoaded: beacon:fails",
            "onTestStructureLoaded: beacon:hangs",
            "onTestPassed: beacon:passes",
            // the shim's start can come after the game's verdict: the verdict stays
            r#"[2026-10-19 20:04:29:493 WARN] [Scripting] BEACON_TEST {"event":"start","name":"beacon:passes"}"#,
            "onTestFailed: beacon:fails - expected a pig - really",
            "onTestFailed: beacon:hangs - Didn't succeed or fail within 20 ticks",
            "onTestPassed: ",
            "onTestFailed:",
            "xonTestPassed: beacon:fails",
        ];
        let (results, complete) = parse_console(&lines);
        assert!(!complete);
        let st: Vec<_> = results
            .iter()
            .map(|r| (r.name.as_str(), r.status, r.message.as_deref()))
            .collect();
        assert_eq!(
            st,
            [
                ("beacon:passes", Status::Passed, None),
                (
                    "beacon:fails",
                    Status::Failed,
                    Some("expected a pig - really")
                ),
                (
                    "beacon:hangs",
                    Status::TimedOut,
                    Some("Didn't succeed or fail within 20 ticks")
                ),
                ("beacon:never", Status::TimedOut, None),
            ]
        );
        let r = TestReport {
            schema: SCHEMA,
            bds_version: String::new(),
            results,
            complete,
        };
        let codes: Vec<_> = r.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            [
                Code::GameTestFailed,
                Code::GameTestTimeout,
                Code::GameTestTimeout,
                Code::GameTestTimeout
            ]
        );
        assert_eq!(
            parse_line(r#"x BEACON_TEST {"event":"done"}"#),
            Some(Event::Done)
        );
        assert_eq!(parse_line(r#"BEACON_TEST {"event":"bogus"}"#), None);
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

    #[test]
    fn shim_filter_is_a_json_literal() {
        let s = shim(Some("a\"b*"));
        assert!(s.contains(r#"const FILTER = "a\"b*";"#));
        assert!(shim(None).contains("const FILTER = null;"));
        assert!(!s.contains("__BEACON_FILTER__"));
        assert!(s.contains(&format!("from \"{SHIM_REAL}\"")) && s.contains(RUN_TAG));
        assert!(RUN_COMMAND.ends_with(RUN_TAG));
    }

    #[test]
    fn beta_apis_on() {
        let root = Tag::Compound(vec![
            (b"LevelName".to_vec(), Tag::string("w")),
            (
                b"experiments".to_vec(),
                Tag::Compound(vec![(b"experiments_ever_used".to_vec(), Tag::Byte(0))]),
            ),
        ]);
        let out = beta_apis(&nbt::write_level_dat(10, &root)).unwrap();
        let (v, r) = nbt::parse_level_dat(&out).unwrap();
        assert_eq!(v, 10);
        for k in [
            "gametest",
            "experiments_ever_used",
            "saved_with_toggled_experiments",
        ] {
            assert_eq!(
                nbt::get(&r, &format!("experiments.{k}")),
                Some(&Tag::Byte(1)),
                "{k}"
            );
        }
        assert_eq!(nbt::get(&r, "LevelName"), Some(&Tag::string("w")));
        assert!(beta_apis(b"short").is_err());
    }
}
