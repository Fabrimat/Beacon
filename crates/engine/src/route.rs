//! Workstream A1: config loading and routing (5.3 concepts 1 and 2).

use crate::config::{CONFIG_FILE, MODULE_FILE, ModuleJson, OnConflict, ProjectConfig};
use crate::diag::{Code, Diagnostic};
use crate::model::{Body, ContentItem, Dest, Generator, Merge, OutputId, Pack};
use crate::path::{RelPath, Vfs};
use crate::suffix::{is_unrouted, route_file};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::Path;

fn glob(g: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(g).literal_separator(true).build()
}

/// `./src/` -> `src`; `.` -> `None` (the root).
fn norm_source(s: &str) -> Result<Option<RelPath>, String> {
    let mut s = s;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    let s = s.trim_end_matches('/');
    if s.is_empty() || s == "." {
        return Ok(None);
    }
    RelPath::parse(s).map(Some).map_err(|e| e.to_string())
}

fn ns_ok(s: &str) -> bool {
    s != "minecraft"
        && !s.is_empty()
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// 1-based line of the last line containing every needle (the duplicate, for duplicates).
fn line_of(text: &str, needles: &[&str]) -> Option<u32> {
    let mut found = None;
    for (i, l) in text.lines().enumerate() {
        if needles.iter().all(|n| l.contains(n)) {
            found = Some(i as u32 + 1);
        }
    }
    found
}

/// Reads and validates `config.json` (4.1). Errors use `ConfigInvalid` / `ConfigField`.
pub fn load(root: &Path) -> Result<ProjectConfig, Vec<Diagnostic>> {
    let cfg = RelPath::parse(CONFIG_FILE).expect("const");
    let one =
        |code, msg: String, line, col| vec![Diagnostic::new(code, msg).at(cfg.clone(), line, col)];
    let text = std::fs::read_to_string(root.join(CONFIG_FILE)).map_err(|e| {
        one(
            Code::ConfigInvalid,
            format!("cannot read {CONFIG_FILE}: {e}"),
            None,
            None,
        )
    })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let c: ProjectConfig = serde_json::from_str(text).map_err(|e| {
        let code = if e.is_data() {
            Code::ConfigField
        } else {
            Code::ConfigInvalid
        };
        one(
            code,
            e.to_string(),
            Some(e.line() as u32),
            Some(e.column() as u32),
        )
    })?;

    let mut d = Vec::new();
    let mut bad = |msg: String, line| {
        d.push(Diagnostic::new(Code::ConfigField, msg).at(cfg.clone(), line, None))
    };
    let ns_msg =
        |n: &str| format!("namespace {n:?} must be lowercase [a-z0-9_] and not `minecraft`");
    if !ns_ok(&c.namespace) {
        bad(
            ns_msg(&c.namespace),
            line_of(text, &["\"namespace\"", &format!("\"{}\"", c.namespace)]),
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for o in &c.beacon.outputs {
        let line = |k: &str, v: &str| line_of(text, &[k, &format!("\"{v}\"")]);
        if o.id.is_empty() || !seen.insert(&o.id) {
            bad(
                format!("output id {:?} is empty or duplicated", o.id),
                line("\"id\"", &o.id),
            );
        }
        if let Some(n) = o.namespace.as_deref().filter(|n| !ns_ok(n)) {
            bad(ns_msg(n), line("\"namespace\"", n));
        }
        for g in &o.include {
            if let Err(e) = glob(g) {
                bad(
                    format!("output {:?}: bad include glob {g:?}: {e}", o.id),
                    line_of(text, &[&format!("\"{g}\"")]),
                );
            }
        }
    }
    for s in &c.beacon.sources {
        if let Err(e) = norm_source(s) {
            bad(
                format!("bad source dir {s:?}: {e}"),
                line_of(text, &[&format!("\"{s}\"")]),
            );
        }
    }
    if d.is_empty() { Ok(c) } else { Err(d) }
}

#[derive(Debug, Default)]
pub struct Routed {
    /// Convention + `module.json` items, sorted by (dest, origin). Not conflict-resolved yet.
    pub items: Vec<ContentItem>,
    pub generators: Vec<Generator>,
    /// Per BP output: game-script entries = every module's top-level `scripts/*.ts`
    /// (not `.d.ts`), sorted. Phase-2 hooks replace this rule.
    pub script_entries: BTreeMap<OutputId, Vec<RelPath>>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Every file below `dir`, sorted, as (path, module-relative path).
fn files_under(vfs: &dyn Vfs, dir: &RelPath, rel: &str, out: &mut Vec<(RelPath, String)>) {
    for name in vfs.list(Some(dir)) {
        let Ok(child) = dir.join(&name) else { continue };
        let r = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        if vfs.list(Some(&child)).is_empty() {
            out.push((child, r));
        } else {
            files_under(vfs, &child, &r, out);
        }
    }
}

/// Routes every indexed file under `beacon.sources`. Modules = first-level folders of each
/// source dir. Output: `module.json` `output`, else the first output whose `include` glob
/// matches the module folder, else the implicit `main` output when `outputs` is empty.
pub fn route(cfg: &ProjectConfig, vfs: &dyn Vfs) -> Routed {
    let mut r = Routed::default();
    let outs = &cfg.beacon.outputs;
    let sets: Vec<GlobSet> = outs
        .iter()
        .map(|o| {
            let mut b = GlobSetBuilder::new();
            o.include.iter().filter_map(|g| glob(g).ok()).for_each(|g| {
                b.add(g);
            });
            b.build().unwrap_or_else(|_| GlobSet::empty())
        })
        .collect();

    for src in &cfg.beacon.sources {
        let Ok(src) = norm_source(src) else { continue };
        for name in vfs.list(src.as_ref()) {
            let Ok(path) = src
                .as_ref()
                .map_or(RelPath::parse(&name), |s| s.join(&name))
            else {
                continue;
            };
            if vfs.list(Some(&path)).is_empty() {
                if !is_unrouted(&name) {
                    r.diagnostics.push(
                        Diagnostic::new(
                            Code::UnknownSuffix,
                            format!("{path} is not inside a module folder"),
                        )
                        .at(path, None, None),
                    );
                }
                continue;
            }
            let mut files = Vec::new();
            files_under(vfs, &path, "", &mut files);
            route_module(cfg, vfs, &sets, &name, &path, &files, &mut r);
        }
    }
    r.items
        .sort_by(|a, b| (&a.dest, &a.origin).cmp(&(&b.dest, &b.origin)));
    r.generators.sort();
    r.script_entries.values_mut().for_each(|v| v.sort());
    r
}

fn route_module(
    cfg: &ProjectConfig,
    vfs: &dyn Vfs,
    sets: &[GlobSet],
    name: &str,
    module: &RelPath,
    files: &[(RelPath, String)],
    r: &mut Routed,
) {
    let outs = &cfg.beacon.outputs;
    // module.json
    let mj_path = module.join(MODULE_FILE).ok();
    let mut mj = ModuleJson::default();
    let mj_err = |r: &mut Routed, msg: String, line: Option<u32>, col: Option<u32>| {
        let d = Diagnostic::new(Code::ModuleJsonInvalid, msg);
        r.diagnostics.push(match &mj_path {
            Some(p) => d.at(p.clone(), line, col),
            None => d,
        });
    };
    if let Some(p) = mj_path.as_ref().filter(|p| vfs.exists(p)) {
        match vfs.read(p) {
            Ok(b) => match serde_json::from_slice(
                b.strip_prefix(&[0xEF_u8, 0xBB, 0xBF][..]).unwrap_or(&b),
            ) {
                Ok(m) => mj = m,
                Err(e) => {
                    return mj_err(
                        r,
                        e.to_string(),
                        Some(e.line() as u32),
                        Some(e.column() as u32),
                    );
                }
            },
            Err(e) => return mj_err(r, format!("cannot read module.json: {e}"), None, None),
        }
    }

    // output
    let output: &str = match &mj.output {
        Some(o) if outs.iter().any(|x| &x.id == o) || (outs.is_empty() && o == "main") => o,
        Some(o) => {
            return mj_err(
                r,
                format!("output {o:?} is not defined in config.json"),
                None,
                None,
            );
        }
        None if outs.is_empty() => "main",
        None => match sets.iter().position(|s| {
            s.is_match(module.as_str()) || files.iter().any(|(p, _)| s.is_match(p.as_str()))
        }) {
            Some(i) => &outs[i].id,
            None => {
                r.diagnostics.push(
                    Diagnostic::new(
                        Code::ModuleWithoutOutput,
                        format!("module {module} matches no output include"),
                    )
                    .at(module.clone(), None, None),
                );
                return;
            }
        },
    };
    let namespace = outs
        .iter()
        .find(|o| o.id == output)
        .and_then(|o| o.namespace.as_deref())
        .unwrap_or(&cfg.namespace);

    // module-relative `paths` -> (glob, pack, dir)
    let mut paths = Vec::new();
    for (g, v) in &mj.paths {
        let target = v.split_once('/').and_then(|(p, d)| match p {
            "BP" => Some((Pack::Behavior, d)),
            "RP" => Some((Pack::Resource, d)),
            _ => None,
        });
        match (glob(g), target) {
            (Ok(g), Some((pack, dir))) => paths.push((g.compile_matcher(), pack, dir)),
            _ => mj_err(
                r,
                format!("bad paths entry {g:?}: {v:?} (want a glob and `BP/<dir>` or `RP/<dir>`)"),
                None,
                None,
            ),
        }
    }

    for (file, rel) in files {
        let fname = file.file_name();
        // `merge` only makes sense for json and lang files; skip/error apply to any file
        let policy = |default: Merge| match mj.on_conflict {
            Some(OnConflict::Merge) if !(fname.ends_with(".json") || fname.ends_with(".lang")) => {
                default
            }
            Some(c) => c.to_merge(fname),
            None => default,
        };
        let routed = match paths.iter().find(|(g, ..)| g.is_match(rel)) {
            Some((_, pack, dir)) => Some((*pack, format!("{dir}/{fname}"), policy(Merge::Error))),
            None => route_file(name, namespace, rel).map(|(p, d, m)| (p, d, policy(m))),
        };
        let Some((pack, dest, merge)) = routed else {
            if fname.ends_with(".gen.ts") {
                r.generators.push(Generator {
                    path: file.clone(),
                    module: module.clone(),
                    output: output.to_owned(),
                    namespace: namespace.to_owned(),
                });
            } else if rel
                .strip_prefix("scripts/")
                .is_some_and(|s| s.ends_with(".ts") && !s.ends_with(".d.ts") && !s.contains('/'))
            {
                r.script_entries
                    .entry(output.to_owned())
                    .or_default()
                    .push(file.clone());
            } else if !is_unrouted(fname) {
                r.diagnostics.push(
                    Diagnostic::new(
                        Code::UnknownSuffix,
                        format!("{file} matches no convention rule"),
                    )
                    .at(file.clone(), None, None),
                );
            }
            continue;
        };
        match RelPath::parse(&dest) {
            Ok(path) => r.items.push(ContentItem {
                dest: Dest {
                    output: output.to_owned(),
                    pack,
                    path,
                },
                body: Body::Passthrough(file.clone()),
                merge,
                origin: file.clone(),
                module: module.clone(),
            }),
            Err(e) => r.diagnostics.push(
                Diagnostic::new(Code::NonPortablePath, format!("{file} routes to {e}")).at(
                    file.clone(),
                    None,
                    None,
                ),
            ),
        }
    }
}
