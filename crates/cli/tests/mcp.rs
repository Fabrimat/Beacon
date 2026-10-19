//! `beacon mcp` (= `beacon serve --stdio --mcp`): the real binary over pipes, through the
//! shim and a daemon it starts. Every test isolates `BEACON_HOME` and sets
//! `BEACON_SERVE_IDLE=0`, and its guard stops the daemon, so none lingers.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    tx: ChildStdin,
    rx: BufReader<ChildStdout>,
    dir: tempfile::TempDir,
    root: PathBuf,
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let name = e.file_name();
        if ["packs", "dist", ".beacon"].contains(&name.to_str().unwrap()) {
            continue;
        }
        if e.file_type().unwrap().is_dir() {
            copy(&e.path(), &to.join(&name));
        } else {
            std::fs::copy(e.path(), to.join(&name)).unwrap();
        }
    }
}

fn beacon(dir: &Path, root: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"));
    c.args(["--project", root.to_str().unwrap()])
        .args(args)
        .env("BEACON_HOME", dir.join("home"))
        .env("BEACON_SERVE_IDLE", "0")
        .env("BEACON_ESBUILD", "no-esbuild-here")
        .env_remove("BEACON_COM_MOJANG");
    c
}

impl Mcp {
    /// `beacon mcp <extra>` on a copy of the minimal fixture.
    fn start(extra: &[&str]) -> Mcp {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal"),
            &root,
        );
        let mut args = vec!["mcp"];
        args.extend(extra);
        let mut child = beacon(dir.path(), &root, &args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let tx = child.stdin.take().unwrap();
        let rx = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            tx,
            rx,
            dir,
            root,
        }
    }

    fn raw(&mut self, line: &[u8]) -> Value {
        self.tx.write_all(line).unwrap();
        self.tx.write_all(b"\n").unwrap();
        self.tx.flush().unwrap();
        let mut s = String::new();
        self.rx.read_line(&mut s).unwrap();
        serde_json::from_str(&s).unwrap_or_else(|_| panic!("not JSON: {s:?}"))
    }

    fn req(&mut self, method: &str, params: Value) -> Value {
        let r = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        self.raw(r.to_string().as_bytes())
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.req("tools/call", json!({ "name": name, "arguments": args }))
    }

    /// A control command of the same daemon.
    fn serve(&self, args: &[&str]) -> std::process::Output {
        let mut a = vec!["serve"];
        a.extend(args);
        beacon(self.dir.path(), &self.root, &a).output().unwrap()
    }

    /// Every file of the project with its bytes.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn walk(d: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path(), out);
                } else {
                    out.insert(e.path(), std::fs::read(e.path()).unwrap());
                }
            }
        }
        let mut m = BTreeMap::new();
        walk(&self.root, &mut m);
        m
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.serve(&["--stop", "--force"]);
    }
}

fn first_text(r: &Value) -> &str {
    r["result"]["content"][0]["text"].as_str().unwrap()
}

fn json_text(r: &Value) -> Value {
    serde_json::from_str(first_text(r)).unwrap_or_else(|_| panic!("not JSON: {r}"))
}

#[test]
fn initialize_and_list() {
    let mut m = Mcp::start(&[]);
    let r = m.req("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let r = m.req("initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    // a notification gets no response: the next reply belongs to the next request
    m.tx.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .unwrap();
    assert_eq!(m.req("ping", json!({}))["result"], json!({}));
    // every tool is listed, whatever the grants
    let r = m.req("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 15);
    for t in tools {
        let a = &t["annotations"];
        assert_eq!(a["openWorldHint"], false);
        assert!(t["description"].as_str().unwrap().contains("project data"));
        let read_only = a["readOnlyHint"] == true;
        assert!(!(a["destructiveHint"] == true && read_only));
        assert!(t["inputSchema"]["properties"].is_object());
    }
    for n in ["check", "project_info", "explain"] {
        let t = tools.iter().find(|t| t["name"] == n).unwrap();
        assert_eq!(t["annotations"]["readOnlyHint"], true);
        assert_eq!(t["annotations"]["destructiveHint"], false);
    }
    let rename = tools.iter().find(|t| t["name"] == "rename_symbol").unwrap();
    assert_eq!(rename["annotations"]["readOnlyHint"], false);
    assert_eq!(rename["annotations"]["destructiveHint"], true);
}

#[test]
fn check_is_restricted_and_writes_nothing() {
    let mut m = Mcp::start(&[]);
    // client-supplied trust is ignored, in the arguments and beside them
    let r = m.req(
        "tools/call",
        json!({ "name": "check", "trust": true, "arguments": { "trust": true } }),
    );
    assert_eq!(r["result"]["isError"], false);
    let sarif: Value = serde_json::from_str(first_text(&r)).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    assert!(first_text(&r).contains("BCN0050"));
    assert!(first_text(&r).contains("restricted mode"));
    let info = json_text(&m.tool("project_info", json!({ "trust": true })));
    assert_eq!(info["name"], "beacon-minimal");
    assert_eq!(info["namespace"], "minimal");
    assert_eq!(info["targets"], json!(["addon", "dev"]));
    assert_eq!(info["trust"]["trusted"], false);
    assert_eq!(info["trust"]["source"], "none");
    assert!(!m.root.join(".beacon").exists(), "check wrote state");
    assert!(!m.root.join("packs").exists());
}

#[test]
fn trust_comes_from_the_command_line_only() {
    let mut m = Mcp::start(&["--trust"]);
    let info = json_text(&m.tool("project_info", json!({})));
    assert_eq!(info["trust"], json!({ "trusted": true, "source": "flag" }));
    // a bad grant list is a usage error before anything starts
    let dir = tempfile::tempdir().unwrap();
    let o = beacon(dir.path(), &m.root, &["mcp", "--allow", "all"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown grant"));
}

#[test]
fn check_since_returns_the_delta() {
    let mut m = Mcp::start(&[]);
    let r = m.tool("check", json!({}));
    let meta: Value =
        serde_json::from_str(r["result"]["content"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["delta"], false);
    let cursor = meta["cursor"].as_str().unwrap().to_owned();
    // nothing changed: an empty delta against that cursor
    let r = m.tool("check", json!({ "since": cursor }));
    let meta: Value =
        serde_json::from_str(r["result"]["content"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["delta"], true);
    assert_eq!(meta["new"], 0);
    let sarif: Value = serde_json::from_str(first_text(&r)).unwrap();
    assert_eq!(sarif["runs"][0]["results"].as_array().unwrap().len(), 0);
    // an unknown cursor gives the full result
    let r = m.tool("check", json!({ "since": "nope" }));
    let meta: Value =
        serde_json::from_str(r["result"]["content"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["delta"], false);
}

#[test]
fn explain_known_and_unknown() {
    let mut m = Mcp::start(&[]);
    let r = m.tool("explain", json!({ "code": "BCN0050" }));
    assert_eq!(r["result"]["isError"], false);
    assert!(first_text(&r).starts_with("BCN0050"));
    let r = m.tool("explain", json!({ "code": "BCN9999" }));
    assert_eq!(r["result"]["isError"], true);
    let r = m.tool("nope", json!({}));
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn bad_input_keeps_serving() {
    let mut m = Mcp::start(&[]);
    assert_eq!(m.raw(b"{not json")["error"]["code"], -32700);
    assert_eq!(m.raw(b"[1]")["error"]["code"], -32600);
    assert_eq!(m.req("bogus/method", json!({}))["error"]["code"], -32601);
    let big = vec![b'a'; 2 << 20];
    let r = m.raw(&big);
    assert_eq!(r["error"]["code"], -32600);
    assert_eq!(m.req("ping", json!({}))["result"], json!({}));
}

#[test]
fn symbols_tools() {
    let mut m = Mcp::start(&[]);
    let r = json_text(&m.tool(
        "symbols_find",
        json!({ "query": "ghost", "kind": "entity" }),
    ));
    let s = &r["symbols"][0];
    assert_eq!(
        (s["name"].as_str(), s["kind"].as_str()),
        (Some("minimal:ghost"), Some("entity"))
    );
    let r = json_text(&m.tool("symbols_definition", json!({ "name": "minimal:ghost" })));
    let defs = &r["definitions"][0]["definitions"];
    assert!(
        defs[0]["path"]
            .as_str()
            .unwrap()
            .ends_with("ghost.entity.json")
    );
    assert!(defs[0]["line"].as_u64().unwrap() > 0);
    let r = json_text(&m.tool(
        "symbols_references",
        json!({ "name": "minimal:ghost", "limit": 1 }),
    ));
    assert!(r["total"].as_u64().unwrap() >= 2, "{r}");
    assert_eq!(r["references"].as_array().unwrap().len(), 1);
    assert_eq!(r["nextCursor"], "1");
    let r = json_text(&m.tool(
        "symbols_references",
        json!({ "name": "minimal:ghost", "limit": 1, "cursor": "1" }),
    ));
    assert!(r["references"][0]["line"].as_u64().unwrap() > 0);
    let r = m.tool("symbols_find", json!({ "kind": "nonsense" }));
    assert_eq!(r["result"]["isError"], true);
}

#[test]
fn index_and_debug_tools_never_download() {
    let mut m = Mcp::start(&[]);
    // no API index in this BEACON_HOME: a note, not an error
    for (tool, args) in [
        ("api_lookup", json!({ "query": "World" })),
        ("schema_lookup", json!({ "kind": "entity" })),
    ] {
        let r = m.tool(tool, args);
        assert_eq!(r["result"]["isError"], false, "{tool}: {r}");
        assert!(first_text(&r).contains("BCN0150"), "{tool}: {r}");
    }
    assert!(!m.dir.path().join("home").join("api").exists());
    // debug_config answers with a configuration or a diagnostic, never a JSON-RPC error
    let r = m.tool("debug_config", json!({}));
    assert!(r["result"]["content"][0]["text"].is_string(), "{r}");
}

/// A call on a tool of `TOOLS`, with the arguments that make it do something real.
fn calls() -> Vec<(&'static str, Value)> {
    vec![
        ("check", json!({})),
        ("project_info", json!({})),
        ("explain", json!({ "code": "BCN0050" })),
        ("symbols_find", json!({})),
        ("symbols_definition", json!({ "name": "minimal:ghost" })),
        ("symbols_references", json!({ "name": "minimal:ghost" })),
        ("schema_lookup", json!({})),
        ("api_lookup", json!({ "query": "World" })),
        ("debug_config", json!({})),
        (
            "rename_symbol",
            json!({ "from": "minimal:ghost", "to": "minimal:spirit" }),
        ),
        ("fix", json!({})),
        ("add_content", json!({ "kind": "item", "name": "gem" })),
        ("build", json!({})),
        ("deploy", json!({})),
        ("pack", json!({})),
    ]
}

#[test]
fn all_fifteen_tools_respond() {
    let mut m = Mcp::start(&[]);
    let list = m.req("tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let calls = calls();
    assert_eq!(names.len(), calls.len());
    for (tool, args) in calls {
        assert!(names.contains(&tool), "{tool} is not listed");
        let r = m.tool(tool, args);
        assert!(r["error"].is_null(), "{tool}: {r}");
        assert!(r["result"]["content"][0]["text"].is_string(), "{tool}: {r}");
    }
}

#[test]
fn writes_need_their_grant_and_dry_runs_write_nothing() {
    let mut m = Mcp::start(&[]);
    let before = m.snapshot();
    // dryRun (the default) needs no grant and writes nothing
    for (tool, args) in calls().into_iter().filter(|(t, _)| {
        [
            "rename_symbol",
            "fix",
            "add_content",
            "build",
            "deploy",
            "pack",
        ]
        .contains(t)
    }) {
        let r = m.tool(tool, args.clone());
        assert!(r["error"].is_null(), "{tool}: {r}");
        let mut explicit = args;
        explicit["dryRun"] = json!(true);
        let r = m.tool(tool, explicit);
        assert!(!first_text(&r).contains("BCN0162"), "{tool}: {r}");
    }
    assert_eq!(m.snapshot(), before, "a dry run wrote");
    // dryRun: false without the grant names the flag
    for (tool, flag) in [
        ("rename_symbol", "edit"),
        ("fix", "edit"),
        ("add_content", "edit"),
        ("build", "build"),
        ("deploy", "deploy"),
        ("pack", "release"),
    ] {
        let mut args = calls().into_iter().find(|c| c.0 == tool).unwrap().1;
        args["dryRun"] = json!(false);
        let r = m.tool(tool, args);
        assert_eq!(r["result"]["isError"], true, "{tool}: {r}");
        let t = first_text(&r);
        assert!(t.contains("BCN0162"), "{tool}: {t}");
        assert!(t.contains(&format!("--allow {flag}")), "{tool}: {t}");
    }
    // a grant for one thing is not a grant for another
    assert_eq!(m.snapshot(), before, "a refused write wrote");
}

#[test]
fn rename_and_add_with_the_edit_grant() {
    let mut m = Mcp::start(&["--allow", "edit"]);
    let plan = json_text(&m.tool(
        "rename_symbol",
        json!({ "from": "minimal:ghost", "to": "minimal:spirit" }),
    ));
    assert_eq!(plan["dryRun"], true);
    assert!(plan["edits"].as_array().unwrap().len() >= 3, "{plan}");
    let entity = m.root.join("src/demo/entities/ghost.entity.json");
    assert!(
        std::fs::read_to_string(&entity)
            .unwrap()
            .contains("minimal:ghost")
    );
    let done = json_text(&m.tool(
        "rename_symbol",
        json!({ "from": "minimal:ghost", "to": "minimal:spirit", "dryRun": false }),
    ));
    assert_eq!(done["applied"], true, "{done}");
    let text = std::fs::read_to_string(&entity).unwrap();
    assert!(text.contains("minimal:spirit") && !text.contains("minimal:ghost"));
    // the daemon's table is fresh right away
    let r = json_text(&m.tool(
        "symbols_find",
        json!({ "query": "spirit", "kind": "entity" }),
    ));
    assert_eq!(r["symbols"][0]["name"], "minimal:spirit");
    // a rename that cannot apply is a diagnostic, nothing written
    let r = m.tool(
        "rename_symbol",
        json!({ "from": "minimal:spirit", "to": "minecraft:zombie", "dryRun": false }),
    );
    assert_eq!(r["result"]["isError"], true);
    assert!(first_text(&r).contains("BCN0143"));
    // add_content: the plan, then the files
    let r = json_text(&m.tool("add_content", json!({ "kind": "item", "name": "gem" })));
    assert!(r["files"].as_array().unwrap().len() >= 2);
    let gem = m.root.join("src/demo/items/gem.item.json");
    assert!(!gem.exists());
    let r = json_text(&m.tool(
        "add_content",
        json!({ "kind": "item", "name": "gem", "dryRun": false }),
    ));
    assert_eq!(r["dryRun"], false);
    assert!(gem.exists());
    // fix has nothing to apply on a fresh project
    let r = json_text(&m.tool("fix", json!({ "dryRun": false })));
    assert_eq!(r["applied"], 0);
    // the edit grant does not cover builds
    let r = m.tool("build", json!({ "dryRun": false }));
    assert!(first_text(&r).contains("--allow build"));
}

#[test]
fn serve_control_commands() {
    let mut m = Mcp::start(&[]);
    assert_eq!(m.req("ping", json!({}))["result"], json!({}));
    let o = m.serve(&["--status"]);
    assert_eq!(o.status.code(), Some(0));
    let st: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(st["clients"], 1);
    assert_eq!(st["trustedClients"], 0);
    // a client is attached: a plain stop is refused
    assert_eq!(m.serve(&["--stop"]).status.code(), Some(1));
    assert_eq!(m.serve(&["--stop", "--force"]).status.code(), Some(0));
    // the shim notices that the daemon went away
    assert!(m.child.wait().is_ok());
    let o = m.serve(&["--status"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        m.serve(&["--stop"]).status.code(),
        Some(0),
        "stopping nothing is fine"
    );
}

#[test]
fn project_local_agent_config_is_protected_from_agents() {
    let m = Mcp::start(&[]);
    let hook = |file: &str, tool: &str| {
        let input = match tool {
            "Write" => json!({ "file_path": file, "content": "{}" }),
            _ => json!({ "file_path": file, "old_string": "a", "new_string": "b" }),
        };
        let ev = json!({ "hook_event_name": "PreToolUse", "tool_name": tool,
            "tool_input": input, "cwd": m.root });
        let mut c = beacon(m.dir.path(), &m.root, &["hook", "pre-tool-use"])
            .current_dir(&m.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        c.stdin
            .take()
            .unwrap()
            .write_all(ev.to_string().as_bytes())
            .unwrap();
        let o = c.wait_with_output().unwrap();
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    let denied = |v: &Value| v["hookSpecificOutput"]["permissionDecision"] == "deny";
    for f in [
        ".mcp.json",
        ".claude/settings.json",
        ".claude/settings.local.json",
        ".vscode/mcp.json",
        "./.CLAUDE/Settings.json",
        "src/../.mcp.json",
    ] {
        for tool in ["Write", "Edit"] {
            let v = hook(f, tool);
            assert!(denied(&v), "{tool} {f}: {v}");
            let why = v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap();
            assert!(why.contains("--allow") || why.contains("grant"), "{why}");
        }
    }
    // other files in those folders, and ordinary sources, stay editable
    for f in [
        ".claude/commands/x.md",
        ".vscode/launch.json",
        "src/demo/items/ruby.item.json",
    ] {
        assert!(!denied(&hook(f, "Edit")), "{f}");
    }
}
