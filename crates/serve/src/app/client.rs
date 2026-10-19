//! Per-call daemon client (phase 5 decisions 1 and 2): every action opens a fresh connection
//! through [`shim::connect`] carrying only that action's grant, sends one MCP `tools/call`
//! (after `initialize`) and reads the result. The daemon is started from the bundled `beacon`
//! (`[beacon_exe, "serve", "--project", root]`), never from `current_exe` (the Tauri binary).
//! The hello never carries `trust: true`: trust comes from the stored decision, so a trust
//! grant takes effect on the next call.

use super::{AppError, AppResult};
use crate::transport::Stream;
use crate::{Channel, Grant, discovery, shim};
use beacon_engine::diag::{Code, Diagnostic};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// The grants the app may ever ask for. Never `build` or `release`.
pub const MAX_GRANTS: &[Grant] = &[Grant::Edit, Grant::Deploy];

/// A successful `tools/call`: its text blocks (unsanitized; the caller sanitizes what it
/// forwards). A tool-level failure is an [`AppError`], not a `ToolOutput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub text: Vec<String>,
}

impl ToolOutput {
    /// The first text block parsed as JSON (SARIF for `check`, results for the other tools).
    pub fn json(&self) -> AppResult<Value> {
        let t = self.text.first().map(String::as_str).unwrap_or_default();
        serde_json::from_str(t).map_err(|e| unavailable(format!("bad tool result: {e}")))
    }
}

fn unavailable(m: impl Into<String>) -> AppError {
    AppError {
        code: Code::ServeUnavailable.id().into(),
        message: super::sanitize(&m.into()),
    }
}

/// Longest a read tool (`check`) may take, and any other tool (`deploy` builds and can be slow).
const READ_DEADLINE: Duration = Duration::from_secs(30);
const WRITE_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// One reply line, within `end`: the stream's read timeout is set to what is left, so a silent
/// daemon ends the call (no helper thread, nothing left holding the connection).
fn next(r: &mut BufReader<Stream>, end: Instant) -> AppResult<Value> {
    let left = end.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(unavailable("the daemon timed out"));
    }
    r.get_mut()
        .set_read_timeout(Some(left))
        .map_err(|e| unavailable(format!("pipe: {e}")))?;
    let mut l = String::new();
    match r.read_line(&mut l) {
        Ok(0) => Err(unavailable("the daemon closed the connection")),
        Ok(_) => serde_json::from_str(&l).map_err(|e| unavailable(format!("bad reply: {e}"))),
        Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
            Err(unavailable("the daemon timed out"))
        }
        Err(e) => Err(unavailable(format!("pipe: {e}"))),
    }
}

/// `grant` must be one of [`MAX_GRANTS`] (or `None` for read tools).
pub fn call_tool(
    beacon_exe: &Path,
    root: &Path,
    grant: Option<Grant>,
    tool: &str,
    arguments: Value,
) -> AppResult<ToolOutput> {
    let deadline = if grant.is_none() {
        READ_DEADLINE
    } else {
        WRITE_DEADLINE
    };
    call_tool_within(beacon_exe, root, grant, tool, arguments, deadline)
}

/// [`call_tool`] with an explicit deadline for the whole exchange; past it: `BCN0160`-class
/// "timed out" [`AppError`] (`ServeUnavailable`).
pub fn call_tool_within(
    beacon_exe: &Path,
    root: &Path,
    grant: Option<Grant>,
    tool: &str,
    arguments: Value,
    deadline: Duration,
) -> AppResult<ToolOutput> {
    if grant.is_some_and(|g| !MAX_GRANTS.contains(&g)) {
        return Err(unavailable("the app never asks for that grant"));
    }
    let run = discovery::run_dir().ok_or_else(|| unavailable("no user runtime directory"))?;
    let opts = shim::ShimOptions {
        project: root.into(),
        channel: Channel::Mcp,
        trust: false,
        allow: grant.into_iter().collect(),
        run,
        daemon: vec![
            beacon_exe.into(),
            "serve".into(),
            "--project".into(),
            root.into(),
        ],
    };
    let mut s: Stream = shim::connect(&opts).map_err(|e| AppError {
        code: e.code.id().into(),
        message: super::sanitize(&e.message),
    })?;
    let mut r = BufReader::new(
        s.try_clone()
            .map_err(|e| unavailable(format!("pipe: {e}")))?,
    );
    talk(&mut s, &mut r, tool, arguments, deadline)
}

fn talk(
    s: &mut Stream,
    r: &mut BufReader<Stream>,
    tool: &str,
    arguments: Value,
    deadline: Duration,
) -> AppResult<ToolOutput> {
    let end = Instant::now() + deadline;
    let mut send = |v: Value| -> AppResult<()> {
        writeln!(s, "{v}")
            .and_then(|()| s.flush())
            .map_err(|e| unavailable(format!("pipe: {e}")))
    };
    send(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": { "name": "beacon-app", "version": env!("CARGO_PKG_VERSION") } } }),
    )?;
    next(r, end)?;
    send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
    send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments } }))?;
    let v = next(r, end)?;
    if let Some(e) = v.get("error") {
        return Err(unavailable(
            e["message"].as_str().unwrap_or("tool call refused"),
        ));
    }
    let text: Vec<String> = v["result"]["content"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| c["text"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if v["result"]["isError"] == true {
        let first = text.first().map(String::as_str).unwrap_or_default();
        // failures carry `{"diagnostics": [...]}`
        return Err(serde_json::from_str::<Value>(first)
            .ok()
            .and_then(|j| serde_json::from_value::<Vec<Diagnostic>>(j["diagnostics"].clone()).ok())
            .and_then(|d| d.into_iter().next())
            .map(AppError::from)
            .unwrap_or_else(|| unavailable(first)));
    }
    Ok(ToolOutput { text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{Listener, connect};

    /// A daemon that accepts and never answers: the call ends at its deadline.
    #[test]
    fn a_silent_daemon_times_out() {
        let dir = tempfile::tempdir().unwrap();
        crate::transport::secure_dir(dir.path()).unwrap();
        let (mut l, name) = Listener::bind(dir.path(), "silent").unwrap();
        let mut c = connect(&name).unwrap();
        let _held = l.accept().unwrap();
        let mut r = BufReader::new(c.try_clone().unwrap());
        let t0 = Instant::now();
        let e = talk(
            &mut c,
            &mut r,
            "deploy",
            json!({}),
            Duration::from_millis(300),
        )
        .expect_err("no reply");
        assert!(e.message.contains("timed out"), "{e:?}");
        assert!(t0.elapsed() < Duration::from_secs(5));
    }
}
