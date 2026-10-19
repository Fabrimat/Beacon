//! Phase 3 B3: `legacy_filter`, a Regolith filter kept as an external-process plugin
//! (ARCHITECTURE 5.10, 14.3). Trusted code: the caller only runs it in a trusted session.
//!
//! Config: `beacon.plugins: [{"legacyFilter": {"run": ["python", "filter.py"], "settings": {},
//! "stage": "afterMerge", "cwd": "tools/filter", "timeoutSecs": 120}}]`. Each non-static output's
//! merged items are written to a temp dir as `BP/`, `RP/` and an empty `data/`, the command runs
//! there with a scrubbed environment, and only `BP/` and `RP/` are read back.

use crate::config::CONFIG_FILE;
use crate::diag::{Code, Diagnostic, Severity};
use crate::model::{Body, ContentItem, Merge, OutputId, Pack};
use crate::path::{RelPath, Vfs};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Environment the filter sees (plus `FILTER_SETTINGS`); everything else is dropped.
pub const ENV_ALLOW: &[&str] = &[
    "PATH",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOME",
    "PYTHONUTF8",
    "PYTHONDONTWRITEBYTECODE",
    "FILTER_SETTINGS",
];

struct Filter {
    run: Vec<String>,
    settings: Value,
    dir: PathBuf,
    timeout: Duration,
    max_file: u64,
    max_total: u64,
}

const MAX_FILE: u64 = 64 << 20;
const MAX_TOTAL: u64 = 512 << 20;
const LOG_TAIL: u64 = 64 << 10;

type PErr = (Code, String);

fn parse(root: &Path, plugins: &[Value]) -> Result<Vec<Filter>, PErr> {
    let bad = |m: String| (Code::LegacyFilterFailed, m);
    // read once, only when a plugin has a git source (pins are in the lockfile, never the config)
    let mut lock = None;
    let mut v = vec![];
    for def in plugins.iter().filter_map(|p| p.get("legacyFilter")) {
        let run: Vec<String> = def
            .get("run")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .filter(|r: &Vec<String>| !r.is_empty())
            .ok_or_else(|| bad("legacyFilter.run must be a non-empty array of strings".into()))?;
        match def.get("stage").and_then(Value::as_str) {
            None | Some("afterMerge") => {}
            Some(s) => return Err(bad(format!("legacyFilter.stage {s:?} is not supported"))),
        }
        // a git source replaces the project as the base of `cwd`: the pinned, re-hashed checkout
        let base = match def.get("source") {
            None => root.to_owned(),
            Some(s) => {
                let id = def.get("id").and_then(Value::as_str);
                let git = s.get("git").and_then(Value::as_str);
                let (Some(id), Some(git)) = (id, git) else {
                    return Err(bad("legacyFilter.source needs `id` and `source.git`".into()));
                };
                let l = lock.get_or_insert_with(|| crate::plugin::read_lock(root));
                crate::plugin::verified_checkout(id, git, l.as_ref())?
            }
        };
        let dir = match def.get("cwd").and_then(Value::as_str) {
            Some(c) => base.join(
                RelPath::parse(c)
                    .map_err(|e| bad(format!("legacyFilter.cwd: {e}")))?
                    .as_str(),
            ),
            None => base,
        };
        v.push(Filter {
            run,
            settings: def
                .get("settings")
                .cloned()
                .unwrap_or(Value::Object(Default::default())),
            dir,
            timeout: Duration::from_secs(
                def.get("timeoutSecs")
                    .and_then(Value::as_u64)
                    .unwrap_or(120),
            ),
            // the caps are overridable so tests can use small ones
            max_file: def
                .get("maxFileBytes")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_FILE),
            max_total: def
                .get("maxTotalBytes")
                .and_then(Value::as_u64)
                .unwrap_or(MAX_TOTAL),
        });
    }
    Ok(v)
}

/// Runs every `legacyFilter` plugin over each non-static output of `merged`. On any error the
/// affected output keeps its pre-filter items and the function returns `true` (the build must
/// write nothing). `manifest.json` of every pack is restored after the filter, so a filter can
/// never change UUIDs or versions.
pub(crate) fn apply(
    root: &Path,
    plugins: &[Value],
    merged: &mut Vec<ContentItem>,
    vfs: &dyn Vfs,
    is_static: &dyn Fn(&OutputId) -> bool,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    let mut failed = false;
    let fail = |code, m: String| {
        Diagnostic::new(code, m).at(RelPath::parse(CONFIG_FILE).expect("const"), None, None)
    };
    let filters = match parse(root, plugins) {
        Ok(f) if !f.is_empty() => f,
        Ok(_) => return false,
        Err(e) => {
            diags.push(fail(e.0, e.1));
            return true;
        }
    };
    let mut outputs: Vec<OutputId> = merged.iter().map(|i| i.dest.output.clone()).collect();
    outputs.sort();
    outputs.dedup();
    for o in outputs.into_iter().filter(|o| !is_static(o)) {
        let mut items: Vec<ContentItem> = merged
            .iter()
            .filter(|i| i.dest.output == o)
            .cloned()
            .collect();
        let manifests: Vec<ContentItem> = items
            .iter()
            .filter(|i| i.dest.path.as_str() == "manifest.json")
            .cloned()
            .collect();
        let mut ok = true;
        for f in &filters {
            match run_one(f, &o, &items, vfs, diags) {
                Ok(next) => items = next,
                Err(d) => {
                    diags.push(d);
                    ok = false;
                    failed = true;
                    break;
                }
            }
        }
        if ok {
            for m in manifests {
                let same = items
                    .iter()
                    .find(|i| i.dest == m.dest)
                    .is_some_and(|i| body_bytes(&i.body) == body_bytes(&m.body));
                if !same {
                    let mut d = Diagnostic::new(
                        Code::LegacyFilterEscaped,
                        format!(
                            "legacy filter changed {}/manifest.json; the generated manifest was restored",
                            if m.dest.pack == Pack::Behavior {
                                "BP"
                            } else {
                                "RP"
                            }
                        ),
                    );
                    d.severity = Severity::Note;
                    diags.push(d);
                }
                items.retain(|i| i.dest != m.dest);
                items.push(m);
            }
            merged.retain(|i| i.dest.output != o);
            merged.extend(items);
        }
    }
    failed
}

/// Bytes of an in-memory body; `Passthrough` (a manifest never is one here) compares equal only to itself.
fn body_bytes(b: &Body) -> Option<Vec<u8>> {
    match b {
        Body::Passthrough(_) => None,
        Body::Json(v) => Some(v.to_string().into_bytes()),
        Body::Text(t) => Some(t.clone().into_bytes()),
        Body::Bytes(b) => Some(b.clone()),
    }
}

/// Removes the scratch dir when dropped, on every exit path.
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "beacon-lf-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn run_one(
    f: &Filter,
    output: &OutputId,
    items: &[ContentItem],
    vfs: &dyn Vfs,
    diags: &mut Vec<Diagnostic>,
) -> Result<Vec<ContentItem>, Diagnostic> {
    let cfg = || RelPath::parse(CONFIG_FILE).expect("const");
    let err = |code, m: String| Diagnostic::new(code, m).at(cfg(), None, None);
    let io = |e: std::io::Error| err(Code::LegacyFilterFailed, format!("legacy filter I/O: {e}"));
    // `top` holds the work dir and the log; anything else appearing in it is an escape
    // created before `Scratch` owns it: `create_dir` fails if the path exists (a pre-placed
    // dir or link on a shared temp dir), and then nothing of ours is there for `Drop` to remove
    let path = scratch();
    fs::create_dir(&path).map_err(io)?;
    let top = Scratch(path);
    let top = &top.0;
    let work = top.join("w");
    (|| {
        for d in ["BP", "RP", "data"] {
            fs::create_dir_all(work.join(d)).map_err(io)?;
        }
        for i in items {
            let p = work
                .join(if i.dest.pack == Pack::Behavior {
                    "BP"
                } else {
                    "RP"
                })
                .join(i.dest.path.as_str());
            fs::create_dir_all(p.parent().expect("has parent")).map_err(io)?;
            let bytes = match &i.body {
                Body::Passthrough(s) => vfs.read(s).map_err(io)?,
                Body::Json(v) => v.to_string().into_bytes(),
                Body::Text(t) => t.clone().into_bytes(),
                Body::Bytes(b) => b.clone(),
            };
            fs::write(p, bytes).map_err(io)?;
        }
        let settings = f.settings.to_string();
        let log = fs::File::create(top.join("log")).map_err(io)?;
        let mut cmd = Command::new(&f.run[0]);
        // a relative script path names a file of the filter dir (cwd is the temp dir)
        for a in &f.run[1..] {
            let p = f.dir.join(a);
            cmd.arg(if p.is_file() {
                p.into_os_string()
            } else {
                a.into()
            });
        }
        cmd.arg(&settings)
            .current_dir(&work)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(io)?)
            .stderr(log);
        for k in ENV_ALLOW {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
        // no `.pyc` next to a sibling module: a checkout must stay byte-identical to its pin
        cmd.env("PYTHONUTF8", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .env("FILTER_SETTINGS", &settings);
        let tail = || {
            use std::io::{Read, Seek, SeekFrom};
            let mut buf = vec![];
            if let Ok(mut l) = fs::File::open(top.join("log")) {
                let len = l.metadata().map_or(0, |m| m.len());
                let _ = l.seek(SeekFrom::Start(len.saturating_sub(LOG_TAIL)));
                let _ = l.take(LOG_TAIL).read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        };
        let mut child = cmd.spawn().map_err(|e| {
            err(
                Code::LegacyFilterFailed,
                format!("cannot run {:?}: {e}", f.run[0]),
            )
        })?;
        let deadline = Instant::now() + f.timeout;
        let status = loop {
            if let Some(s) = child.try_wait().map_err(io)? {
                break s;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(err(
                    Code::LegacyFilterFailed,
                    format!("{:?} timed out after {}s", f.run[0], f.timeout.as_secs()),
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !status.success() {
            return Err(err(
                Code::LegacyFilterFailed,
                format!("{:?} exited with {status}: {}", f.run[0], tail()),
            ));
        }
        ingest(top, &work, output, (f.max_file, f.max_total), diags).map_err(|(c, m)| err(c, m))
    })()
}

pub(crate) fn is_link(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true; // reparse point: symlink or junction
        }
    }
    m.file_type().is_symlink()
}

/// Reads `BP/` and `RP/` back. `Err` = an escape attempt; the whole output is discarded.
fn ingest(
    top: &Path,
    work: &Path,
    output: &OutputId,
    (max_file, max_total): (u64, u64),
    diags: &mut Vec<Diagnostic>,
) -> Result<Vec<ContentItem>, (Code, String)> {
    let esc = |m: String| (Code::LegacyFilterEscaped, m);
    let mut total = 0u64;
    for e in fs::read_dir(top).map_err(|e| esc(e.to_string()))?.flatten() {
        let n = e.file_name();
        if n != "w" && n != "log" {
            return Err(esc(format!("filter wrote outside its temp dir: {n:?}")));
        }
    }
    let origin = RelPath::parse(CONFIG_FILE).expect("const");
    let mut found: BTreeMap<(u8, RelPath), Vec<u8>> = BTreeMap::new();
    for e in fs::read_dir(work)
        .map_err(|e| esc(e.to_string()))?
        .flatten()
    {
        let n = e.file_name().to_string_lossy().into_owned();
        let m = fs::symlink_metadata(e.path()).map_err(|e| esc(e.to_string()))?;
        if is_link(&m) {
            return Err(esc(format!("{n}: link in the filter output")));
        }
        let pack = match n.as_str() {
            "BP" => Pack::Behavior,
            "RP" => Pack::Resource,
            "data" => continue,
            _ => {
                let mut d = Diagnostic::new(
                    Code::LegacyFilterEscaped,
                    format!("ignored {n}: only BP/ and RP/ are read back"),
                );
                d.severity = Severity::Note;
                diags.push(d);
                continue;
            }
        };
        let mut todo = vec![(e.path(), String::new())];
        while let Some((dir, rel)) = todo.pop() {
            for c in fs::read_dir(&dir)
                .map_err(|e| esc(e.to_string()))?
                .flatten()
            {
                let name = c
                    .file_name()
                    .into_string()
                    .map_err(|n| esc(format!("{n:?}: not UTF-8")))?;
                let rel = if rel.is_empty() {
                    name
                } else {
                    format!("{rel}/{name}")
                };
                let m = fs::symlink_metadata(c.path()).map_err(|e| esc(e.to_string()))?;
                if is_link(&m) {
                    return Err(esc(format!("{rel}: link in the filter output")));
                } else if m.is_dir() {
                    todo.push((c.path(), rel));
                } else if m.is_file() {
                    let p = RelPath::parse(&rel).map_err(|e| esc(e.to_string()))?;
                    total += m.len();
                    if m.len() > max_file || total > max_total {
                        return Err((
                            Code::LegacyFilterFailed,
                            format!(
                                "{rel}: filter output exceeds the size cap ({max_file} per file, {max_total} total)"
                            ),
                        ));
                    }
                    found.insert(
                        (pack as u8, p),
                        fs::read(c.path()).map_err(|e| esc(e.to_string()))?,
                    );
                }
            }
        }
    }
    Ok(found
        .into_iter()
        .map(|((pack, path), b)| ContentItem {
            dest: crate::model::Dest {
                output: output.clone(),
                pack: if pack == Pack::Behavior as u8 {
                    Pack::Behavior
                } else {
                    Pack::Resource
                },
                path,
            },
            body: String::from_utf8(b).map_or_else(|e| Body::Bytes(e.into_bytes()), Body::Text),
            merge: Merge::Error,
            origin: origin.clone(),
            module: origin.clone(),
        })
        .collect())
}
