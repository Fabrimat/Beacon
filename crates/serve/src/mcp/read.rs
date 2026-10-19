//! Read tools: they never write (`check` persists nothing, the API index is never downloaded).

use super::{Client, arg, bad_args, failure, note, ok, page, text};
use crate::daemon::EngineHandle;
use beacon_engine::config::{Artifact, CONFIG_FILE};
use beacon_engine::debug::{self, Mode};
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::lock::Lockfile;
use beacon_engine::path::RelPath;
use beacon_engine::symbols::{SymbolKind, SymbolTable};
use beacon_engine::{agents, route, sarif};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

/// Every kind, for a reference lookup of a name nothing defines (typed references by kind).
const ALL_KINDS: [SymbolKind; 10] = [
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

fn key(d: &Diagnostic) -> String {
    serde_json::to_string(d).unwrap_or_default()
}

/// `check`: SARIF of the project (restricted unless this connection is trusted). With
/// `since` = the cursor of this connection's previous `check`, only the new diagnostics and
/// the keys of the resolved ones. Content 0 is SARIF; content 1 the cursor and counts.
pub(super) fn check(c: &mut Client, a: &Value) -> Value {
    let h2 = c.h.clone(); // trust is read on the engine thread
    let diags = c.h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        let mut d = st.session(t).check().diagnostics;
        if let Ok(cfg) = route::load(&root) {
            d.extend(agents::check(&root, &cfg));
        }
        d
    });
    c.n += 1;
    let cursor = format!("c{}", c.n);
    let keys: BTreeSet<String> = diags.iter().map(key).collect();
    let prev = c.last.replace((cursor.clone(), keys.clone()));
    let base = match (a["since"].as_str(), prev) {
        (Some(s), Some((pc, pk))) if s == pc => Some(pk),
        _ => None,
    };
    let (shown, meta) = match base {
        Some(pk) => {
            let new: Vec<Diagnostic> = diags
                .iter()
                .filter(|d| !pk.contains(&key(d)))
                .cloned()
                .collect();
            let resolved: Vec<String> = pk
                .difference(&keys)
                .filter_map(|k| serde_json::from_str::<Diagnostic>(k).ok())
                .map(|d| format!("{} {}", d.code.id(), d.message))
                .collect();
            let meta = json!({ "cursor": cursor, "delta": true, "new": new.len(),
                "resolved": resolved, "total": diags.len() });
            (new, meta)
        }
        None => {
            let meta = json!({ "cursor": cursor, "delta": false, "total": diags.len() });
            (diags, meta)
        }
    };
    let mut r = text(
        serde_json::to_string(&sarif::to_sarif(&shown)).unwrap_or_default(),
        false,
    );
    r["content"]
        .as_array_mut()
        .expect("content")
        .push(json!({ "type": "text", "text": meta.to_string() }));
    r
}

pub(super) fn project_info(h: &EngineHandle) -> Value {
    let root = h.call(|st| st.root().to_path_buf());
    let Some(cfg) = std::fs::read(root.join(CONFIG_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return text(format!("cannot read {CONFIG_FILE} in the project"), true);
    };
    let b = &cfg["beacon"];
    let outputs: Vec<&Value> = b["outputs"]
        .as_array()
        .map(|a| a.iter().map(|x| &x["id"]).collect())
        .unwrap_or_default();
    let targets: Vec<&String> = b["targets"]
        .as_object()
        .map(|m| m.keys().collect())
        .unwrap_or_default();
    ok(json!({
        "name": cfg["name"], "namespace": cfg["namespace"], "targetVersion": cfg["targetVersion"],
        "outputs": outputs, "targets": targets,
        "trust": { "trusted": h.trusted(), "source": h.trust_source() },
    }))
}

pub(super) fn explain(a: &Value) -> Value {
    let id = a["code"].as_str().unwrap_or_default();
    match Code::from_id(id) {
        Some(c) => {
            let sev = serde_json::to_value(c.default_severity()).unwrap_or_default();
            text(
                format!(
                    "{} ({})\n\n{}",
                    c.id(),
                    sev.as_str().unwrap_or("?"),
                    c.doc().trim()
                ),
                false,
            )
        }
        None => text(format!("unknown diagnostic code `{id}`"), true),
    }
}

pub(super) fn kind_arg(a: &Value) -> Result<Option<SymbolKind>, Value> {
    match a["kind"].as_str() {
        None => Ok(None),
        Some(k) => serde_json::from_value(Value::String(k.into()))
            .map(Some)
            .map_err(|_| bad_args(&format!("unknown kind `{k}`"))),
    }
}

/// Runs `f` on the symbol table of this connection's session (checked first if it has none).
fn with_table(
    h: &EngineHandle,
    f: impl FnOnce(&SymbolTable, &Path) -> Value + Send + 'static,
) -> Value {
    let h2 = h.clone(); // trust is read on the engine thread
    h.call(move |st| {
        let t = h2.trusted();
        let root = st.root().to_path_buf();
        let s = st.session(t);
        let mut diags = vec![];
        if s.symbols().is_none() {
            diags = s.check().diagnostics;
        }
        match s.symbols() {
            Some(table) => f(table, &root),
            None => failure(&diags),
        }
    })
}

pub(super) fn symbols_find(h: &EngineHandle, a: &Value) -> Value {
    let kinds: Vec<SymbolKind> = match kind_arg(a) {
        Ok(k) => k.into_iter().collect(),
        Err(e) => return e,
    };
    let query = a["query"].as_str().unwrap_or_default().to_owned();
    let a = a.clone();
    with_table(h, move |t, _| {
        let list = t
            .find(&query, &kinds, usize::MAX)
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect();
        page(list, &a, "symbols")
    })
}

pub(super) fn symbols_definition(h: &EngineHandle, a: &Value) -> Value {
    let name = match arg(a, "name") {
        Ok(n) => n.to_owned(),
        Err(e) => return e,
    };
    let kinds: Vec<SymbolKind> = match kind_arg(a) {
        Ok(k) => k.into_iter().collect(),
        Err(e) => return e,
    };
    with_table(h, move |t, _| {
        let defs: Vec<Value> = t
            .find(&name, &kinds, usize::MAX)
            .into_iter()
            .filter(|e| e.name == name)
            .filter_map(|e| serde_json::to_value(e).ok())
            .collect();
        ok(json!({ "definitions": defs }))
    })
}

/// 1-based line and byte column of `offset`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text.as_bytes()[..offset.min(text.len())];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let col = before.len()
        - before
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |p| p + 1)
        + 1;
    (line, col)
}

pub(super) fn symbols_references(h: &EngineHandle, a: &Value) -> Value {
    let name = match arg(a, "name") {
        Ok(n) => n.to_owned(),
        Err(e) => return e,
    };
    let kind = match kind_arg(a) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let a = a.clone();
    with_table(h, move |t, root| {
        let kinds: Vec<SymbolKind> = match kind {
            Some(k) => vec![k],
            None => {
                let defined: Vec<_> = t
                    .find(&name, &[], usize::MAX)
                    .into_iter()
                    .filter(|e| e.name == name)
                    .map(|e| e.kind)
                    .collect();
                if defined.is_empty() {
                    ALL_KINDS.to_vec()
                } else {
                    defined
                }
            }
        };
        let mut seen = BTreeSet::new();
        let mut by_file: Vec<(RelPath, usize)> = vec![];
        for k in kinds {
            for l in t.references(k, &name) {
                if let Some(r) = l.occ.range
                    && seen.insert((l.path.clone(), r.start))
                {
                    by_file.push((l.path.clone(), r.start));
                }
            }
        }
        by_file.sort();
        let mut texts: Option<(RelPath, String)> = None;
        let items = by_file
            .into_iter()
            .map(|(p, off)| {
                if texts.as_ref().is_none_or(|(tp, _)| *tp != p) {
                    let body = std::fs::read_to_string(root.join(p.as_str())).unwrap_or_default();
                    texts = Some((p.clone(), body));
                }
                let (line, column) = line_col(&texts.as_ref().expect("read").1, off);
                json!({ "path": p, "line": line, "column": column })
            })
            .collect();
        page(items, &a, "references")
    })
}

/// The game version of the project, or of `target`.
fn game_version(root: &Path, target: Option<&str>) -> Result<String, Vec<Diagnostic>> {
    let cfg = route::load(root)?;
    Ok(target
        .and_then(|t| cfg.beacon.targets.get(t))
        .and_then(|t| t.target_version.clone())
        .unwrap_or(cfg.target_version))
}

/// Runs `f` on the API index of the project's game version; a missing index is a note.
fn with_index(
    h: &EngineHandle,
    target: Option<String>,
    f: impl FnOnce(&beacon_engine::api_index::ApiIndex) -> Value + Send + 'static,
) -> Value {
    h.call(move |st| {
        let v = match game_version(st.root(), target.as_deref()) {
            Ok(v) => v,
            Err(d) => return failure(&d),
        };
        match st.api_index(&v) {
            Ok(ix) => f(ix),
            Err(d) => note(d), // never a download, never an error
        }
    })
}

pub(super) fn schema_lookup(h: &EngineHandle, a: &Value) -> Value {
    let target = a["target"].as_str().map(str::to_owned);
    let (kind, component) = (
        a["kind"].as_str().map(str::to_owned),
        a["component"].as_str().map(str::to_owned),
    );
    let a = a.clone();
    with_index(h, target, move |ix| {
        let Some(kind) = kind else {
            let kinds: Vec<&String> = ix.components.keys().collect();
            return ok(json!({ "gameVersion": ix.game_version, "kinds": kinds }));
        };
        let Some(comps) = ix.components.get(&kind) else {
            let kinds: Vec<&String> = ix.components.keys().collect();
            return text(
                format!("unknown file kind `{kind}`; kinds: {kinds:?}"),
                true,
            );
        };
        match component {
            Some(c) => {
                let hits: Vec<Value> = comps
                    .iter()
                    .filter(|(n, _)| **n == c || n.ends_with(&format!(":{c}")))
                    .map(|(n, v)| json!({ "name": n, "description": v.description, "experimental": v.experimental }))
                    .collect();
                if hits.is_empty() {
                    text(format!("no component `{c}` in `{kind}` for {}", ix.game_version), true)
                } else {
                    ok(json!({ "gameVersion": ix.game_version, "kind": kind, "components": hits }))
                }
            }
            None => page(
                comps
                    .iter()
                    .map(|(n, v)| json!({ "name": n, "description": v.description, "experimental": v.experimental }))
                    .collect(),
                &a,
                "components",
            ),
        }
    })
}

pub(super) fn api_lookup(h: &EngineHandle, a: &Value) -> Value {
    let q = match arg(a, "query") {
        Ok(q) => q.to_owned(),
        Err(e) => return e,
    };
    let limit = a["limit"].as_u64().map_or(20, |l| l.clamp(1, 100)) as usize;
    with_index(h, a["target"].as_str().map(str::to_owned), move |ix| {
        ok(json!({ "gameVersion": ix.game_version, "hits": ix.lookup(&q, limit) }))
    })
}

pub(super) fn debug_config(h: &EngineHandle, a: &Value) -> Value {
    let mode = match a["mode"].as_str() {
        None | Some("connect") => Mode::Connect,
        Some("listen") => Mode::Listen,
        Some(m) => return bad_args(&format!("unknown mode `{m}`")),
    };
    let (target, output) = (
        a["target"].as_str().map(str::to_owned),
        a["output"].as_str().map(str::to_owned),
    );
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        h.call(move |st| -> Result<debug::LaunchConfig, Vec<Diagnostic>> {
            let root = st.root().to_path_buf();
            let cfg = route::load(&root)?;
            let target = target
                .or_else(|| {
                    cfg.beacon
                        .targets
                        .iter()
                        .find(|(_, t)| t.artifact == Artifact::Dev)
                        .map(|(id, _)| id.clone())
                })
                .unwrap_or_else(|| "dev".into());
            let output = output.unwrap_or_else(|| {
                cfg.beacon
                    .outputs
                    .first()
                    .map_or_else(|| "main".into(), |o| o.id.clone())
            });
            // frozen: reading the lockfile must never write it
            let lock = Lockfile::ensure(&root, &cfg, std::slice::from_ref(&output), &[], &[], true)
                .map_err(|d| vec![d])?;
            debug::launch_config(&root, &cfg, &lock, &target, &output, mode).map_err(|d| vec![d])
        })
    }));
    match r {
        Ok(Ok(c)) => ok(serde_json::to_value(c).unwrap_or_default()),
        Ok(Err(d)) => failure(&d),
        Err(_) => text(
            "debug_config is not available yet in this build".into(),
            true,
        ),
    }
}
