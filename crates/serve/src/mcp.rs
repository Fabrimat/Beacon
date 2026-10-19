//! Full MCP (8.1), served by the daemon on the `mcp` channel; replaces the phase-2
//! standalone `beacon mcp` (same framing, versions, size caps and trust rules; `beacon mcp`
//! becomes `beacon serve --stdio --mcp`).
//!
//! Rules for every tool:
//! - results are compact, paginated with an opaque `cursor`, capped at 64 KiB; anything
//!   carrying project or game text is marked as data, never instructions (8.1);
//! - write tools take `dryRun` (default **true**: they return the edits/diff and write
//!   nothing), need the operator's [`Grant`], and never touch protected files (lockfile,
//!   generated manifests, generated AGENTS.md regions: `scaffold::generated_file`) except
//!   through their owning command (`pack` writes versions to the lockfile; nothing else does);
//! - a file open with unsaved changes in an LSP client is `DocumentDirty`, not overwritten;
//! - code runs only in trusted sessions (5.5): `build` / `deploy` / `pack` of a project with
//!   code are `Untrusted` otherwise; edit tools run no code and work restricted.
//!
//! Not tools on purpose: `plugin update` and `trust` (human decisions that change what code
//! runs), raw file reads/writes (agents have their own; the daemon sees the result through
//! the watcher), script debugging (the official minecraft-debugger ships its own MCP server;
//! a native bridge is backlog B6), game reload / content log / GameTests (phase 5,
//! `beacon test`).

use crate::Grant;

/// Serves one MCP client on `conn` (after the handshake) until it disconnects: newline
/// JSON-RPC 2.0. Engine access only through `handle`; grants via `handle.allows`.
///
/// Wave-1 stub: every request gets a JSON-RPC error (notifications nothing), lines capped at
/// 1 MiB. The phase-2 tools stay in `beacon mcp` until phase 4 wave 2 fills this in.
pub fn serve(conn: crate::transport::Stream, handle: crate::daemon::EngineHandle) {
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Read};
    let _ = handle;
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut r = BufReader::new(conn);
    loop {
        let mut line = vec![];
        match (&mut r).take(1 << 20).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.last() != Some(&b'\n') && line.len() >= 1 << 20 {
            return; // over the cap: not a client of ours
        }
        let resp = match serde_json::from_slice::<Value>(&line) {
            Ok(v) if v.get("id").is_none() => continue, // notification
            Ok(v) => json!({
                "jsonrpc": "2.0", "id": v["id"],
                "error": { "code": -32601, "message": "beacon serve: MCP tools arrive in phase 4 wave 2; use `beacon mcp`" },
            }),
            Err(_) if line.iter().all(u8::is_ascii_whitespace) => continue,
            Err(_) => {
                json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } })
            }
        };
        if crate::write_line(&mut out, &resp).is_err() {
            return;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    pub name: &'static str,
    /// MCP annotations.
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    /// Needed operator grant; `None` for read-only tools.
    pub grant: Option<Grant>,
    pub summary: &'static str,
}

const fn read(name: &'static str, summary: &'static str) -> ToolSpec {
    ToolSpec {
        name,
        read_only: true,
        destructive: false,
        idempotent: true,
        grant: None,
        summary,
    }
}

const fn write(
    name: &'static str,
    grant: Grant,
    destructive: bool,
    idempotent: bool,
    summary: &'static str,
) -> ToolSpec {
    ToolSpec {
        name,
        read_only: false,
        destructive,
        idempotent,
        grant: Some(grant),
        summary,
    }
}

/// The phase-4 tool list (names are stable; `snake_case` like phase 2).
pub const TOOLS: &[ToolSpec] = &[
    read(
        "check",
        "Diagnostics as SARIF; `since` returns only the delta since that call's cursor.",
    ),
    read(
        "project_info",
        "Name, namespace, targetVersion, outputs, targets, trust state.",
    ),
    read("explain", "Explain a diagnostic code."),
    read(
        "symbols_find",
        "Search defined ids by name, optionally by kind (paginated).",
    ),
    read(
        "symbols_definition",
        "Where an id is defined (path, line, column).",
    ),
    read(
        "symbols_references",
        "Every reference to an id (paginated).",
    ),
    read(
        "schema_lookup",
        "Components of one file kind for the target's game version, or one component.",
    ),
    read(
        "api_lookup",
        "@minecraft/* script API signature, vanilla id or component, for the target version.",
    ),
    read(
        "debug_config",
        "Resolved minecraft-debugger launch configuration for an output and target.",
    ),
    write(
        "rename_symbol",
        Grant::Edit,
        true,
        false,
        "Rename an id and every reference (dryRun: the edits).",
    ),
    write(
        "fix",
        Grant::Edit,
        false,
        true,
        "Apply safe fix-its (dryRun: the edits).",
    ),
    write(
        "add_content",
        Grant::Edit,
        false,
        false,
        "Add an entity, item, block or module like `beacon add` (dryRun: the files).",
    ),
    write(
        "build",
        Grant::Build,
        false,
        true,
        "Build a non-dev target or the packs projection (dryRun: emitted diff).",
    ),
    write(
        "deploy",
        Grant::Deploy,
        false,
        true,
        "Build a dev target into the game's com.mojang (dryRun: emitted diff).",
    ),
    write(
        "pack",
        Grant::Release,
        false,
        false,
        "Release build: bump pack versions in the lockfile (dryRun: planned versions).",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_table_is_consistent() {
        let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
        assert!((15..=20).contains(&TOOLS.len()));
        for t in TOOLS {
            assert_eq!(t.read_only, t.grant.is_none(), "{}", t.name);
            assert!(!(t.read_only && t.destructive), "{}", t.name);
        }
        // the phase-2 tools keep their names
        for n in ["check", "project_info", "explain"] {
            assert!(TOOLS.iter().any(|t| t.name == n));
        }
    }
}
