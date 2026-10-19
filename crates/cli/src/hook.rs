//! `beacon hook pre-tool-use|stop`: Claude Code hooks (8.2). Event JSON on stdin, decision
//! JSON on stdout, always exit 0 (exit 2 would block; a broken hook must never do that).
//!
//! Schema verified against https://code.claude.com/docs/en/hooks (PreToolUse input:
//! `tool_name`, `tool_input.file_path` | `notebook_path`, `cwd`; output
//! `hookSpecificOutput.permissionDecision` = allow|deny|ask|defer + `permissionDecisionReason`;
//! Stop input `stop_hook_active`, output `{"decision":"block","reason":...}`; empty output or
//! `{}` = no opinion).
//! No opinion is printed instead of an explicit `allow`: `allow` would skip the user's own
//! permission prompt for the edit.

use super::migrate::HookEvent;
use beacon_engine::scaffold::{Edit, generated_file, normalize, project_root};
use serde_json::{Value, json};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn run(event: HookEvent) -> i32 {
    let mut s = String::new();
    let input = std::io::stdin()
        .take(16 << 20)
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()));
    let out = match (input, event) {
        (Err(e), _) => {
            eprintln!("beacon hook: unreadable input ({e}); allowing");
            json!({})
        }
        (Ok(v), HookEvent::PreToolUse) => pre_tool_use(&v),
        (Ok(v), HookEvent::Stop) => stop(&v),
    };
    println!("{out}");
    0
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn pair(e: &Value) -> Option<(&str, &str)> {
    Some((str_of(e, "old_string")?, str_of(e, "new_string")?))
}

fn pre_tool_use(v: &Value) -> Value {
    let (Some(tool), Some(input), Some(cwd)) = (
        str_of(v, "tool_name"),
        v.get("tool_input"),
        str_of(v, "cwd"),
    ) else {
        eprintln!("beacon hook: missing tool_name, tool_input or cwd; allowing");
        return json!({});
    };
    let key = if tool == "NotebookEdit" {
        "notebook_path"
    } else {
        "file_path"
    };
    if !matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        return json!({});
    }
    let Some(path) = str_of(input, key) else {
        eprintln!("beacon hook: {tool} without {key}; allowing");
        return json!({});
    };
    let cwd = Path::new(cwd);
    let file = normalize(cwd, Path::new(path));
    let edit = match tool {
        "Write" => Edit::Write(str_of(input, "content").unwrap_or_default()),
        "Edit" => Edit::Replace(pair(input).into_iter().collect()),
        "MultiEdit" => Edit::Replace(
            input
                .get("edits")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(pair).collect())
                .unwrap_or_default(),
        ),
        _ => Edit::Opaque,
    };
    let root = file
        .parent()
        .and_then(project_root)
        .or_else(|| project_root(cwd));
    match generated_file(root.as_deref(), &file, &edit) {
        Some(reason) => json!({ "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }}),
        None => json!({}),
    }
}

/// `beacon check` (restricted unless the project is trusted) in the project of `cwd`; blocks
/// the stop only when check reports errors (exit 1).
fn stop(v: &Value) -> Value {
    if v.get("stop_hook_active").and_then(Value::as_bool) == Some(true) {
        return json!({});
    }
    let Some(root) =
        str_of(v, "cwd").and_then(|c| project_root(&normalize(Path::new("."), Path::new(c))))
    else {
        return json!({});
    };
    let run = std::env::current_exe().and_then(|exe| {
        Command::new(exe)
            .arg("--project")
            .arg(&root)
            .arg("check")
            .stdin(Stdio::null())
            .output()
    });
    match run {
        Ok(o) if o.status.code() == Some(1) => {
            let text = String::from_utf8_lossy(&o.stderr);
            let summary: String = text.lines().take(20).collect::<Vec<_>>().join("\n");
            json!({
                "decision": "block",
                "reason": format!("`beacon check` reports errors; fix them before finishing:\n{}", summary.chars().take(2000).collect::<String>()),
            })
        }
        Ok(o) if o.status.success() => json!({}),
        Ok(o) => {
            eprintln!(
                "beacon hook: `beacon check` exited {:?}; not blocking",
                o.status.code()
            );
            json!({})
        }
        Err(e) => {
            eprintln!("beacon hook: cannot run `beacon check` ({e}); not blocking");
            json!({})
        }
    }
}
