//! P4-2A: the LSP channel end to end. A real daemon (in a thread of this process) over its
//! real pipe/socket, a hand-written client speaking LSP, on a copy of `fixtures/rename`.
//! Untrusted connection: restricted sessions, no generators (what the CLI's `rename` sees).

use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use beacon_serve::discovery::{self, Discovery};
use beacon_serve::lsp::{offset, path_to_uri, position, uri_to_path};
use beacon_serve::{ENGINE_VERSION, PROTOCOL_VERSION, daemon, transport};
use lsp_server::{Message, Notification, Request, RequestId};
use serde_json::{Value, json};
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(60);

struct NoHost;

impl GeneratorHost for NoHost {
    fn run(&self, _: &Generator, _: &GenCtx, _: &dyn Vfs) -> GenResult {
        GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        }
    }
    fn version(&self) -> String {
        "none".into()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

/// Every file below `dir` except the engine's state dir: (relative path, bytes), sorted.
fn tree(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with(".beacon") {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                out.push((rel, fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn copy_fixture() -> (tempfile::TempDir, PathBuf) {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws.path().join("rename");
    for (rel, bytes) in tree(&fixtures().join("rename")) {
        let p = dir.join(&rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }
    (ws, dir)
}

struct Client {
    w: transport::Stream,
    rx: Receiver<Message>,
    next: i32,
    /// The canonical project root (what the daemon reports).
    root: PathBuf,
    pipe: String,
    _keep: (tempfile::TempDir, tempfile::TempDir),
}

impl Client {
    fn start(document_changes: bool) -> Client {
        Client::start_as(document_changes, false)
    }

    fn start_as(document_changes: bool, trust: bool) -> Client {
        let (ws, dir) = copy_fixture();
        let run = tempfile::tempdir().unwrap();
        let opts = daemon::DaemonOptions {
            project: dir.clone(),
            idle: Duration::from_secs(3600),
            run: run.path().into(),
        };
        std::thread::spawn(move || {
            daemon::run(
                &opts,
                Box::new(|root, o: BuildOptions| Session::new(root, o, Box::new(NoHost), None)),
            )
        });
        let key = discovery::project_key(&dir);
        let path = discovery::discovery_path(run.path(), &key);
        let t0 = Instant::now();
        let disc = loop {
            if let Some(d) = Discovery::read(&path) {
                break d;
            }
            assert!(t0.elapsed() < WAIT, "daemon did not start");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut s = transport::connect(&disc.pipe).expect("connect");
        let hello = json!({ "beacon": PROTOCOL_VERSION, "engine": ENGINE_VERSION, "channel": "lsp", "trust": trust });
        s.write_all(format!("{hello}\n").as_bytes()).unwrap();
        let mut reply = Vec::new();
        let mut b = [0u8];
        while s.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
            reply.push(b[0]);
        }
        let reply: Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        let (tx, rx) = mpsc::channel();
        let mut r = BufReader::new(s.try_clone().unwrap());
        std::thread::spawn(move || {
            while let Ok(Some(m)) = Message::read(&mut r) {
                if tx.send(m).is_err() {
                    break;
                }
            }
        });
        let mut c = Client {
            w: s,
            rx,
            next: 0,
            root: PathBuf::from(&disc.project),
            pipe: disc.pipe.clone(),
            _keep: (ws, run),
        };
        let caps =
            json!({ "workspace": { "workspaceEdit": { "documentChanges": document_changes } } });
        let init = c.request(
            "initialize",
            json!({ "capabilities": caps, "processId": null, "rootUri": null }),
        );
        assert_eq!(
            init["capabilities"]["renameProvider"]["prepareProvider"],
            true
        );
        c.notify("initialized", json!({}));
        c
    }

    fn send(&mut self, m: Message) {
        m.write(&mut self.w).unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(Notification::new(method.into(), params).into());
    }

    /// The raw result or error object.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, (i32, String)> {
        self.next += 1;
        let id = RequestId::from(self.next);
        self.send(Request::new(id.clone(), method.into(), params).into());
        let t0 = Instant::now();
        loop {
            let left = WAIT.saturating_sub(t0.elapsed());
            match self.rx.recv_timeout(left).expect("no response") {
                Message::Response(r) if r.id == id => {
                    return match r.response_result {
                        Err(e) => Err((e.code, e.message)),
                        Ok(v) => Ok(v),
                    };
                }
                _ => {}
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.call(method, params)
            .unwrap_or_else(|e| panic!("{method}: {e:?}"))
    }

    /// Waits for a notification of `method` whose params satisfy `f`.
    fn wait(&mut self, method: &str, f: impl Fn(&Value) -> bool) -> Value {
        let t0 = Instant::now();
        loop {
            let left = WAIT.saturating_sub(t0.elapsed());
            match self.rx.recv_timeout(left) {
                Ok(Message::Notification(n)) if n.method == method && f(&n.params) => {
                    return n.params;
                }
                Ok(_) => {}
                Err(_) => panic!("timed out waiting for {method}"),
            }
        }
    }

    fn uri(&self, rel: &str) -> String {
        path_to_uri(&self.root.join(rel)).as_str().to_owned()
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn open(&mut self, rel: &str, version: i32, text: &str) {
        let doc =
            json!({ "uri": self.uri(rel), "languageId": "x", "version": version, "text": text });
        self.notify("textDocument/didOpen", json!({ "textDocument": doc }));
    }

    fn change(&mut self, rel: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({ "textDocument": { "uri": self.uri(rel), "version": version },
                    "contentChanges": [{ "text": text }] }),
        );
    }

    /// Position of the `nth` (0-based) `needle` in `rel`'s disk text, `delta` bytes in.
    fn at(&self, rel: &str, needle: &str, nth: usize, delta: usize) -> Value {
        let t = self.read(rel);
        let off = t.match_indices(needle).nth(nth).unwrap().0 + delta;
        json!({ "textDocument": { "uri": self.uri(rel) }, "position": position(&t, off as u32) })
    }

    fn references(&mut self, at: Value, declaration: bool) -> Vec<Value> {
        let mut p = at;
        p["context"] = json!({ "includeDeclaration": declaration });
        match self.request("textDocument/references", p) {
            Value::Array(v) => v,
            other => panic!("references: {other}"),
        }
    }

    fn rename(&mut self, at: Value, name: &str) -> Result<Value, (i32, String)> {
        let mut p = at;
        p["newName"] = json!(name);
        self.call("textDocument/rename", p)
    }

    fn shutdown(&mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
    }
}

/// Text under a location, read from `texts` (disk) unless `buffers` has the file.
fn slice_of(root: &Path, loc: &Value, buffers: &[(&str, &str)]) -> (String, String) {
    let path = uri_to_path(&loc["uri"].as_str().unwrap().parse().unwrap()).unwrap();
    let rel = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_lowercase()
        .replace(
            &root.to_string_lossy().replace('\\', "/").to_lowercase(),
            "",
        )
        .trim_start_matches('/')
        .to_owned();
    let text = buffers
        .iter()
        .find(|(r, _)| rel == r.to_lowercase())
        .map_or_else(
            || fs::read_to_string(&path).unwrap(),
            |(_, t)| (*t).to_owned(),
        );
    let o = |p: &Value| {
        offset(
            &text,
            lsp_types::Position {
                line: p["line"].as_u64().unwrap() as u32,
                character: p["character"].as_u64().unwrap() as u32,
            },
        ) as usize
    };
    let (a, b) = (o(&loc["range"]["start"]), o(&loc["range"]["end"]));
    (rel, text[a..b].to_owned())
}

fn apply_edit(edit: &Value) {
    let per_file: Vec<(String, Vec<Value>)> = match edit.get("documentChanges") {
        Some(dc) => dc
            .as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["textDocument"]["uri"].as_str().unwrap().to_owned(),
                    d["edits"].as_array().unwrap().clone(),
                )
            })
            .collect(),
        None => edit["changes"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(u, e)| (u.clone(), e.as_array().unwrap().clone()))
            .collect(),
    };
    for (uri, mut edits) in per_file {
        let path = uri_to_path(&uri.parse().unwrap()).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        let pos = |p: &Value| lsp_types::Position {
            line: p["line"].as_u64().unwrap() as u32,
            character: p["character"].as_u64().unwrap() as u32,
        };
        edits.sort_by_key(|e| {
            let p = pos(&e["range"]["start"]);
            (p.line, p.character)
        });
        for e in edits.iter().rev() {
            let (a, b) = (
                offset(&text, pos(&e["range"]["start"])) as usize,
                offset(&text, pos(&e["range"]["end"])) as usize,
            );
            text.replace_range(a..b, e["newText"].as_str().unwrap());
        }
        fs::write(path, text).unwrap();
    }
}

fn diagnostics_of(c: &mut Client, rel: &str, pred: impl Fn(&[Value]) -> bool) -> Vec<Value> {
    let uri = c.uri(rel);
    let p = c.wait("textDocument/publishDiagnostics", |p| {
        p["uri"] == uri && pred(p["diagnostics"].as_array().unwrap())
    });
    p["diagnostics"].as_array().unwrap().clone()
}

const SPAWN: &str = "src/world/functions/spawn.mcfunction";
const CAR: &str = "src/vehicles/entities/car.entity.json";

#[test]
fn rename_matches_the_cli_expected_tree() {
    for document_changes in [false, true] {
        let mut c = Client::start(document_changes);
        let at = c.at(SPAWN, "demo:car", 0, 2);
        let prep = c.request("textDocument/prepareRename", at.clone());
        assert_eq!(prep["placeholder"], "demo:car");
        let edit = c.rename(at, "demo:truck").unwrap();
        assert_eq!(edit.get("documentChanges").is_some(), document_changes);
        assert_eq!(
            edit.get("changes")
                .or(edit.get("documentChanges"))
                .unwrap()
                .as_array()
                .map_or_else(|| edit["changes"].as_object().unwrap().len(), Vec::len),
            7
        );
        apply_edit(&edit);
        let (got, want) = (tree(&c.root), tree(&fixtures().join("rename.expected")));
        assert_eq!(
            got.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            want.iter().map(|(p, _)| p).collect::<Vec<_>>()
        );
        for ((p, a), (_, b)) in got.iter().zip(&want) {
            assert!(
                a == b,
                "{p} differs:\n{}\n---\n{}",
                String::from_utf8_lossy(a),
                String::from_utf8_lossy(b)
            );
        }
        c.shutdown();
    }
}

#[test]
fn definition_references_hover_completion() {
    let mut c = Client::start(false);
    // from a use in a function to the entity definition
    let def = c.request("textDocument/definition", c.at(SPAWN, "demo:car", 0, 3));
    let def = def.as_array().unwrap();
    assert_eq!(def.len(), 1, "{def:?}");
    assert_eq!(
        slice_of(&c.root, &def[0], &[]),
        (CAR.into(), "demo:car".into())
    );
    // from the definition to every use: 11 occurrences with it, 10 without
    let at = c.at(CAR, "demo:car", 0, 0);
    let all = c.references(at.clone(), true);
    let uses = c.references(at, false);
    assert_eq!((all.len(), uses.len()), (11, 10), "{all:?}");
    let files: std::collections::BTreeSet<_> =
        all.iter().map(|l| slice_of(&c.root, l, &[]).0).collect();
    assert_eq!(files.len(), 7, "{files:?}");
    for l in &all {
        let (rel, text) = slice_of(&c.root, l, &[]);
        // the `${ns}:car` template head covers only its local name
        assert!(
            text == "demo:car" || (text == "car" && rel.ends_with("ids.ts")),
            "{rel}: {text}"
        );
    }
    // hover: kind and counts; nothing in a script
    let h = c.request("textDocument/hover", c.at(SPAWN, "demo:car", 0, 3));
    let md = h["contents"]["value"].as_str().unwrap();
    assert!(md.contains("entity") && md.contains("1 definition"), "{md}");
    let ts = "src/vehicles/scripts/lib/ids.ts";
    let h = c.request("textDocument/hover", c.at(ts, "demo:", 0, 2));
    assert!(h.is_null(), "{h}");
    // completion of an id prefix
    let mut at = c.at(SPAWN, "demo:car", 0, 5);
    let items = c.request("textDocument/completion", at.take());
    let labels: Vec<_> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap())
        .collect();
    assert!(labels.contains(&"demo:car"), "{labels:?}");
    // nothing renameable on plain text
    assert!(
        c.call(
            "textDocument/prepareRename",
            c.at("config.json", "beacon-rename", 0, 1)
        )
        .is_err()
    );
    c.shutdown();
}

#[test]
fn utf16_columns_with_non_ascii_text_before_the_id() {
    let mut c = Client::start(false);
    // 4 + è (1) + emoji (2, 4 bytes) + space = 8 units before the id, 11 bytes... 12 in UTF-8
    let buf = "say è😀 demo:car\n";
    c.open(SPAWN, 1, buf);
    let at = json!({ "textDocument": { "uri": c.uri(CAR) },
        "position": position(&c.read(CAR), c.read(CAR).find("demo:car").unwrap() as u32) });
    let all = c.references(at, true);
    let hit = all
        .iter()
        .find(|l| l["uri"] == c.uri(SPAWN))
        .unwrap_or_else(|| panic!("{all:?}"));
    assert_eq!(hit["range"]["start"], json!({ "line": 0, "character": 8 }));
    assert_eq!(hit["range"]["end"], json!({ "line": 0, "character": 16 }));
    assert_eq!(slice_of(&c.root, hit, &[(SPAWN, buf)]).1, "demo:car");
    // and back: a cursor inside the id (UTF-16 column 10) finds the definition
    let cur = json!({ "textDocument": { "uri": c.uri(SPAWN) }, "position": { "line": 0, "character": 10 } });
    let def = c.request("textDocument/definition", cur);
    assert_eq!(def.as_array().unwrap().len(), 1, "{def}");
    c.shutdown();
}

#[test]
fn open_buffer_overlay_survives_updates_and_ends_on_close() {
    let mut c = Client::start(false);
    let at = c.at(CAR, "demo:car", 0, 0);
    assert_eq!(c.references(at.clone(), false).len(), 10);
    let buf = format!("{}summon demo:car\n", c.read(SPAWN));
    c.open(SPAWN, 1, &buf);
    assert_eq!(c.references(at.clone(), false).len(), 11);
    // a check (what the watcher runs) re-reads the project; the buffer stays
    c.change(SPAWN, 2, &buf);
    diagnostics_of(&mut c, SPAWN, |_| true);
    let after = c.references(at.clone(), false);
    assert_eq!(after.len(), 11, "{after:?}");
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": c.uri(SPAWN) } }),
    );
    // closing rescans from disk at the next check
    c.change(CAR, 1, &c.read(CAR));
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": c.uri(CAR) } }),
    );
    let t0 = Instant::now();
    while c.references(at.clone(), false).len() != 10 {
        assert!(t0.elapsed() < WAIT, "overlay not closed");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.shutdown();
}

#[test]
fn rename_uses_open_buffers_and_reports_versions() {
    let mut c = Client::start(true);
    let buf = format!("{}summon demo:car\n", c.read(SPAWN));
    c.open(SPAWN, 7, &buf);
    let edit = c.rename(c.at(CAR, "demo:car", 0, 1), "demo:truck").unwrap();
    let docs = edit["documentChanges"].as_array().unwrap();
    let spawn = docs
        .iter()
        .find(|d| d["textDocument"]["uri"] == c.uri(SPAWN))
        .unwrap();
    assert_eq!(spawn["textDocument"]["version"], 7);
    assert_eq!(spawn["edits"].as_array().unwrap().len(), 3, "{spawn}");
    let other = docs
        .iter()
        .find(|d| d["textDocument"]["uri"] == c.uri(CAR))
        .unwrap();
    assert!(other["textDocument"]["version"].is_null());
    c.shutdown();
}

#[test]
fn diagnostics_follow_edits_and_clear() {
    // trusted: generators ran (here: none), so an undefined id is an error at its place
    let mut c = Client::start_as(false, true);
    let good = c.read(SPAWN);
    c.open(SPAWN, 1, &good);
    let bad = format!(
        "{good}summon demo:nope
"
    );
    c.change(SPAWN, 2, &bad);
    let d = diagnostics_of(&mut c, SPAWN, |d| !d.is_empty());
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0]["code"], "BCN0140");
    assert_eq!(d[0]["source"], "beacon");
    let line = good.lines().count() as u64;
    assert_eq!(
        d[0]["range"]["start"],
        json!({ "line": line, "character": 7 })
    );
    assert_eq!(
        d[0]["range"]["end"],
        json!({ "line": line, "character": 16 })
    );
    c.change(SPAWN, 3, &good);
    let d = diagnostics_of(&mut c, SPAWN, |d| d.is_empty());
    assert!(d.is_empty());
    c.shutdown();
}

#[test]
fn restricted_connection_folds_unresolved_into_a_note() {
    // untrusted + a project with generators: the id may come from a generator, so no error
    let mut c = Client::start(false);
    let bad = format!(
        "{}summon demo:nope
",
        c.read(SPAWN)
    );
    c.open(SPAWN, 1, &bad);
    let d = diagnostics_of(&mut c, "config.json", |d| {
        d.iter().any(|x| x["code"] == "BCN0140")
    });
    let n = d.iter().find(|x| x["code"] == "BCN0140").unwrap();
    assert_eq!(n["severity"], 3);
    assert!(n["message"].as_str().unwrap().contains("restricted mode"));
    c.shutdown();
}

#[test]
fn blocked_rename_is_an_error_and_edits_nothing() {
    let mut c = Client::start(false);
    let before = tree(&c.root);
    // `demo:lamp` is already defined; `Truck` is not an id; vanilla ids are never renamed
    for (name, msg) in [("demo:lamp", "already defined"), ("Truck", "invalid id")] {
        let (code, m) = c.rename(c.at(CAR, "demo:car", 0, 1), name).unwrap_err();
        assert_eq!(code, -32803);
        assert!(m.contains(msg), "{m}");
    }
    assert!(
        c.rename(c.at(SPAWN, "demo:car", 0, 1), "minecraft:x")
            .is_err()
    );
    assert_eq!(tree(&c.root), before);
    // an unknown method is MethodNotFound, the server keeps serving
    assert_eq!(c.call("workspace/nope", json!({})).unwrap_err().0, -32601);
    c.shutdown();
}

/// One MCP request over its own connection to the same daemon, granted `edit`.
fn mcp_tool(c: &Client, name: &str, args: Value) -> Value {
    let mut s = transport::connect(&c.pipe).expect("connect");
    let hello = json!({ "beacon": PROTOCOL_VERSION, "engine": ENGINE_VERSION, "channel": "mcp", "allow": ["edit"] });
    s.write_all(format!("{hello}\n").as_bytes()).unwrap();
    let line = |s: &mut transport::Stream| {
        let (mut v, mut b) = (Vec::new(), [0u8]);
        while s.read(&mut b).unwrap() == 1 && b[0] != b'\n' {
            v.push(b[0]);
        }
        serde_json::from_slice::<Value>(&v).unwrap()
    };
    assert_eq!(line(&mut s)["ok"], true);
    let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": args } });
    s.write_all(format!("{req}\n").as_bytes()).unwrap();
    line(&mut s)
}

#[test]
fn mcp_write_is_refused_for_a_buffer_open_in_lsp() {
    let mut c = Client::start(false);
    let rel = CAR;
    let args = json!({ "from": "demo:car", "to": "demo:truck", "dryRun": false });
    let before = tree(&c.root);
    // a buffer is open with unsaved text: the file must not be overwritten
    let text = c.read(rel);
    c.open(rel, 1, &format!("{text} "));
    c.change(rel, 2, &format!("{text}  "));
    c.wait("textDocument/publishDiagnostics", |_| true);
    let r = mcp_tool(&c, "rename_symbol", args.clone());
    assert_eq!(r["result"]["isError"], true, "{r}");
    let t = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(t.contains("BCN0163"), "{t}");
    assert_eq!(tree(&c.root), before, "nothing was written");
    // closed again: the same call goes through
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": c.uri(rel) } }),
    );
    let r = mcp_tool(&c, "rename_symbol", args);
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(c.read(rel).contains("demo:truck"));
    c.shutdown();
}

#[test]
fn many_changes_then_close_leave_no_overlay() {
    let mut c = Client::start(false);
    let at = c.at(CAR, "demo:car", 0, 0);
    let base = c.read(SPAWN);
    c.open(SPAWN, 1, &base);
    for v in 2..60 {
        c.change(SPAWN, v, &format!("{base}summon demo:car\n"));
    }
    assert_eq!(c.references(at.clone(), false).len(), 11);
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": c.uri(SPAWN) } }),
    );
    // a check (any open + close) rescans the closed file from disk
    let car = c.read(CAR);
    c.open(CAR, 1, &car);
    c.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": c.uri(CAR) } }),
    );
    let t0 = Instant::now();
    while c.references(at.clone(), false).len() != 10 {
        assert!(t0.elapsed() < WAIT, "overlay left behind");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.shutdown();
}

/// `symbols_references` of a file open with unsaved edits: line/column come from the buffer the
/// table was scanned from, not from the disk text.
#[test]
fn mcp_references_positions_follow_the_open_buffer() {
    let mut c = Client::start(false);
    let client = "src/vehicles/entities/car.client_entity.json";
    let disk = c.read(client);
    // three lines on top: every position in the buffer is 3 lines below the disk's
    let buffer = format!("\n\n\n{disk}");
    c.open(client, 1, &buffer);
    c.wait("textDocument/publishDiagnostics", |_| true);
    let r = mcp_tool(&c, "symbols_references", json!({ "name": "demo:car" }));
    let t = r["result"]["content"][0]["text"].as_str().unwrap();
    let v: Value = serde_json::from_str(t).unwrap();
    let hit = v["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["path"] == client)
        .unwrap_or_else(|| panic!("no hit in {client}: {t}"))
        .clone();
    let off = buffer.find("demo:car").unwrap();
    let line = buffer[..off].matches('\n').count() + 1;
    let col = off - buffer[..off].rfind('\n').map_or(0, |p| p + 1) + 1;
    assert_eq!(
        (hit["line"].as_u64(), hit["column"].as_u64()),
        (Some(line as u64), Some(col as u64)),
        "{t}"
    );
    c.shutdown();
}
