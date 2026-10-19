//! LSP channel (6.4): JSON content, `.lang`, mcfunction, Molang strings, `module.json`, and
//! cross-file string-literal identifiers (also inside `.ts`, never TypeScript semantics).
//!
//! Crate: `lsp-server` + `lsp-types` (rust-analyzer's), not `tower-lsp`. Measured on a
//! minimal stdio server (Windows, release, stripped): 1.0 MB vs 2.8 MB for
//! `tower-lsp-server` 0.23 + tokio; no async runtime or extra worker threads in a resident
//! process with a 100 MB idle budget (12); the codebase is std-thread + channels throughout
//! (`watch`, `mcp`). `tower-lsp` itself is unmaintained since 2023.
//!
//! Layout: `lsp.rs` (capabilities, the dispatch loop over an `lsp_server::Connection` built on
//! the pipe, document sync with versions), `lsp/convert.rs` (byte offsets <-> LSP positions in
//! UTF-16, `file:` URIs, `diag::Diagnostic` -> LSP diagnostic), `lsp/features.rs` (definition,
//! references, prepareRename + rename, completion, hover, diagnostics; plain functions run on
//! the engine thread).
//!
//! Open documents live in the connection (version + text) and as overlays in the session's
//! symbol table (`SymbolTable::overlay`, kept across watcher re-checks until `didClose` or the
//! connection ends). Diagnostics are pushed (`publishDiagnostics`) 150 ms after the last
//! change; rename returns a `WorkspaceEdit` built from `symbols::rename` (open files from
//! their buffers, other files verified against the disk like `symbols::apply`) and is applied
//! by the editor. Ceiling: a `check` reads the disk, so non-symbol diagnostics of an unsaved
//! buffer lag until it is saved.

use crate::daemon::{EngineHandle, EngineState};
use beacon_engine::path::RelPath;
use features::{Doc, Docs};
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    CompletionOptions, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, HoverProviderCapability, InitializeParams, OneOf, RenameOptions,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Uri,
    WorkDoneProgressOptions,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod convert;
pub(crate) mod features;

pub use convert::{offset, offset_at, path_to_uri, position, position_at, uri_to_path};

/// How long a burst of edits must be quiet before diagnostics are recomputed and pushed.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// The phase-4 subset: diagnostics (push), definition, references, rename (with prepare),
/// completion (ids), hover. Full-document sync: files are small and incremental sync is not
/// worth its bugs yet.
pub fn capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![":".into(), "\"".into(), ".".into()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

/// Serves one LSP client on `conn` (after the handshake) until it disconnects or sends
/// `exit`. Engine access only through `handle` ([`crate::daemon::EngineHandle::call`]); the
/// session is the connection's trust level (`handle.trusted()`), so an untrusted client gets
/// restricted checks (no generators).
pub fn serve(conn: crate::transport::Stream, handle: EngineHandle) {
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let (in_tx, in_rx) = crossbeam_channel::unbounded::<Message>();
    let (out_tx, out_rx) = crossbeam_channel::unbounded::<Message>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(conn);
        while let Ok(Some(m)) = Message::read(&mut r) {
            let exit = matches!(&m, Message::Notification(n) if n.method == "exit");
            if in_tx.send(m).is_err() || exit {
                break;
            }
        }
    });
    std::thread::spawn(move || {
        for m in out_rx {
            if m.write(&mut out).is_err() {
                break;
            }
        }
    });
    run(
        &Connection {
            sender: out_tx,
            receiver: in_rx,
        },
        &handle,
    );
}

struct Server<'a> {
    conn: &'a Connection,
    handle: &'a EngineHandle,
    docs: Docs,
    /// Files that carry diagnostics in the client.
    published: BTreeSet<RelPath>,
    /// Use `documentChanges` in rename results (the client said it can).
    document_changes: bool,
    root: PathBuf,
}

/// Closes the overlays of the open documents however the connection ends.
/// Each file maps to the trust (session) its overlay lives in.
struct CloseDocs<'a>(&'a EngineHandle, BTreeMap<RelPath, bool>);

impl Drop for CloseDocs<'_> {
    fn drop(&mut self) {
        let open = std::mem::take(&mut self.1);
        for t in [false, true] {
            let rels: Vec<RelPath> = open
                .iter()
                .filter(|&(_, &o)| o == t)
                .map(|(p, _)| p.clone())
                .collect();
            if rels.is_empty() {
                continue;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.0
                    .call(move |st| features::close_overlays(st, t, &rels));
            }));
        }
    }
}

fn run(conn: &Connection, handle: &EngineHandle) {
    let Ok((id, params)) = conn.initialize_start() else {
        return;
    };
    let init: InitializeParams = serde_json::from_value(params).unwrap_or_default();
    let document_changes = init
        .capabilities
        .workspace
        .and_then(|w| w.workspace_edit)
        .and_then(|e| e.document_changes)
        .unwrap_or(false);
    let reply = json!({
        "capabilities": capabilities(),
        "serverInfo": { "name": "beacon", "version": crate::ENGINE_VERSION },
    });
    if conn.initialize_finish(id, reply).is_err() {
        return;
    }
    let mut s = Server {
        conn,
        handle,
        docs: Docs::new(),
        published: BTreeSet::new(),
        document_changes,
        root: handle.call(|st| st.root().to_path_buf()),
    };
    // cleanup covers every file ever overlaid, whatever happens to `docs`
    let mut cleanup = CloseDocs(handle, BTreeMap::new());
    let mut dirty: Option<Instant> = None;
    let mut shutdown = false;
    loop {
        let msg = match dirty {
            Some(at) => match conn.receiver.recv_deadline(at) {
                Ok(m) => Some(m),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            },
            None => match conn.receiver.recv() {
                Ok(m) => Some(m),
                Err(_) => break,
            },
        };
        let Some(msg) = msg else {
            dirty = None;
            s.publish();
            continue;
        };
        match msg {
            Message::Request(r) => {
                let resp = if shutdown {
                    fail(
                        r.id,
                        features::Fail(-32600, "the server is shutting down".into()),
                    )
                } else if r.method == "shutdown" {
                    shutdown = true;
                    Response::new_ok(r.id, Value::Null)
                } else {
                    s.request(r)
                };
                if conn.sender.send(resp.into()).is_err() {
                    break;
                }
            }
            Message::Notification(n) => match n.method.as_str() {
                "exit" => break,
                "initialized" => dirty = Some(Instant::now()),
                _ => {
                    if s.notification(n, &mut cleanup.1) {
                        dirty = Some(Instant::now() + DEBOUNCE);
                    }
                }
            },
            Message::Response(_) => {}
        }
    }
}

fn fail(id: RequestId, f: features::Fail) -> Response {
    Response::new_err(id, f.0, f.1)
}

fn parse<P: serde::de::DeserializeOwned>(v: Value) -> Result<P, features::Fail> {
    serde_json::from_value(v).map_err(|e| features::Fail(features::INVALID_PARAMS, e.to_string()))
}

impl Server<'_> {
    /// Runs `f` on the engine thread with a snapshot of the open documents.
    fn engine<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut EngineState, bool, &Docs) -> R + Send + 'static,
    ) -> Result<R, features::Fail> {
        let (docs, t) = (self.docs.clone(), self.handle.trusted());
        catch_unwind(AssertUnwindSafe(|| {
            self.handle.call(move |st| f(st, t, &docs))
        }))
        .map_err(|_| features::Fail(-32603, "internal error in the engine".into()))
    }

    fn request(&self, r: Request) -> Response {
        let dc = self.document_changes;
        let res = match r.method.as_str() {
            "textDocument/definition" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::definition(st, t, d, p))),
            "textDocument/references" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::references(st, t, d, p))),
            "textDocument/prepareRename" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::prepare_rename(st, t, d, p))),
            "textDocument/rename" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::rename(st, t, d, dc, p))),
            "textDocument/hover" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::hover(st, t, d, p))),
            "textDocument/completion" => parse(r.params)
                .and_then(|p| self.engine(move |st, t, d| features::completion(st, t, d, p))),
            m => Err(features::Fail(-32601, format!("method not supported: {m}"))),
        };
        match res.and_then(|r| r) {
            Ok(v) => Response::new_ok(r.id, v),
            Err(f) => fail(r.id, f),
        }
    }

    /// Document sync. Returns whether diagnostics are stale. `opened` collects every file
    /// ever overlaid, for cleanup.
    fn notification(&mut self, n: Notification, opened: &mut BTreeMap<RelPath, bool>) -> bool {
        match n.method.as_str() {
            "textDocument/didOpen" => {
                let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(n.params) else {
                    return false;
                };
                let d = p.text_document;
                self.set(&d.uri, d.version, d.text, opened)
            }
            "textDocument/didChange" => {
                let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(n.params) else {
                    return false;
                };
                // full sync: the last change is the whole text; a ranged one is not ours, so
                // the buffer is unknown: dirty (writes refuse it) until a full text or close
                match p.content_changes.into_iter().last() {
                    Some(c) if c.range.is_none() => self.set(
                        &p.text_document.uri,
                        p.text_document.version,
                        c.text,
                        opened,
                    ),
                    _ => {
                        if let Some(rel) = convert::rel_of(&self.root, &p.text_document.uri)
                            && self.docs.contains_key(&rel)
                        {
                            let _ = catch_unwind(AssertUnwindSafe(|| {
                                self.handle.call(move |st| st.set_open_unknown(&rel));
                            }));
                        }
                        false
                    }
                }
            }
            "textDocument/didClose" => {
                let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(n.params) else {
                    return false;
                };
                let Some(rel) = convert::rel_of(&self.root, &p.text_document.uri) else {
                    return false;
                };
                if self.docs.remove(&rel).is_none() {
                    return false;
                }
                // the session the overlay was opened in, whatever trust is now
                let t = opened.remove(&rel).unwrap_or_else(|| self.handle.trusted());
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    self.handle
                        .call(move |st| features::close_overlays(st, t, &[rel]));
                }));
                true
            }
            _ => false,
        }
    }

    fn set(
        &mut self,
        uri: &Uri,
        version: i32,
        text: String,
        opened: &mut BTreeMap<RelPath, bool>,
    ) -> bool {
        let Some(rel) = convert::rel_of(&self.root, uri) else {
            return false; // outside the project
        };
        let text = Arc::new(text);
        self.docs.insert(
            rel.clone(),
            Doc {
                version,
                text: text.clone(),
            },
        );
        let t = self.handle.trusted();
        // trust changed since it was opened: the old session's overlay must go
        if let Some(old) = opened.insert(rel.clone(), t).filter(|&o| o != t) {
            let r = rel.clone();
            let _ = catch_unwind(AssertUnwindSafe(|| {
                self.handle
                    .call(move |st| features::close_overlays(st, old, &[r]));
            }));
        }
        catch_unwind(AssertUnwindSafe(|| {
            self.handle
                .call(move |st| features::set_overlay(st, t, &rel, &text));
        }))
        .is_ok()
    }

    /// Checks and pushes `publishDiagnostics` for every file that has some (and an empty
    /// list for files that had some before).
    fn publish(&mut self) {
        let published = self.published.clone();
        let Ok(by_file) = self.engine(move |st, t, d| features::diagnostics(st, t, d, &published))
        else {
            return;
        };
        self.published = by_file
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(p, _)| p.clone())
            .collect();
        for (rel, diagnostics) in by_file {
            let uri = convert::uri_of(&self.root, &rel);
            let n = Notification::new(
                "textDocument/publishDiagnostics".into(),
                json!({ "uri": uri, "diagnostics": diagnostics }),
            );
            if self.conn.sender.send(n.into()).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset() {
        let c = serde_json::to_value(capabilities()).unwrap();
        assert_eq!(c["renameProvider"]["prepareProvider"], true);
        assert_eq!(c["definitionProvider"], true);
        assert_eq!(c["referencesProvider"], true);
        assert_eq!(c["hoverProvider"], true);
        assert_eq!(c["textDocumentSync"], 1);
        assert!(c.get("semanticTokensProvider").is_none());
    }
}
