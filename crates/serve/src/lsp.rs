//! LSP channel (6.4): JSON content, `.lang`, mcfunction, Molang strings, `module.json`, and
//! cross-file string-literal identifiers (also inside `.ts`, never TypeScript semantics).
//!
//! Crate: `lsp-server` + `lsp-types` (rust-analyzer's), not `tower-lsp`. Measured on a
//! minimal stdio server (Windows, release, stripped): 1.0 MB vs 2.8 MB for
//! `tower-lsp-server` 0.23 + tokio; no async runtime or extra worker threads in a resident
//! process with a 100 MB idle budget (12); the codebase is std-thread + channels throughout
//! (`watch`, `mcp`). `tower-lsp` itself is unmaintained since 2023.
//!
//! Layout (one file each when implemented): `lsp.rs` (capabilities, dispatch loop over an
//! `lsp_server::Connection` built on the pipe), `lsp/convert.rs` (byte offsets <-> LSP
//! positions in UTF-16, `diag::Diagnostic` -> LSP diagnostic, `diag::Fix` -> `TextEdit` /
//! code action), `lsp/docs.rs` (open-document overlays with versions), `lsp/features.rs`
//! (definition, references, prepareRename + rename, completion, hover).
//!
//! Diagnostics are pushed (`publishDiagnostics`) after each check; rename returns a
//! `WorkspaceEdit` built from `symbols::rename` and is applied by the editor (so dirty
//! buffers are the editor's business, 3).

use lsp_types::{
    CompletionOptions, HoverProviderCapability, OneOf, RenameOptions, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, WorkDoneProgressOptions,
};

/// The phase-4 subset: diagnostics (push), definition, references, rename (with prepare),
/// completion (ids, lang keys, component names), hover. Full-document sync: files are small
/// and incremental sync is not worth its bugs yet.
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

/// Serves one LSP client on `conn` (after the handshake) until it disconnects. Engine access
/// only through `handle` ([`crate::daemon::EngineHandle::call`]).
///
/// Wave-1 stub: drains the connection, so the client counts as attached. Phase 4 wave 2
/// replaces the body (an `lsp_server::Connection` over `conn`).
pub fn serve(conn: crate::transport::Stream, handle: crate::daemon::EngineHandle) {
    let _ = handle;
    let _ = std::io::copy(&mut { conn }, &mut std::io::sink());
}

/// 0-based (line, UTF-16 column) of byte `offset` in `text` (clamped to the end).
pub fn position(text: &str, offset: u32) -> lsp_types::Position {
    let _ = (text, offset);
    todo!("phase 4 wave 2: lsp")
}

/// Byte offset of an LSP position (clamped to the line end / text end).
pub fn offset(text: &str, pos: lsp_types::Position) -> u32 {
    let _ = (text, pos);
    todo!("phase 4 wave 2: lsp")
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
