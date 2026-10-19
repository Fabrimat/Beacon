//! system_template ingestion (5.6, 14.3): Beacon does not evaluate `_map.py`; a real legacy run
//! is passed as `--evaluated <dir>` (Regolith tmp layout: `BP/`, `RP/`, `data/`, plus
//! `system_template_log.json`) and read here into a [`LegacyEval`].
//!
//! - Outputs are the log's keys; contributors come from the log's `sources` in order, with the
//!   policy implied by the status (`Created.`, `Merged.`, `Appended content to the end.`, ...).
//! - A body equal to its (last applied) source is a plain copy (`Body::Passthrough`); anything
//!   else is the evaluated content (`Json`, `Text`, `Bytes`).
//! - Files in the evaluated `BP/` / `RP/` without a log entry are base-pack seeds when they
//!   equal the base file, otherwise outputs of another filter (subfunctions, custom blocks,
//!   esbuild) with no contributors; `data/**/subscripts/**` script files are kept the same way.

use super::{Applied, Contributor, EvalOutput, LegacyEval, LegacyProject, Policy, Template};
use crate::diag::{Code, Diagnostic};
use crate::merge::strip_comments;
use crate::model::Body;
use crate::path::RelPath;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const CODE_EXT: [&str; 6] = [".ts", ".js", ".mjs", ".cjs", ".mts", ".cts"];
const LOG: &str = "system_template_log.json";

#[derive(serde::Deserialize)]
struct LogEntry {
    #[serde(default)]
    sources: Vec<LogSource>,
}

#[derive(serde::Deserialize)]
struct LogSource {
    path: String,
    #[serde(default)]
    status: String,
}

fn status(s: &str) -> (Applied, Policy) {
    let s = s.to_lowercase();
    if s.starts_with("merged") {
        (Applied::Merged, Policy::Merge)
    } else if s.starts_with("appended") && s.contains("beginning") {
        (Applied::Appended, Policy::AppendStart)
    } else if s.starts_with("appended") {
        (Applied::Appended, Policy::AppendEnd)
    } else if s.starts_with("overwr") {
        (Applied::Overwrote, Policy::Overwrite)
    } else if s.starts_with("skipped") {
        (Applied::Skipped, Policy::Skip)
    } else {
        (Applied::Created, Policy::Stop)
    }
}

/// `./data/` -> `data`.
pub(crate) fn trim(s: &str) -> &str {
    let mut s = s;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.trim_end_matches('/')
}

fn lf(b: &[u8]) -> Vec<u8> {
    let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(b);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\r' && b.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// The body of an evaluated file: a passthrough of `source` when equal to it (line endings
/// aside), else parsed JSON, text or bytes.
fn body(path: &str, bytes: &[u8], source: Option<(&RelPath, &[u8])>) -> Body {
    if let Some((src, sb)) = source
        && lf(sb) == lf(bytes)
    {
        return Body::Passthrough(src.clone());
    }
    if path.ends_with(".json")
        && let Ok(v) = serde_json::from_slice::<Value>(&strip_comments(&lf(bytes)))
    {
        return Body::Json(v);
    }
    match String::from_utf8(lf(bytes)) {
        Ok(t) if !bytes.contains(&0) => Body::Text(t),
        _ => Body::Bytes(bytes.to_vec()),
    }
}

/// Every regular file below `dir` as (`/` path relative to `dir`, file). Links are not
/// followed: they are reported and skipped. Contents are read later, one file at a time.
fn files(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>, diags: &mut Vec<Diagnostic>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        match std::fs::symlink_metadata(e.path()) {
            Ok(m) if m.file_type().is_symlink() => diags.push(Diagnostic::new(
                Code::MigrateUnsupported,
                format!("--evaluated {r}: a link, not followed"),
            )),
            Ok(m) if m.is_dir() => files(&e.path(), &r, out, diags),
            Ok(m) if m.is_file() => out.push((r, e.path())),
            _ => {}
        }
    }
}

/// Reads a real system_template run (see the module docs). Errors: no or unreadable log.
pub fn ingest(project: &LegacyProject, evaluated: &Path) -> Result<LegacyEval, Vec<Diagnostic>> {
    let err = |m: String| vec![Diagnostic::new(Code::MigrateUnsupported, m)];
    let log_name = project
        .config
        .regolith
        .profiles
        .values()
        .flat_map(|p| &p.filters)
        .filter(|f| f.filter.as_deref() == Some("system_template"))
        .find_map(|f| f.settings.as_ref()?.get("log_path")?.as_str())
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
        .unwrap_or_else(|| LOG.into());
    let bytes = std::fs::read(evaluated.join(&log_name)).map_err(|e| {
        err(format!(
            "--evaluated: cannot read {log_name} ({e}); pass a real system_template run (Regolith tmp layout plus its log)"
        ))
    })?;
    let log: BTreeMap<String, LogEntry> = serde_json::from_slice(&strip_comments(&lf(&bytes)))
        .map_err(|e| err(format!("--evaluated {log_name}: {e}")))?;
    let data = trim(
        project
            .config
            .regolith
            .data_path
            .as_deref()
            .unwrap_or(super::DEFAULT_DATA_PATH),
    )
    .to_string();
    let base: Vec<(&str, String)> = [
        ("BP/", &project.config.packs.behavior_pack),
        ("RP/", &project.config.packs.resource_pack),
    ]
    .into_iter()
    .filter_map(|(k, v)| Some((k, trim(v.as_deref()?).to_string())))
    .collect();
    // tmp-layout `data/x` -> project-relative `<dataPath>/x`; `BP/x` -> the base pack
    let project_rel = |p: &str| -> Option<RelPath> {
        if let Some(rest) = p.strip_prefix("data/") {
            return RelPath::parse(&format!("{data}/{rest}")).ok();
        }
        let (k, dir) = base.iter().find(|(k, _)| p.starts_with(*k))?;
        RelPath::parse(&format!("{dir}/{}", &p[k.len()..])).ok()
    };

    let mut ev = LegacyEval::default();
    let mut tree = Vec::new();
    for top in ["BP", "RP", "data"] {
        files(&evaluated.join(top), top, &mut tree, &mut ev.diagnostics);
    }
    let tree: BTreeMap<String, PathBuf> = tree.into_iter().collect();
    let read = |r: &str, f: &Path, diags: &mut Vec<Diagnostic>| match std::fs::read(f) {
        Ok(b) => Some(b),
        Err(e) => {
            let m = format!("--evaluated {r}: {e}");
            diags.push(Diagnostic::new(Code::MigrateUnsupported, m));
            None
        }
    };
    let st_root = format!("{data}/system_template/");
    for (target, entry) in &log {
        let Ok(t) = RelPath::parse(target) else {
            ev.diagnostics.push(Diagnostic::new(
                Code::MigrateUnsupported,
                format!(
                    "system_template target {target:?} is not a portable relative path; skipped"
                ),
            ));
            continue;
        };
        // `UNPACK:SUBFUNCTION` sources: the logged `X.mcfunction` became a folder `X/` of
        // functions (subfunctions filter); every file below it comes from this entry
        let files: Vec<(&String, &PathBuf)> = match tree.get_key_value(t.as_str()) {
            Some(f) => vec![f],
            None => match t.as_str().strip_suffix(".mcfunction") {
                Some(stem) => {
                    let dir = format!("{stem}/");
                    tree.range(dir.clone()..)
                        .take_while(|(k, _)| k.starts_with(&dir))
                        .collect()
                }
                None => vec![],
            },
        };
        if files.is_empty() {
            ev.diagnostics.push(Diagnostic::new(
                Code::MigrateUnsupported,
                format!("--evaluated has no {t} (listed in {log_name}); skipped"),
            ));
            continue;
        }
        let mut contributors = Vec::new();
        for s in &entry.sources {
            let Some(source) = project_rel(&s.path.replace('\\', "/")) else {
                ev.diagnostics.push(Diagnostic::new(
                    Code::MigrateUnsupported,
                    format!("{t}: source {:?} is not inside the data dir", s.path),
                ));
                continue;
            };
            let system = source
                .as_str()
                .strip_prefix(&st_root)
                .and_then(|r| r.split_once('/'))
                .map_or("", |(s, _)| s);
            let map = RelPath::parse(&format!("{st_root}{system}/_map.py"))
                .unwrap_or_else(|_| source.clone());
            let (applied, policy) = status(&s.status);
            contributors.push(Contributor {
                map,
                entry: 0,
                source,
                policy,
                template: Template::None,
                applied,
            });
        }
        // the body is compared with the source that decided it last
        let last = contributors
            .iter()
            .rev()
            .find(|c| c.applied != Applied::Skipped)
            .map(|c| c.source.clone());
        let src = last.and_then(|s| {
            let b = std::fs::read(project.root.join(s.as_str())).ok()?;
            Some((s, b))
        });
        for (path, file) in files {
            let Some(bytes) = read(path, file, &mut ev.diagnostics) else {
                continue;
            };
            let body = body(path, &bytes, src.as_ref().map(|(s, b)| (s, &b[..])));
            ev.outputs.insert(
                path.clone(),
                EvalOutput {
                    path: path.clone(),
                    body,
                    contributors: contributors.clone(),
                },
            );
        }
    }
    // esbuild roots: `data/<X>/` holding a `subscripts/` folder
    let esbuild_roots: BTreeSet<String> = tree
        .keys()
        // `data/system_template/` holds the systems (the filter's input), not a bundle root
        .filter(|k| {
            k.starts_with("data/")
                && !k.starts_with("data/system_template/")
                && !k.contains("/node_modules/")
        })
        .filter_map(|k| k.split_once("/subscripts/").map(|(r, _)| format!("{r}/")))
        .collect();
    // the rest of BP/RP: base seeds (equal to the base file) or other filters' outputs
    for (p, file) in &tree {
        if ev.outputs.contains_key(p) || RelPath::parse(p).is_err() {
            continue;
        }
        // `data/` is the legacy data dir: only the code system_template_esbuild bundles
        let scripts = p.starts_with("data/")
            && esbuild_roots.iter().any(|r| p.starts_with(r.as_str()))
            && !p.split('/').any(|s| s == "node_modules" || s == ".yalc")
            && CODE_EXT.iter().any(|e| p.ends_with(e));
        if p.starts_with("data/") && !scripts {
            continue;
        }
        let Some(bytes) = read(p, file, &mut ev.diagnostics) else {
            continue;
        };
        let seed = !scripts
            && project_rel(p)
                .and_then(|b| std::fs::read(project.root.join(b.as_str())).ok())
                .is_some_and(|b| lf(&b) == lf(&bytes));
        if seed {
            continue;
        }
        ev.outputs.insert(
            p.clone(),
            EvalOutput {
                path: p.clone(),
                body: body(p, &bytes, None),
                contributors: vec![],
            },
        );
    }
    Ok(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(status("Created."), (Applied::Created, Policy::Stop));
        assert_eq!(status("Merged."), (Applied::Merged, Policy::Merge));
        assert_eq!(
            status("Appended content to the end."),
            (Applied::Appended, Policy::AppendEnd)
        );
        assert_eq!(
            status("Appended content to the beginning."),
            (Applied::Appended, Policy::AppendStart)
        );
        assert_eq!(
            status("Overwritten."),
            (Applied::Overwrote, Policy::Overwrite)
        );
        assert_eq!(status("Skipped."), (Applied::Skipped, Policy::Skip));
    }

    #[test]
    fn bodies() {
        let s = RelPath::parse("d/a.json").unwrap();
        assert_eq!(
            body("BP/a.json", b"{}\r\n", Some((&s, b"{}\n"))),
            Body::Passthrough(s.clone())
        );
        assert_eq!(
            body("BP/a.json", b"{\"a\":1}", Some((&s, b"{}"))),
            Body::Json(serde_json::json!({"a": 1}))
        );
        assert_eq!(
            body("RP/x.lang", b"a=1\r\n", None),
            Body::Text("a=1\n".into())
        );
        assert_eq!(body("RP/x.png", b"\0\x01", None), Body::Bytes(vec![0, 1]));
    }
}
