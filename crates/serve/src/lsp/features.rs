//! The LSP features, each a plain function run on the engine thread (`EngineHandle::call`)
//! over the symbol table of the connection's session. Nothing here touches the disk except
//! to read files that are not open (text for UTF-16 columns) and, for rename, the same
//! on-disk range verification `symbols::apply` does.
//!
//! Open documents arrive as a snapshot ([`Docs`]); their text is the overlay the table was
//! scanned from, so ranges and line tables agree.

use super::convert::{line_starts, offset_at, position_at, range, rel_of, uri_of};
use beacon_engine::config::CONFIG_FILE;
use beacon_engine::diag::{Diagnostic, Span};
use beacon_engine::path::RelPath;
use beacon_engine::symbols::{
    self, ByteRange, Occurrence, Role, SymbolKind, SymbolTable, classify,
};
use lsp_types::{
    CompletionParams, GotoDefinitionParams, HoverParams, ReferenceParams, RenameParams,
    TextDocumentPositionParams,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::Arc;

/// An open document: the client's version and its full text.
#[derive(Clone)]
pub struct Doc {
    pub version: i32,
    pub text: Arc<String>,
}

pub type Docs = BTreeMap<RelPath, Doc>;

/// A request failure: JSON-RPC code and message.
pub struct Fail(pub i32, pub String);

pub const INVALID_PARAMS: i32 = -32602;
/// `RequestFailed`: the request was valid but could not be carried out (a blocked rename).
pub const REQUEST_FAILED: i32 = -32803;

pub type Res = Result<Value, Fail>;

const KINDS: [SymbolKind; 10] = [
    SymbolKind::Entity,
    SymbolKind::Item,
    SymbolKind::Block,
    SymbolKind::LangKey,
    SymbolKind::Function,
    SymbolKind::Animation,
    SymbolKind::AnimationController,
    SymbolKind::RenderController,
    SymbolKind::Texture,
    SymbolKind::Sound,
];

fn kind_name(k: SymbolKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Brings the session's table in line (a check when it has none yet) and runs `f` on it.
pub fn with_table<R>(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    f: impl FnOnce(&Path, &SymbolTable) -> R,
) -> R {
    let root = st.root().to_path_buf();
    let s = st.session(trusted);
    if s.symbols().is_none() {
        s.check();
    }
    match s.symbols() {
        Some(t) => f(&root, t),
        None => f(&root, &SymbolTable::default()),
    }
}

/// Opens (or replaces) the overlay of `rel` in the session's table.
pub fn set_overlay(st: &mut crate::daemon::EngineState, trusted: bool, rel: &RelPath, text: &str) {
    with_table(st, trusted, |_, _| ());
    if let Some(t) = st.session(trusted).symbols_mut() {
        t.overlay(rel, text);
    }
}

pub fn close_overlays(st: &mut crate::daemon::EngineState, trusted: bool, rels: &[RelPath]) {
    if let Some(t) = st.session(trusted).symbols_mut() {
        for r in rels {
            t.close_overlay(r);
        }
    }
}

/// Text of `rel`: the open buffer, else the disk.
fn text_of(root: &Path, docs: &Docs, rel: &RelPath) -> Option<Arc<String>> {
    match docs.get(rel) {
        Some(d) => Some(d.text.clone()),
        None => std::fs::read_to_string(root.join(rel.as_str()))
            .ok()
            .map(Arc::new),
    }
}

/// (file, its text, byte offset) under the cursor.
fn cursor(
    root: &Path,
    docs: &Docs,
    p: &TextDocumentPositionParams,
    t: &SymbolTable,
) -> Option<(RelPath, Arc<String>, usize)> {
    let rel = rel_of(root, &p.text_document.uri)?;
    let text = text_of(root, docs, &rel)?;
    let lines = t
        .line_starts(&rel)
        .map_or_else(|| line_starts(&text), <[u32]>::to_vec);
    let off = offset_at(&text, &lines, p.position) as usize;
    Some((rel, text, off))
}

fn range_json(
    root: &Path,
    docs: &Docs,
    t: &SymbolTable,
    rel: &RelPath,
    r: Option<ByteRange>,
) -> Option<Value> {
    let text = text_of(root, docs, rel)?;
    let lines = t.line_starts(rel)?;
    let (a, b) = match r {
        Some(r) => (
            position_at(&text, lines, r.start as u32),
            position_at(&text, lines, r.end as u32),
        ),
        // a definition without text (a function file): the top of the file
        None => Default::default(),
    };
    Some(range(a, b))
}

fn location(
    root: &Path,
    docs: &Docs,
    t: &SymbolTable,
    rel: &RelPath,
    r: Option<ByteRange>,
) -> Option<Value> {
    let range = range_json(root, docs, t, rel, r)?;
    Some(json!({ "uri": uri_of(root, rel), "range": range }))
}

/// The kinds a name resolves as: the occurrence's own, else every kind it is defined as.
fn kinds_of(t: &SymbolTable, o: &Occurrence) -> Vec<SymbolKind> {
    if let Some(k) = o.kind {
        return vec![k];
    }
    let k: Vec<_> = KINDS
        .into_iter()
        .filter(|&k| !t.definitions(k, &o.name).is_empty())
        .collect();
    if k.is_empty() {
        vec![SymbolKind::Entity]
    } else {
        k
    }
}

pub fn definition(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    p: GotoDefinitionParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let Some((rel, _, off)) = cursor(root, docs, &p.text_document_position_params, t) else {
            return Ok(Value::Null);
        };
        let Some(at) = t.at(&rel, off) else {
            return Ok(Value::Null);
        };
        let mut out: Vec<(RelPath, Option<ByteRange>)> = kinds_of(t, at.occ)
            .into_iter()
            .flat_map(|k| t.definitions(k, &at.occ.name))
            .map(|l| (l.path.clone(), l.occ.range))
            .collect();
        out.sort();
        out.dedup();
        Ok(Value::Array(
            out.iter()
                .filter_map(|(p, r)| location(root, docs, t, p, *r))
                .collect(),
        ))
    })
}

pub fn references(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    p: ReferenceParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let Some((rel, _, off)) = cursor(root, docs, &p.text_document_position, t) else {
            return Ok(Value::Null);
        };
        let Some(at) = t.at(&rel, off) else {
            return Ok(Value::Null);
        };
        let mut hits = BTreeSet::new();
        for k in kinds_of(t, at.occ) {
            for l in t.references(k, &at.occ.name) {
                hits.insert((l.path.clone(), l.occ.range));
            }
            if p.context.include_declaration {
                for l in t.definitions(k, &at.occ.name) {
                    hits.insert((l.path.clone(), l.occ.range));
                }
            }
        }
        Ok(Value::Array(
            hits.iter()
                .filter(|(_, r)| r.is_some())
                .filter_map(|(p, r)| location(root, docs, t, p, *r))
                .collect(),
        ))
    })
}

/// A namespaced id that rename can move: `ns:name`, not vanilla, not a lang key.
fn renameable(o: &Occurrence) -> bool {
    o.range.is_some()
        && o.kind != Some(SymbolKind::LangKey)
        && o.name.contains(':')
        && !o.name.starts_with("minecraft:")
        && !o.name.contains(char::is_whitespace)
}

pub fn prepare_rename(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    p: TextDocumentPositionParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let at = cursor(root, docs, &p, t).and_then(|(rel, text, off)| {
            let l = t.at(&rel, off)?;
            Some((rel, text, l.occ.clone()))
        });
        let Some((rel, text, o)) = at.filter(|(_, _, o)| renameable(o)) else {
            return Err(Fail(
                REQUEST_FAILED,
                "nothing renameable here: put the cursor on a namespaced id (`ns:name`)".into(),
            ));
        };
        let r = o.range.expect("renameable has a range");
        let placeholder = text.get(r.start..r.end).unwrap_or(&o.name).to_owned();
        match range_json(root, docs, t, &rel, Some(r)) {
            Some(range) => Ok(json!({ "range": range, "placeholder": placeholder })),
            None => Err(Fail(REQUEST_FAILED, "cannot read the file".into())),
        }
    })
}

pub fn rename(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    document_changes: bool,
    p: RenameParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let blocked = |m: String| Fail(REQUEST_FAILED, m);
        let at = cursor(root, docs, &p.text_document_position, t)
            .and_then(|(rel, _, off)| Some(t.at(&rel, off)?.occ.clone()))
            .filter(renameable)
            .ok_or_else(|| blocked("nothing renameable here".into()))?;
        let from = at.name.clone();
        // a template head (`${ns}:car`) is shown, and typed, as its local name
        let to = match (at.local, p.new_name.contains(':')) {
            (true, false) => format!(
                "{}:{}",
                from.split_once(':').map_or("", |(n, _)| n),
                p.new_name
            ),
            _ => p.new_name.clone(),
        };
        let fixes = symbols::rename(t, None, &from, &to).map_err(|d| blocked(d.message))?;
        let (open, closed): (Vec<_>, Vec<_>) =
            fixes.into_iter().partition(|f| docs.contains_key(&f.path));
        // files not open: the same on-disk verification as `symbols::apply`
        let planned = symbols::plan(root, t, &closed).map_err(|d| blocked(d.message))?;
        let mut files: Vec<(RelPath, Arc<String>, Vec<beacon_engine::diag::Fix>)> = Vec::new();
        for g in &planned {
            let text = String::from_utf8_lossy(&g.old).into_owned();
            files.push((g.path.clone(), Arc::new(text), g.fixes.clone()));
        }
        for g in open.chunk_by(|a, b| a.path == b.path) {
            files.push((g[0].path.clone(), docs[&g[0].path].text.clone(), g.to_vec()));
        }
        let mut edits = Vec::new();
        for (rel, text, fixes) in files {
            let lines = match docs.contains_key(&rel) {
                true => t
                    .line_starts(&rel)
                    .map_or_else(|| line_starts(&text), <[u32]>::to_vec),
                false => line_starts(&text),
            };
            let e: Vec<Value> = fixes
                .iter()
                .map(|f| {
                    let r = range(
                        position_at(&text, &lines, f.start as u32),
                        position_at(&text, &lines, f.end as u32),
                    );
                    json!({ "range": r, "newText": f.replacement })
                })
                .collect();
            edits.push((rel, e));
        }
        if document_changes {
            let v: Vec<Value> = edits
                .into_iter()
                .map(|(rel, e)| {
                    let version = docs.get(&rel).map(|d| d.version);
                    json!({ "textDocument": { "uri": uri_of(root, &rel), "version": version },
                            "edits": e })
                })
                .collect();
            Ok(json!({ "documentChanges": v }))
        } else {
            let m: serde_json::Map<String, Value> = edits
                .into_iter()
                .map(|(rel, e)| (uri_of(root, &rel).as_str().to_owned(), Value::Array(e)))
                .collect();
            Ok(json!({ "changes": m }))
        }
    })
}

pub fn hover(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    p: HoverParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let Some((rel, _, off)) = cursor(root, docs, &p.text_document_position_params, t) else {
            return Ok(Value::Null);
        };
        // the TypeScript extension owns hover in scripts
        if classify(&rel) == beacon_engine::symbols::FileClass::Script {
            return Ok(Value::Null);
        }
        let Some(at) = t.at(&rel, off) else {
            return Ok(Value::Null);
        };
        let kinds = kinds_of(t, at.occ);
        let defs: usize = kinds
            .iter()
            .map(|&k| t.definitions(k, &at.occ.name).len())
            .sum();
        let refs: BTreeSet<_> = kinds
            .iter()
            .flat_map(|&k| t.references(k, &at.occ.name))
            .map(|l| (l.path.clone(), l.occ.range))
            .collect();
        let kind = kinds
            .iter()
            .map(|&k| kind_name(k))
            .collect::<Vec<_>>()
            .join(" / ");
        let role = if at.occ.role == Role::Definition {
            "definition"
        } else {
            "reference"
        };
        let value = format!(
            "**{kind}** `{}`\n\n{defs} definition(s), {} reference(s) ({role} here)",
            at.occ.name,
            refs.len()
        );
        let mut v = json!({ "contents": { "kind": "markdown", "value": value } });
        if let Some(r) = range_json(root, docs, t, &rel, at.occ.range) {
            v["range"] = r;
        }
        Ok(v)
    })
}

pub fn completion(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    p: CompletionParams,
) -> Res {
    with_table(st, trusted, |root, t| {
        let empty = Ok(json!([]));
        let Some((rel, text, off)) = cursor(root, docs, &p.text_document_position, t) else {
            return empty;
        };
        let before = text.get(..off).unwrap_or("");
        let word_start = before
            .char_indices()
            .rev()
            .find(|&(_, c)| !(c.is_ascii_alphanumeric() || "_:.".contains(c)))
            .map_or(0, |(i, c)| i + c.len_utf8());
        let word = &before[word_start..];
        let kinds = match t.at(&rel, off).and_then(|l| l.occ.kind) {
            Some(k) => vec![k],
            None if word.contains(':') => vec![
                SymbolKind::Entity,
                SymbolKind::Item,
                SymbolKind::Block,
                SymbolKind::Function,
            ],
            None => return empty,
        };
        let items: Vec<Value> = t
            .find("", &kinds, usize::MAX)
            .into_iter()
            .filter(|e| e.name.starts_with(word))
            .take(200)
            .map(|e| json!({ "label": e.name, "kind": 12, "detail": kind_name(e.kind) }))
            .collect();
        Ok(json!(items))
    })
}

/// Runs a check and returns the diagnostics per file (project-relative), each file that had
/// diagnostics before and has none now with an empty list (so the editor clears it).
/// Diagnostics without a file go to `config.json`.
pub fn diagnostics(
    st: &mut crate::daemon::EngineState,
    trusted: bool,
    docs: &Docs,
    published: &BTreeSet<RelPath>,
) -> BTreeMap<RelPath, Vec<Value>> {
    let root = st.root().to_path_buf();
    let diags: Vec<Diagnostic> = st.session(trusted).check().diagnostics;
    let config = RelPath::parse(CONFIG_FILE).expect("config file name is a path");
    let mut texts: HashMap<RelPath, Option<Arc<String>>> = HashMap::new();
    let mut text = |rel: &RelPath| {
        texts
            .entry(rel.clone())
            .or_insert_with(|| text_of(&root, docs, rel))
            .clone()
    };
    // open files always get a (possibly empty) list; so do files that had some before
    let mut out: BTreeMap<RelPath, Vec<Value>> = published
        .iter()
        .chain(docs.keys())
        .map(|p| (p.clone(), vec![]))
        .collect();
    for d in &diags {
        let rel = d.span.as_ref().map_or(config.clone(), |s| s.path.clone());
        let t = text(&rel);
        let mut related = Vec::new();
        for s in &d.related {
            let rt = text(&s.path);
            related.push((s.clone(), rt));
        }
        let v = super::convert::diagnostic(d, t.as_deref().map(String::as_str), |s: &Span| {
            let rt = related.iter().find(|(r, _)| r == s)?.1.clone()?;
            let lines = line_starts(&rt);
            let ls = *lines.get(s.line? as usize - 1)? as usize;
            let a = position_at(
                &rt,
                &lines,
                (ls + s.column.unwrap_or(1) as usize - 1) as u32,
            );
            Some(json!({
                "location": { "uri": uri_of(&root, &s.path), "range": range(a, a) },
                "message": "related",
            }))
        });
        out.entry(rel).or_default().push(v);
    }
    out
}
