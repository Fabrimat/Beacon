//! Project edits for `beacon add`, `beacon rename-output` (6.2) and the generated-file guard
//! behind the Claude Code `PreToolUse` hook (8.2). Errors are `(exit code, message)`.

use crate::agents::{END, FILE as AGENTS, START};
use crate::config::{CONFIG_FILE, LOCK_FILE, MODULE_FILE, ModuleJson, ProjectConfig};
use crate::diag::exit;
use crate::lock::Lockfile;
use crate::{canonical, route, sarif};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub type Res<T> = Result<T, (i32, String)>;

fn usage<T>(m: impl Into<String>) -> Res<T> {
    Err((exit::USAGE, m.into()))
}

fn io<T>(what: &str, e: std::io::Error) -> Res<T> {
    Err((exit::INTERNAL, format!("{what}: {e}")))
}

fn load(root: &Path) -> Res<ProjectConfig> {
    route::load(root).map_err(|d| (exit::DIAGNOSTICS, sarif::to_text(&d)))
}

/// Source dir as a project-relative path (`./src/` -> `src`, `.` -> ``).
fn source_rel(s: &str) -> &str {
    let mut s = s;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.trim_matches('/').trim_matches('.')
}

/// Module folders (project-relative `src/main`), sources in config order, names sorted.
fn modules(root: &Path, cfg: &ProjectConfig) -> Vec<String> {
    let mut out = Vec::new();
    for s in &cfg.beacon.sources {
        let s = source_rel(s);
        let Ok(rd) = std::fs::read_dir(root.join(s)) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        out.extend(
            names
                .into_iter()
                .map(|n| if s.is_empty() { n } else { format!("{s}/{n}") }),
        );
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{:x}.tmp", u64::from_le_bytes(rnd)));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

// ---------------------------------------------------------------- add

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Entity,
    Item,
    Block,
    Module,
}

#[derive(Debug)]
pub struct Added {
    pub module: String,
    /// Project-relative files created (or the `.lang` file appended to).
    pub files: Vec<String>,
    pub note: Option<String>,
}

fn name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn title(name: &str) -> String {
    name.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| w[..1].to_uppercase() + &w[1..])
        .collect::<Vec<_>>()
        .join(" ")
}

fn module_json(root: &Path, module: &str) -> Res<ModuleJson> {
    match std::fs::read(root.join(module).join(MODULE_FILE)) {
        Ok(b) => serde_json::from_slice(&b)
            .map_err(|e| (exit::DIAGNOSTICS, format!("{module}/{MODULE_FILE}: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ModuleJson::default()),
        Err(e) => io(&format!("{module}/{MODULE_FILE}"), e),
    }
}

/// The namespace identifiers in `module` get: its output's `namespace`, else the project's.
fn namespace_of(cfg: &ProjectConfig, module: &str, mj: &ModuleJson) -> String {
    let outs = &cfg.beacon.outputs;
    let globs = |g: &str| {
        globset::GlobBuilder::new(g)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
    };
    let hit = |g: &String| {
        globs(g).is_ok_and(|m| m.is_match(module) || m.is_match(format!("{module}/x")))
    };
    let out = match &mj.output {
        Some(id) => outs.iter().find(|o| &o.id == id),
        None => outs.iter().find(|o| o.include.iter().any(hit)),
    };
    out.and_then(|o| o.namespace.clone())
        .unwrap_or_else(|| cfg.namespace.clone())
}

/// Convention files (module-relative path, content) and the `.lang` line of one content kind.
fn templates(kind: Kind, ns: &str, name: &str) -> (Vec<(String, String)>, String) {
    let id = format!("{ns}:{name}");
    let pretty = |v: Value| serde_json::to_string_pretty(&v).expect("json") + "\n";
    match kind {
        Kind::Entity => (
            vec![
                (
                    format!("entities/{name}.entity.json"),
                    pretty(json!({
                        "format_version": "1.21.0",
                        "minecraft:entity": {
                            "description": { "identifier": id, "is_spawnable": true, "is_summonable": true },
                            "components": {
                                "minecraft:type_family": { "family": [name] },
                                "minecraft:health": { "value": 10, "max": 10 },
                                "minecraft:collision_box": { "width": 0.9, "height": 0.9 },
                                "minecraft:physics": {}
                            }
                        }
                    })),
                ),
                (
                    format!("entities/{name}.client_entity.json"),
                    pretty(json!({
                        "format_version": "1.10.0",
                        "minecraft:client_entity": {
                            "description": {
                                "identifier": id,
                                "materials": { "default": "pig" },
                                "textures": { "default": "textures/entity/pig/pig" },
                                "geometry": { "default": "geometry.pig.v3" },
                                "render_controllers": ["controller.render.pig"],
                                "spawn_egg": { "base_color": "#c0a080", "overlay_color": "#604020" }
                            }
                        }
                    })),
                ),
            ],
            format!("entity.{id}.name={}", title(name)),
        ),
        Kind::Item => (
            vec![(
                format!("items/{name}.item.json"),
                pretty(json!({
                    "format_version": "1.21.0",
                    "minecraft:item": {
                        "description": { "identifier": id, "menu_category": { "category": "items" } },
                        "components": { "minecraft:max_stack_size": 64 }
                    }
                })),
            )],
            format!("item.{id}={}", title(name)),
        ),
        Kind::Block => (
            vec![(
                format!("blocks/{name}.block.json"),
                pretty(json!({
                    "format_version": "1.21.0",
                    "minecraft:block": {
                        "description": { "identifier": id, "menu_category": { "category": "construction" } },
                        "components": { "minecraft:destructible_by_mining": { "seconds_to_destroy": 1.0 } }
                    }
                })),
            )],
            format!("tile.{id}.name={}", title(name)),
        ),
        Kind::Module => unreachable!("modules have no templates"),
    }
}

/// Scaffolds `name` into a module (default: the first module that uses convention routing).
/// Existing files are never overwritten; the `.lang` line is appended (once) to
/// `texts/en_US.lang`. A module with `packRoots` needs `force_convention` when named.
pub fn add(
    root: &Path,
    kind: Kind,
    name: &str,
    module: Option<&str>,
    force_convention: bool,
) -> Res<Added> {
    if !name_ok(name) {
        return usage(format!("name {name:?} must match [a-z0-9_]+"));
    }
    let cfg = load(root)?;
    if kind == Kind::Module {
        if module.is_some() {
            return usage("`add module` takes no --module");
        }
        let src = cfg.beacon.sources.first().map_or("", |s| source_rel(s));
        let rel = if src.is_empty() {
            name.to_string()
        } else {
            format!("{src}/{name}")
        };
        let dir = root.join(&rel);
        if dir.exists() {
            return usage(format!("{rel} already exists"));
        }
        std::fs::create_dir_all(&dir).or_else(|e| io(&rel, e))?;
        return Ok(Added {
            module: rel.clone(),
            files: vec![format!("{rel}/")],
            note: None,
        });
    }
    let mods = modules(root, &cfg);
    let roots = |m: &str| module_json(root, m).map(|j| !j.pack_roots.is_empty());
    let mut note = None;
    let module = match module {
        Some(m) => {
            let Some(found) = mods.iter().find(|x| x.rsplit('/').next() == Some(m)) else {
                return usage(format!(
                    "no module `{m}` (modules: {}); create it with `beacon add module {m}`",
                    mods.join(", ")
                ));
            };
            if roots(found)? && !force_convention {
                return usage(format!(
                    "module `{m}` uses packRoots (verbatim routing); pass --force-convention to add convention files anyway"
                ));
            }
            found.clone()
        }
        None => {
            let mut skipped = Vec::new();
            let mut pick = None;
            for m in &mods {
                if roots(m)? && !force_convention {
                    skipped.push(m.as_str());
                } else {
                    pick = Some(m.clone());
                    break;
                }
            }
            let Some(m) = pick else {
                return usage(
                    "no module with convention routing; create one with `beacon add module <name>`",
                );
            };
            if !skipped.is_empty() {
                note = Some(format!(
                    "skipped {} (packRoots); using convention module {m}",
                    skipped.join(", ")
                ));
            }
            m
        }
    };
    let ns = namespace_of(&cfg, &module, &module_json(root, &module)?);
    let (files, lang) = templates(kind, &ns, name);
    let base = root.join(&module);
    if let Some((rel, _)) = files.iter().find(|(r, _)| base.join(r).exists()) {
        return usage(format!("{module}/{rel} already exists; nothing written"));
    }
    let mut made = Vec::new();
    for (rel, text) in &files {
        let p = base.join(rel);
        let w = std::fs::create_dir_all(p.parent().expect("parent")).and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&p)
                .and_then(|mut f| f.write_all(text.as_bytes()))
        });
        w.or_else(|e| io(rel, e))?;
        made.push(format!("{module}/{rel}"));
    }
    let lang_rel = "texts/en_US.lang";
    let lp = base.join(lang_rel);
    let key = lang.split('=').next().unwrap_or_default().to_string() + "=";
    let old = std::fs::read_to_string(&lp).unwrap_or_default();
    if !old.lines().any(|l| l.starts_with(&key)) {
        let sep = if old.is_empty() || old.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        let w = std::fs::create_dir_all(lp.parent().expect("parent")).and_then(|()| {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&lp)
                .and_then(|mut f| f.write_all(format!("{sep}{lang}\n").as_bytes()))
        });
        w.or_else(|e| io(lang_rel, e))?;
        made.push(format!("{module}/{lang_rel}"));
    }
    Ok(Added {
        module,
        files: made,
        note,
    })
}

// ---------------------------------------------------------------- rename-output

const CONFIG_PATHS: &[&[&str]] = &[
    &["beacon", "outputs", "[]", "id"],
    &["beacon", "outputs", "[]", "dependsOn", "[]"],
    &["beacon", "primaryOutput"],
    &["beacon", "targets", "*", "outputs", "[]"],
];
const MODULE_PATHS: &[&[&str]] = &[&["output"]];

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Replaces the string values equal to `from` at the JSON paths `pats` (`*` = any key,
/// `[]` = array element) with `to`, leaving every other byte (order, indentation) alone.
fn rewrite(text: &str, from: &str, to: &str, pats: &[&[&str]]) -> String {
    enum F {
        Obj(String, bool),
        Arr,
    }
    let b = text.as_bytes();
    let (mut st, mut spans, mut i) = (Vec::<F>::new(), Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            b'{' => st.push(F::Obj(String::new(), true)),
            b'[' => st.push(F::Arr),
            b'}' | b']' => {
                st.pop();
            }
            b',' => {
                if let Some(F::Obj(_, k)) = st.last_mut() {
                    *k = true;
                }
            }
            b':' => {
                if let Some(F::Obj(_, k)) = st.last_mut() {
                    *k = false;
                }
            }
            b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != b'"' {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(b.len());
                let s: String = serde_json::from_str(&text[i..end]).unwrap_or_default();
                match st.last_mut() {
                    Some(F::Obj(k, true)) => *k = s,
                    _ if s == from => {
                        let path: Vec<&str> = st
                            .iter()
                            .map(|f| match f {
                                F::Obj(k, _) => k.as_str(),
                                F::Arr => "[]",
                            })
                            .collect();
                        let m = |p: &&[&str]| {
                            p.len() == path.len()
                                && p.iter().zip(&path).all(|(a, b)| *a == "*" || a == b)
                        };
                        if pats.iter().any(m) {
                            spans.push((i, end));
                        }
                    }
                    _ => {}
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let mut out = text.to_string();
    for (a, z) in spans.into_iter().rev() {
        out.replace_range(a..z, &format!("\"{to}\""));
    }
    out
}

/// Renames output `from` to `to` in `config.json`, every `module.json` and the lockfile
/// (UUIDs and releases kept). All new contents are computed first, then written atomically
/// file by file; a failed write restores the files already replaced. Returns the files changed.
pub fn rename_output(root: &Path, from: &str, to: &str) -> Res<Vec<String>> {
    if !id_ok(from) || !id_ok(to) {
        return usage("output ids must match [A-Za-z0-9_-]+");
    }
    if from == to {
        return usage("`from` and `to` are the same");
    }
    let cfg = load(root)?;
    let outs = &cfg.beacon.outputs;
    if !outs.iter().any(|o| o.id == from) {
        return usage(format!("no output `{from}` in config.json beacon.outputs"));
    }
    if outs.iter().any(|o| o.id == to) {
        return usage(format!("output `{to}` already exists"));
    }
    // (project-relative name, path, original bytes, new bytes)
    let mut plan: Vec<(String, PathBuf, Vec<u8>, Vec<u8>)> = Vec::new();
    let mut stage = |rel: &str, pats: &[&[&str]]| -> Res<()> {
        let p = root.join(rel);
        let old = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return io(rel, e),
        };
        let text =
            String::from_utf8(old.clone()).map_err(|e| (exit::DIAGNOSTICS, e.to_string()))?;
        let new = rewrite(&text, from, to, pats);
        if new != text {
            plan.push((rel.to_string(), p, old, new.into_bytes()));
        }
        Ok(())
    };
    for m in modules(root, &cfg) {
        stage(&format!("{m}/{MODULE_FILE}"), MODULE_PATHS)?;
    }
    stage(CONFIG_FILE, CONFIG_PATHS)?;
    match std::fs::read(root.join(LOCK_FILE)) {
        Ok(old) => {
            let mut lock: Lockfile = serde_json::from_slice(&old)
                .map_err(|e| (exit::DIAGNOSTICS, format!("{LOCK_FILE}: {e}")))?;
            if lock.outputs.contains_key(from) {
                lock.rename_output(from, to)
                    .map_err(|d| (exit::DIAGNOSTICS, d.message))?;
                let new = canonical::to_vec(&serde_json::to_value(&lock).expect("lock json"));
                plan.push((LOCK_FILE.into(), root.join(LOCK_FILE), old, new));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return io(LOCK_FILE, e),
    }
    for (n, (rel, p, _, new)) in plan.iter().enumerate() {
        if let Err(e) = write_atomic(p, new) {
            for (_, p, old, _) in &plan[..n] {
                let _ = write_atomic(p, old);
            }
            return io(rel, e);
        }
    }
    let mut files: Vec<String> = plan.into_iter().map(|p| p.0).collect();
    // the AGENTS.md region lists the outputs; best effort, `beacon fix` repairs a failure
    if let Ok(cfg) = route::load(root)
        && let d = crate::agents::check(root, &cfg)
        && crate::agents::apply_safe_fixes(root, &d).is_ok_and(|n| n > 0)
    {
        files.push(AGENTS.into());
    }
    Ok(files)
}

// ---------------------------------------------------------------- generated-file guard

/// How a tool call changes a file.
pub enum Edit<'a> {
    /// `Write`: the whole new content.
    Write(&'a str),
    /// `Edit` / `MultiEdit`: (old_string, new_string) pairs.
    Replace(Vec<(&'a str, &'a str)>),
    /// Cannot be inspected (`NotebookEdit`).
    Opaque,
}

/// Absolute, `.`/`..`-free form of `p` (relative paths resolve against `cwd`). Lexical: links
/// are not resolved.
pub fn normalize(cwd: &Path, p: &Path) -> PathBuf {
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Nearest ancestor of `start` (itself included) with a Beacon `config.json`.
pub fn project_root(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|d| {
        let v: Value = serde_json::from_slice(&std::fs::read(d.join(CONFIG_FILE)).ok()?).ok()?;
        v.get("beacon").is_some().then(|| d.to_path_buf())
    })
}

fn fold(p: &Path) -> Vec<String> {
    p.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            Component::Prefix(x) => Some(x.as_os_str().to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect()
}

const DEV_FOLDERS: &[&str] = &[
    "development_behavior_packs",
    "development_resource_packs",
    "development_skin_packs",
];

/// The one source of truth for generated files: why a tool must not edit `file` (a normalized
/// absolute path), or `None`. Case-folded. `root` is the project root (without it only dev
/// pack folders are known). `AGENTS.md` is protected only inside its generated region.
// ponytail: lexical paths only; a symlink or junction into a protected file is not followed.
pub fn generated_file(root: Option<&Path>, file: &Path, edit: &Edit) -> Option<String> {
    let all = fold(file);
    let name = all.last()?.as_str();
    let dev = all.iter().any(|c| DEV_FOLDERS.contains(&c.as_str()));
    let manifest_msg = "generated manifest.json files are owned by beacon; change config.json or sources and run `beacon build`";
    if name == "manifest.json" && dev {
        return Some(manifest_msg.into());
    }
    let root = root?;
    let r = fold(root);
    if !all.starts_with(&r) {
        return None;
    }
    let rel = all[r.len()..].to_vec();
    if rel == [LOCK_FILE] {
        return Some(format!(
            "{LOCK_FILE} is owned by beacon (UUIDs, releases); it is updated by `beacon build`, `beacon pack` and `beacon rename-output`"
        ));
    }
    if name == "manifest.json" {
        let mut dirs: Vec<Vec<String>> =
            vec![vec!["packs".into()], vec![".beacon".into(), "out".into()]];
        if let Some(Value::Object(p)) = std::fs::read(root.join(CONFIG_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("packs").cloned())
        {
            let s = |v: &Value| v.as_str().map(|s| fold(Path::new(source_rel(s))));
            dirs.extend(p.values().filter_map(s));
        }
        if dirs.iter().any(|d| rel.starts_with(d)) {
            return Some(manifest_msg.into());
        }
    }
    if rel == [AGENTS.to_lowercase()] {
        return agents_region(file, edit);
    }
    None
}

fn agents_region(file: &Path, edit: &Edit) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?.replace("\r\n", "\n");
    let region = |t: &str| Some((t.find(START)?, t.rfind(END)? + END.len())).filter(|r| r.0 < r.1);
    let (a, b) = region(&text)?;
    let touched = match edit {
        Edit::Opaque => true,
        Edit::Write(new) => {
            let n = new.replace("\r\n", "\n");
            region(&n).map(|(x, y)| &n[x..y]) != Some(&text[a..b])
        }
        Edit::Replace(v) => v.iter().any(|(old, new)| {
            let old = old.replace("\r\n", "\n");
            let overlaps = !old.is_empty()
                && text
                    .match_indices(&old)
                    .any(|(i, _)| i < b && i + old.len() > a);
            overlaps || new.contains(START) || new.contains(END)
        }),
    };
    touched.then(|| {
        format!("the text between the beacon:generated markers in {AGENTS} is generated; edit outside the markers, or run `beacon fix` to regenerate it")
    })
}
