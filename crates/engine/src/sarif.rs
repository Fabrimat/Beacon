//! Workstream A5: SARIF 2.1 writer and compact text view (6.3).

use crate::diag::{Diagnostic, Severity, Span};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn level(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
    }
}

/// Stable order: path, line, column, code, message (diagnostics without a span first).
fn sorted(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    let mut v: Vec<_> = diags.iter().collect();
    v.sort_by_cached_key(|d| {
        let s = d.span.as_ref();
        (
            s.map(|s| s.path.as_str().to_owned()),
            s.and_then(|s| s.line),
            s.and_then(|s| s.column),
            d.code,
            d.message.clone(),
        )
    });
    v
}

// ponytail: `uri` is the raw relative path, not percent-encoded yet (add when paths with spaces/#/% matter).
fn location(s: &Span) -> Value {
    let mut region = serde_json::Map::new();
    if let Some(l) = s.line {
        region.insert("startLine".into(), l.into());
        if let Some(c) = s.column {
            region.insert("startColumn".into(), c.into());
        }
    }
    let mut loc = json!({ "physicalLocation": { "artifactLocation": { "uri": s.path.as_str() } } });
    if !region.is_empty() {
        loc["physicalLocation"]["region"] = region.into();
    }
    loc
}

/// SARIF 2.1.0 log with one run (tool `beacon`, rules from `Code`, results with locations,
/// related locations and fixes).
pub fn to_sarif(diags: &[Diagnostic]) -> Value {
    let diags = sorted(diags);
    let rules: BTreeMap<&str, Value> = diags
        .iter()
        .map(|d| {
            let c = d.code;
            let doc = c.doc().trim();
            let short = doc.lines().next().unwrap_or("");
            (
                c.id(),
                json!({ "id": c.id(), "shortDescription": { "text": short }, "fullDescription": { "text": doc } }),
            )
        })
        .collect();
    let results: Vec<Value> = diags
        .iter()
        .map(|d| {
            let mut r = json!({
                "ruleId": d.code.id(),
                "level": level(d.severity),
                "message": { "text": d.message },
                "locations": d.span.iter().map(location).collect::<Vec<_>>(),
            });
            if !d.related.is_empty() {
                r["relatedLocations"] = d.related.iter().map(location).collect();
            }
            if !d.fixes.is_empty() {
                r["fixes"] = d
                    .fixes
                    .iter()
                    .map(|f| {
                        json!({
                            "description": { "text": f.description },
                            "artifactChanges": [{
                                "artifactLocation": { "uri": f.path.as_str() },
                                "replacements": [{
                                    "deletedRegion": { "byteOffset": f.start, "byteLength": f.end.saturating_sub(f.start) },
                                    "insertedContent": { "text": f.replacement },
                                }],
                            }],
                            "properties": { "safe": f.safe },
                        })
                    })
                    .collect();
            }
            r
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "beacon",
                "version": env!("CARGO_PKG_VERSION"),
                "rules": rules.into_values().collect::<Vec<_>>(),
            } },
            "results": results,
        }],
    })
}

/// `path:line:col: error[BCN0011]: message`, one per line, sorted by path then line.
pub fn to_text(diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in sorted(diags) {
        if let Some(s) = &d.span {
            out += s.path.as_str();
            if let Some(l) = s.line {
                out += &format!(":{l}");
                if let Some(c) = s.column {
                    out += &format!(":{c}");
                }
            }
            out += ": ";
        }
        out += &format!("{}[{}]: {}\n", level(d.severity), d.code.id(), d.message);
    }
    out
}
