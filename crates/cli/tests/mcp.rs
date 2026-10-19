//! `beacon mcp`: the real binary over pipes.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    tx: ChildStdin,
    rx: BufReader<ChildStdout>,
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
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

impl Mcp {
    fn start() -> Mcp {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("proj");
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/minimal"),
            &root,
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_beacon"))
            .args(["--project", root.to_str().unwrap(), "mcp"])
            .env("BEACON_HOME", dir.path().join("home"))
            .env("BEACON_ESBUILD", "no-esbuild-here")
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
            _dir: dir,
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
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn first_text(r: &Value) -> &str {
    r["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn initialize_and_list() {
    let mut m = Mcp::start();
    let r = m.req("initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    let r = m.req("initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    // a notification gets no response: the next reply belongs to the next request
    m.tx.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .unwrap();
    assert_eq!(m.req("ping", json!({}))["result"], json!({}));
    let r = m.req("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    for t in tools {
        let a = &t["annotations"];
        assert_eq!(a["readOnlyHint"], true);
        assert_eq!(a["destructiveHint"], false);
        assert_eq!(a["openWorldHint"], false);
        assert!(t["description"].as_str().unwrap().contains("project data"));
    }
}

#[test]
fn check_is_restricted_and_writes_nothing() {
    let mut m = Mcp::start();
    // client-supplied trust is ignored
    let r = m.tool("check", json!({ "trust": true }));
    assert_eq!(r["result"]["isError"], false);
    let sarif: Value = serde_json::from_str(first_text(&r)).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    assert!(first_text(&r).contains("BCN0050"));
    assert!(first_text(&r).contains("restricted mode"));
    let info: Value = serde_json::from_str(first_text(
        &m.tool("project_info", json!({ "trust": true })),
    ))
    .unwrap();
    assert_eq!(info["name"], "beacon-minimal");
    assert_eq!(info["namespace"], "minimal");
    assert_eq!(info["targets"], json!(["addon", "dev"]));
    assert_eq!(info["trust"]["trusted"], false);
    assert!(!m.root.join(".beacon").exists(), "check wrote state");
    assert!(!m.root.join("packs").exists());
}

#[test]
fn explain_known_and_unknown() {
    let mut m = Mcp::start();
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
    let mut m = Mcp::start();
    assert_eq!(m.raw(b"{not json")["error"]["code"], -32700);
    assert_eq!(m.raw(b"[1]")["error"]["code"], -32600);
    assert_eq!(m.req("bogus/method", json!({}))["error"]["code"], -32601);
    let big = vec![b'a'; 2 << 20];
    let r = m.raw(&big);
    assert_eq!(r["error"]["code"], -32600);
    assert_eq!(m.req("ping", json!({}))["result"], json!({}));
}
