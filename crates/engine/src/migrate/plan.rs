//! The migration planner (5.6, phase 3 B1): evaluated legacy output -> a native Beacon project.
//!
//! Placement, per final output `P` (`BP/...`, `RP/...`) and its contributors, in this order:
//! 1. skip / overwrite / stop are resolved statically: the winner stays, the losers are
//!    `MigrateDropped`.
//! 2. One plain source (the body is the source file unchanged): kept at the module root under
//!    its own name when Beacon's convention (`suffix::route_file`) routes it to exactly `P`,
//!    else at `src/<module>/<bp|rp>/<pack path>` under `packRoots`.
//! 3. A template (or any evaluated body): written as a static file there (`MigrateFlattened`).
//! 4. `merge` on JSON / `appendEnd` on `.lang`: one fragment per contributor (base-pack file
//!    first, in `src/_base`), merged by Beacon (`conflicts: merge`). Beacon's `merge::resolve`
//!    is run on the fragments here; when it does not reproduce the legacy result, the
//!    evaluated file is written once in `src/_merged` instead (`MigrateFlattened`).
//! 5. Base-pack files no entry touched go to `src/_base/<bp|rp>`; base manifests are not files:
//!    they become the lockfile (UUIDs, imported versions) and `packMeta.manifest` extras.
//! 6. Scripts: sources copied under `src/_scripts/` mirroring the data dir (relative imports
//!    keep working), one barrel `src/_scripts/scripts/main.ts` importing the legacy entries in
//!    bundle order.
//!
//! Then codemods in profile order, `config.json`, the lockfile and `MIGRATION.md`.

use super::codemod::{self, AutoNameOpts};
use super::regolith::map_profiles;
use super::system_template::trim;
use super::{
    Applied, Change, Contributor, Entry, EvalOutput, FilterRun, LegacyEval, LegacyKind,
    LegacyProject, MigrateOptions, MigrationPlan, Policy, Report, Template, ToPort, ToPortKind,
};
use crate::diag::{Code, Diagnostic, Severity};
use crate::lock::Lockfile;
use crate::merge::{self, strip_comments};
use crate::model::{Body, ContentItem, Dest, Edition, Merge, Pack};
use crate::path::{RelPath, Vfs};
use crate::suffix::route_file;
use crate::vfs::Index;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The one output a migrated project has.
pub const OUTPUT: &str = "main";
/// Module folders the planner creates itself (legacy modules with these names get `-legacy`).
const RESERVED: [&str; 4] = ["_base", "_merged", "_scripts", "_generated"];
const CODE_EXT: [&str; 6] = [".ts", ".js", ".mjs", ".cjs", ".mts", ".cts"];

fn rel(s: &str) -> RelPath {
    RelPath::parse(s).expect("static path")
}

fn note(code: Code, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::new(code, msg)
}

/// Project-relative `dataPath`.
pub fn data_rel(project: &LegacyProject) -> Option<RelPath> {
    let d = project.config.regolith.data_path.as_deref();
    RelPath::parse(trim(d.unwrap_or(super::DEFAULT_DATA_PATH))).ok()
}

/// Project-relative base packs (`packs.behaviorPack`, `packs.resourcePack`), `./` trimmed.
pub fn base_paths(project: &LegacyProject) -> (Option<RelPath>, Option<RelPath>) {
    let p = |s: &Option<String>| s.as_deref().and_then(|s| RelPath::parse(trim(s)).ok());
    let packs = &project.config.packs;
    (p(&packs.behavior_pack), p(&packs.resource_pack))
}

/// The legacy project as indexed for migration: `.regolith` excluded (plus `.git`,
/// `.beacon`, `node_modules`, and `--oracle` / `--evaluated` when they lie inside it); the
/// base packs are indexed (ModularMC seeds its output tree with them).
pub fn legacy_index(project: &LegacyProject, opts: &MigrateOptions) -> (Index, Vec<Diagnostic>) {
    let root = crate::build::canon(&project.root);
    let mut ex = vec![rel(".regolith")];
    for d in [&opts.oracle, &opts.evaluated].into_iter().flatten() {
        if let Ok(r) = crate::build::canon(d).strip_prefix(&root) {
            let s: Vec<String> = r
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            ex.extend(RelPath::parse(&s.join("/")).ok());
        }
    }
    Index::build(&project.root, &ex, None)
}

/// blake3 (hex) over the sorted (path, content hash) pairs of the index.
pub fn source_hash(idx: &Index) -> String {
    let mut h = blake3::Hasher::new();
    for f in idx.files() {
        h.update(f.as_str().as_bytes());
        h.update(&[0]);
        if let Ok(x) = idx.hash(f) {
            h.update(&x);
        }
    }
    h.finalize().to_hex().to_string()
}

/// A v4-shaped UUID derived from `seed` (deterministic plans: no random identities).
fn det_uuid(seed: &str) -> String {
    let b = blake3::hash(seed.as_bytes());
    let mut x = [0u8; 16];
    x.copy_from_slice(&b.as_bytes()[..16]);
    x[6] = (x[6] & 0x0f) | 0x40;
    x[8] = (x[8] & 0x3f) | 0x80;
    let h: String = x.iter().map(|v| format!("{v:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

fn pack_of(p: &str) -> Option<(Pack, &str)> {
    if let Some(r) = p.strip_prefix("BP/") {
        Some((Pack::Behavior, r))
    } else {
        p.strip_prefix("RP/").map(|r| (Pack::Resource, r))
    }
}

fn root_dir(p: Pack) -> &'static str {
    match p {
        Pack::Behavior => "bp",
        Pack::Resource => "rp",
    }
}

fn pack_name(p: Pack) -> &'static str {
    match p {
        Pack::Behavior => "BP",
        Pack::Resource => "RP",
    }
}

/// `key=value` lines (blank lines and `##` comments ignored, last duplicate wins).
fn lang_map(s: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for line in s.lines() {
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() || line.trim_start().starts_with("##") {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.split("##").next().unwrap_or("").trim_end();
            m.insert(k.trim().to_string(), v.to_string());
        }
    }
    m
}

fn lang_key(line: &str) -> Option<&str> {
    let l = line.trim_start_matches('\u{feff}');
    if l.trim_start().starts_with("##") {
        return None;
    }
    l.split_once('=')
        .map(|(k, _)| k.trim())
        .filter(|k| !k.is_empty())
}

fn json_bytes(v: &Value) -> Vec<u8> {
    let mut b = serde_json::to_vec_pretty(v).expect("Value serializes");
    b.push(b'\n');
    b
}

/// The bytes `apply` writes for a body (`Passthrough` is read through `idx`).
pub(crate) fn body_bytes(b: &Body, idx: &dyn Vfs) -> std::io::Result<Vec<u8>> {
    match b {
        Body::Passthrough(p) => idx.read(p),
        Body::Json(v) => Ok(json_bytes(v)),
        Body::Text(t) => Ok(t.clone().into_bytes()),
        Body::Bytes(b) => Ok(b.clone()),
    }
}

fn text_or_bytes(b: Vec<u8>) -> Body {
    match String::from_utf8(b) {
        Ok(t) => Body::Text(t),
        Err(e) => Body::Bytes(e.into_bytes()),
    }
}

/// The steps of a profile with nested `profile` steps inlined (cycles skipped).
fn flatten<'a>(p: &'a LegacyProject, name: &str, seen: &mut Vec<String>) -> Vec<&'a FilterRun> {
    let mut out = Vec::new();
    let steps = p.config.regolith.profiles.get(name);
    for step in steps.map_or(&[][..], |s| &s.filters[..]) {
        match step.profile.as_deref() {
            Some(n) if !seen.iter().any(|s| s == n) => {
                seen.push(n.into());
                out.extend(flatten(p, n, seen));
            }
            Some(_) => {}
            None => out.push(step),
        }
    }
    out
}

/// The Beacon targets `map_profiles` gives one Regolith profile on its own (nested profiles
/// inlined), with the edition and `targetVersion` that profile implies. The migrated config
/// merges same-named targets of several profiles (first value of a key wins), so its `world`
/// can carry another profile's `targetVersion`.
pub fn profile_targets(project: &LegacyProject, profile: &str) -> Option<Map<String, Value>> {
    let prof = project.config.regolith.profiles.get(profile)?;
    let steps: Vec<FilterRun> = flatten(project, profile, &mut vec![profile.to_string()])
        .into_iter()
        .cloned()
        .collect();
    let mut cfg = project.config.clone();
    let alone = super::Profile {
        filters: steps,
        ..prof.clone()
    };
    cfg.regolith.profiles = BTreeMap::from([(profile.to_string(), alone)]);
    map_profiles(&cfg, Some(&project.root))
        .0
        .as_object()
        .cloned()
}

/// The migrated target an oracle of `profile` is compared with: one of the profile's targets
/// whose definition in the migrated `config` has the same edition and `targetVersion`;
/// archives holding the packs directly first (`mcpack`, `mcaddon`, `mcworld`, then
/// `mctemplate`).
pub fn oracle_target(
    project: &LegacyProject,
    profile: &str,
    config: &Value,
) -> Result<String, String> {
    let own = profile_targets(project, profile).ok_or_else(|| {
        format!("--oracle-profile {profile:?} is not a profile of the legacy config")
    })?;
    let rank = |a: &str| match a {
        "mcpack" => 0,
        "mcaddon" => 1,
        "mcworld" => 2,
        "mctemplate" => 3,
        "dev" => 4,
        _ => 5,
    };
    let mut names: Vec<(&String, &Value)> = own.iter().collect();
    names.sort_by_key(|(n, t)| (rank(t["artifact"].as_str().unwrap_or("")), n.as_str()));
    let tv = |t: &Value| t.get("targetVersion").cloned();
    let targets = config["beacon"]["targets"].as_object();
    let needs_world = |m: &Value| {
        matches!(
            m["artifact"].as_str(),
            Some("mcworld" | "mctemplate" | "marketplace")
        ) && m.get("world").is_none()
    };
    names
        .into_iter()
        .find_map(|(n, t)| {
            let m = &config["beacon"]["targets"][n.as_str()];
            if !(m.is_object() && m["edition"] == t["edition"] && tv(m) == tv(t)) {
                return None;
            }
            if !needs_world(m) {
                return Some(n.clone());
            }
            // a world target without its world folder cannot build; its packs are those of
            // a world-free target with the same edition and targetVersion (`dev` first)
            let mut alt: Vec<(&String, &Value)> = targets?
                .iter()
                .filter(|(_, x)| x["edition"] == m["edition"] && tv(x) == tv(m) && !needs_world(x))
                .filter(|(_, x)| x["artifact"] != "skinpack")
                .collect();
            alt.sort_by_key(|(k, x)| (x["artifact"] != "dev", k.as_str()));
            alt.first().map(|(k, _)| (*k).clone())
        })
        .ok_or_else(|| {
            format!(
                "--oracle-profile {profile:?}: none of its targets ({}) keeps its edition and targetVersion in the migrated config",
                own.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
}

/// The profile migration follows: `default`, else the first one running a legacy filter.
fn main_profile(p: &LegacyProject) -> Vec<&FilterRun> {
    let profiles = &p.config.regolith.profiles;
    let name = if profiles.contains_key("default") {
        Some("default".to_string())
    } else {
        profiles
            .keys()
            .find(|n| {
                flatten(p, n, &mut vec![(*n).clone()]).iter().any(|s| {
                    LegacyKind::ALL
                        .iter()
                        .any(|k| s.filter.as_deref() == Some(k.filter_name()))
                })
            })
            .cloned()
    };
    name.map_or_else(Vec::new, |n| flatten(p, &n, &mut vec![n.clone()]))
}

#[derive(Default)]
struct Module {
    roots: BTreeSet<Pack>,
    conflicts: BTreeSet<String>,
}

struct File {
    change: Change,
    /// Legacy pack path (`BP/...`) for codemods; `None` for non-pack files.
    pack_path: Option<String>,
    /// A merge fragment: content codemods would duplicate in the merge, so only `.lang`
    /// line codemods run on it.
    fragment: bool,
}

struct Planner<'a> {
    project: &'a LegacyProject,
    eval: &'a LegacyEval,
    idx: &'a Index,
    ns: String,
    /// `<data>/modular_mc`, `<data>/system_template`: module names are paths below these.
    legacy_roots: Vec<String>,
    base: [Option<RelPath>; 2],
    files: BTreeMap<RelPath, File>,
    modules: BTreeMap<String, Module>,
    report: Report,
    notes: Vec<Diagnostic>,
    /// Base files superseded by an output (never copied separately).
    consumed: BTreeSet<RelPath>,
    /// Evaluated manifests (an entry targeting `BP/manifest.json`).
    manifests: [Option<Value>; 2],
    /// `pack.name` / `pack.description` values stripped from legacy lang files.
    pack_lang: BTreeMap<String, String>,
}

fn pi(p: Pack) -> usize {
    match p {
        Pack::Behavior => 0,
        Pack::Resource => 1,
    }
}

impl Planner<'_> {
    fn module_of(&self, c: &Contributor) -> String {
        let dir = c.map.parent().map(|p| p.to_string()).unwrap_or_default();
        let name = self
            .legacy_roots
            .iter()
            .find_map(|r| dir.strip_prefix(&format!("{r}/")))
            .map(str::to_string)
            .unwrap_or_else(|| {
                c.map
                    .parent()
                    .map_or("legacy".into(), |p| p.file_name().to_string())
            })
            .replace('/', "-");
        if RESERVED.contains(&name.as_str()) {
            format!("{name}-legacy")
        } else {
            name
        }
    }

    fn base_file(&self, pack: Pack, rest: &str) -> Option<RelPath> {
        let b = self.base[pi(pack)].as_ref()?.join(rest).ok()?;
        self.idx.exists(&b).then_some(b)
    }

    fn read(&self, p: &RelPath) -> Option<Vec<u8>> {
        self.idx.read(p).ok()
    }

    fn free(&self, to: &RelPath, taken: &BTreeSet<RelPath>) -> bool {
        !self.files.contains_key(to) && !taken.contains(to)
    }

    /// Where a plain source of `module` landing on `pack`/`rest` goes: the module root under
    /// its own name when convention routes it to exactly there, else the module's pack root.
    fn candidate(
        &self,
        module: &str,
        name: &str,
        pack: Pack,
        rest: &str,
        taken: &BTreeSet<RelPath>,
    ) -> Option<(RelPath, Option<Pack>)> {
        if let Some((p, d, _)) = route_file(module, &self.ns, name)
            && p == pack
            && d == rest
            && let Ok(to) = RelPath::parse(&format!("src/{module}/{name}"))
            && self.free(&to, taken)
        {
            return Some((to, None));
        }
        let to = RelPath::parse(&format!("src/{module}/{}/{rest}", root_dir(pack))).ok()?;
        self.free(&to, taken).then_some((to, Some(pack)))
    }

    fn commit(&mut self, to: RelPath, root: Option<Pack>, change: Change, pp: &str, frag: bool) {
        let module = to
            .as_str()
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let m = self.modules.entry(module.clone()).or_default();
        if let Some(p) = root {
            m.roots.insert(p);
        }
        // a `.lang` under a pack root merges with every other lang of its pack
        if frag || (root.is_some() && to.as_str().ends_with(".lang")) {
            let prefix = format!("src/{module}/");
            m.conflicts
                .insert(to.as_str().strip_prefix(&prefix).unwrap_or("").to_string());
        }
        self.files.insert(
            to,
            File {
                change,
                pack_path: Some(pp.to_string()),
                fragment: frag,
            },
        );
    }

    fn put(&mut self, to: RelPath, root: Option<Pack>, body: &Body, pp: &str, frag: bool) {
        let change = match body {
            Body::Passthrough(from) => Change::Copy {
                from: from.clone(),
                to: to.clone(),
            },
            b => Change::Write {
                to: to.clone(),
                body: b.clone(),
            },
        };
        self.commit(to, root, change, pp, frag);
    }

    fn flatten_to(&mut self, module: &str, o: &EvalOutput, pack: Pack, rest: &str, why: String) {
        let Ok(to) = RelPath::parse(&format!("src/{module}/{}/{rest}", root_dir(pack))) else {
            return;
        };
        self.put(to, Some(pack), &o.body, &o.path, false);
        self.notes
            .push(note(Code::MigrateFlattened, format!("{}: {why}", o.path)));
        self.report.flattened.push(Entry {
            path: o.path.clone(),
            why,
        });
    }

    fn drop(&mut self, path: &str, why: String) {
        self.notes
            .push(note(Code::MigrateDropped, format!("{path}: {why}")));
        self.report.dropped.push(Entry {
            path: path.into(),
            why,
        });
    }

    fn json_of(&self, b: &Body) -> Option<Value> {
        let bytes = match b {
            Body::Json(v) => return Some(v.clone()),
            b => body_bytes(b, self.idx).ok()?,
        };
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        serde_json::from_slice(&strip_comments(bytes)).ok()
    }

    fn text_of(&self, b: &Body) -> Option<String> {
        String::from_utf8(body_bytes(b, self.idx).ok()?).ok()
    }

    fn output(&mut self, o: &EvalOutput, pack: Pack, rest: &str) {
        let base = self.base_file(pack, rest);
        self.consumed.extend(base.clone());
        // 1. skip / overwrite / stop: who is still live at the end
        let mut live: Vec<&Contributor> = Vec::new();
        let mut on_base = false;
        for c in &o.contributors {
            match c.applied {
                Applied::Created | Applied::Overwrote => {
                    for d in live.drain(..) {
                        self.drop(&o.path, format!("{} overwritten by {}", d.source, c.source));
                    }
                    if on_base || (c.applied == Applied::Overwrote && base.is_some()) {
                        self.drop(&o.path, format!("base file overwritten by {}", c.source));
                    }
                    on_base = false;
                    live.push(c);
                }
                Applied::Skipped => self.drop(
                    &o.path,
                    format!("{} skipped (`skip`: the target already existed)", c.source),
                ),
                Applied::Merged | Applied::Appended => {
                    if live.is_empty() {
                        on_base = true;
                    }
                    live.push(c);
                }
            }
        }
        let base_part = if on_base { base.clone() } else { None };
        match (live.len(), &base_part) {
            (0, _) if o.contributors.is_empty() => {
                // output of another filter (subfunctions, custom blocks, esbuild)
                self.flatten_to(
                    "_generated",
                    o,
                    pack,
                    rest,
                    "produced by another legacy filter; kept as a static file".into(),
                );
            }
            (0, _) => {
                // every entry skipped: the base file wins
                let Some(b) = base.clone() else {
                    return self.flatten_to("_merged", o, pack, rest, "no live source".into());
                };
                let to = RelPath::parse(&format!("src/_base/{}/{rest}", root_dir(pack)));
                if let Ok(to) = to {
                    self.put(to, Some(pack), &Body::Passthrough(b), &o.path, false);
                }
            }
            (1, None) => self.single(o, live[0], pack, rest),
            _ => self.fragments(o, &live, base_part, pack, rest),
        }
    }

    fn single(&mut self, o: &EvalOutput, c: &Contributor, pack: Pack, rest: &str) {
        let module = self.module_of(c);
        let plain = c.template == Template::None && o.body == Body::Passthrough(c.source.clone());
        if plain {
            if let Some((to, root)) =
                self.candidate(&module, c.source.file_name(), pack, rest, &BTreeSet::new())
            {
                self.put(to, root, &o.body, &o.path, false);
            } else {
                self.flatten_to("_merged", o, pack, rest, "path taken in its module".into());
            }
            return;
        }
        let why = match &c.template {
            Template::Json(s) => format!("jsonTemplate from {}, scope {s}", c.source),
            Template::Text(s) => format!("textTemplate from {}, scope {s}", c.source),
            Template::None if is_unpacked(&o.path, &c.source) => format!(
                "unpacked subfunction of {} (`UNPACK:SUBFUNCTION`), kept expanded",
                c.source
            ),
            Template::None => format!("evaluated from {} (template or expansion)", c.source),
        };
        let to = RelPath::parse(&format!("src/{module}/{}/{rest}", root_dir(pack)));
        match to {
            Ok(to) if self.free(&to, &BTreeSet::new()) => {
                self.put(to, Some(pack), &o.body, &o.path, false);
                self.notes
                    .push(note(Code::MigrateFlattened, format!("{}: {why}", o.path)));
                self.report.flattened.push(Entry {
                    path: o.path.clone(),
                    why,
                });
            }
            _ => self.flatten_to("_merged", o, pack, rest, why),
        }
    }

    /// Merge fragments, checked against Beacon's own merge; flattened on any difference.
    fn fragments(
        &mut self,
        o: &EvalOutput,
        live: &[&Contributor],
        base: Option<RelPath>,
        pack: Pack,
        rest: &str,
    ) {
        let first_created = base.is_none();
        let tail = if first_created { &live[1..] } else { live };
        let json = rest.ends_with(".json") && tail.iter().all(|c| c.applied == Applied::Merged);
        let lang = rest.ends_with(".lang")
            && tail
                .iter()
                .all(|c| c.applied == Applied::Appended && c.policy == Policy::AppendEnd);
        let plain = live.iter().all(|c| c.template == Template::None);
        if !(json || lang) || !plain {
            let what = live
                .iter()
                .map(|c| format!("{} ({:?})", c.source, c.applied))
                .collect::<Vec<_>>()
                .join(", ");
            return self.flatten_to(
                "_merged",
                o,
                pack,
                rest,
                format!("combined by the legacy tool ({what}); written pre-merged"),
            );
        }
        // (from, to, root)
        let mut parts: Vec<(RelPath, RelPath, Option<Pack>)> = Vec::new();
        let mut taken = BTreeSet::new();
        if let Some(b) = &base {
            let Ok(to) = RelPath::parse(&format!("src/_base/{}/{rest}", root_dir(pack))) else {
                return;
            };
            taken.insert(to.clone());
            parts.push((b.clone(), to, Some(pack)));
        }
        for c in live {
            let module = self.module_of(c);
            match self.candidate(&module, c.source.file_name(), pack, rest, &taken) {
                Some((to, root)) => {
                    taken.insert(to.clone());
                    parts.push((c.source.clone(), to, root));
                }
                None => {
                    return self.flatten_to(
                        "_merged",
                        o,
                        pack,
                        rest,
                        "fragment paths collide; written pre-merged".into(),
                    );
                }
            }
        }
        // `.lang`: a key set again by a later fragment is dropped from the earlier one
        let mut bodies: Vec<Body> = parts
            .iter()
            .map(|(f, ..)| Body::Passthrough(f.clone()))
            .collect();
        let mut codemods = Vec::new();
        if lang {
            let texts: Vec<Option<String>> = parts
                .iter()
                .map(|(f, ..)| String::from_utf8(self.read(f)?).ok())
                .collect();
            for (i, t) in texts.iter().enumerate() {
                let Some(t) = t else { continue };
                let later: BTreeSet<&str> = texts[i + 1..]
                    .iter()
                    .flatten()
                    .flat_map(|t| t.lines().filter_map(lang_key))
                    .collect();
                let kept: String = t
                    .split_inclusive('\n')
                    .filter(|l| !lang_key(l).is_some_and(|k| later.contains(k)))
                    .collect();
                let (kept, why) = match codemod::lang_dedupe(&o.path, kept.as_bytes()) {
                    Some((b, e)) => (String::from_utf8(b).unwrap_or_default(), Some(e.why)),
                    None => (kept, None),
                };
                if kept != *t {
                    let why = why.unwrap_or_else(|| "keys set again by a later fragment".into());
                    codemods.push(Entry {
                        path: parts[i].1.to_string(),
                        why,
                    });
                    bodies[i] = Body::Text(kept);
                }
            }
        }
        let merge = if lang {
            Merge::LangLines
        } else {
            Merge::JsonDeep
        };
        let items: Vec<ContentItem> = parts
            .iter()
            .zip(&bodies)
            .map(|((_, to, _), body)| ContentItem {
                dest: Dest {
                    output: OUTPUT.into(),
                    pack,
                    path: RelPath::parse(rest).expect("checked by the caller"),
                },
                body: body.clone(),
                merge,
                origin: to.clone(),
                module: to.parent().unwrap_or_else(|| to.clone()),
            })
            .collect();
        let (merged, diags) = merge::resolve(items, self.idx);
        let same = diags.is_empty()
            && merged.len() == 1
            && if lang {
                let a = self.text_of(&merged[0].body).map(|t| lang_map(&t));
                a.is_some() && a == self.text_of(&o.body).map(|t| lang_map(&t))
            } else {
                let a = self.json_of(&merged[0].body);
                a.is_some() && a == self.json_of(&o.body)
            };
        if !same {
            let why = match diags.first() {
                Some(d) => format!("Beacon's merge differs ({}); written pre-merged", d.message),
                None => "Beacon's merge gives a different result; written pre-merged".into(),
            };
            return self.flatten_to("_merged", o, pack, rest, why);
        }
        for ((_, to, root), body) in parts.into_iter().zip(bodies) {
            self.put(to, root, &body, &o.path, true);
        }
        self.report.codemods.extend(codemods);
    }

    fn base_files(&mut self) {
        for (i, pack) in [Pack::Behavior, Pack::Resource].into_iter().enumerate() {
            let Some(dir) = self.base[i].clone() else {
                continue;
            };
            let prefix = format!("{dir}/");
            let files: Vec<RelPath> = self
                .idx
                .files()
                .into_iter()
                .filter(|f| f.as_str().starts_with(&prefix))
                .cloned()
                .collect();
            for f in files {
                let rest = &f.as_str()[prefix.len()..];
                if rest == "manifest.json" || self.consumed.contains(&f) {
                    continue;
                }
                if self.unpacked(&f, pack, rest) {
                    let pp = format!("{}/{rest}", pack_name(pack));
                    let why = "an `UNPACK:SUBFUNCTION` source; its unpacked functions were migrated instead";
                    self.drop(&pp, why.into());
                    continue;
                }
                let Ok(to) = RelPath::parse(&format!("src/_base/{}/{rest}", root_dir(pack))) else {
                    continue;
                };
                let pp = format!("{}/{rest}", pack_name(pack));
                // base langs merge with module langs (and Beacon's `pack.name` lines)
                let frag = rest.ends_with(".lang");
                self.put(to, Some(pack), &Body::Passthrough(f.clone()), &pp, frag);
            }
        }
    }

    /// A base `X.mcfunction` the subfunctions filter replaced by the folder `X/`.
    fn unpacked(&self, f: &RelPath, pack: Pack, rest: &str) -> bool {
        let Some(stem) = rest.strip_suffix(".mcfunction") else {
            return false;
        };
        let dir = format!("{}/{stem}/", pack_name(pack));
        let marked = self.read(f).is_some_and(|b| {
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            b.starts_with(b"UNPACK:SUBFUNCTION")
        });
        marked && self.eval.outputs.keys().any(|k| k.starts_with(&dir))
    }

    /// Script sources under `src/_scripts/` (data-dir layout) and the barrel.
    fn scripts(&mut self, data: &RelPath) -> bool {
        let mut barrel = String::new();
        let mmc = format!("{data}/modular_mc/");
        let dprefix = format!("{data}/");
        let mirror = |p: &RelPath| {
            let r = p.as_str().strip_prefix(&dprefix)?;
            RelPath::parse(&format!("src/_scripts/{r}")).ok()
        };
        if !self.eval.scripts.is_empty() {
            let code: Vec<RelPath> = self
                .idx
                .files()
                .into_iter()
                .filter(|f| {
                    let n = f.file_name();
                    f.as_str().starts_with(&mmc)
                        // maps too: game scripts import constants from them (`./_map.js`)
                        && CODE_EXT.iter().any(|e| n.ends_with(e))
                })
                .cloned()
                .chain(self.eval.scripts.iter().cloned())
                .collect();
            for f in code {
                match mirror(&f) {
                    Some(to) => {
                        let ch = Change::Copy {
                            from: f.clone(),
                            to: to.clone(),
                        };
                        self.modules.entry("_scripts".into()).or_default();
                        // text_replacer rewrote the legacy bundle: same rules on the sources
                        let pack_path = to
                            .as_str()
                            .strip_prefix("src/_scripts/")
                            .map(|r| format!("BP/scripts/{r}"));
                        self.files.insert(
                            to,
                            File {
                                change: ch,
                                pack_path,
                                fragment: false,
                            },
                        );
                    }
                    None => self.notes.push(
                        note(
                            Code::MigrateUnsupported,
                            format!("script {f} is outside the data dir; not copied"),
                        )
                        .warn(),
                    ),
                }
            }
            for s in &self.eval.scripts {
                if let Some(r) = s.as_str().strip_prefix(&dprefix) {
                    barrel.push_str(&format!("import \"../{r}\";\n"));
                }
            }
        }
        // system_template: the esbuild root(s) `data/<X>/` (a `subscripts/` folder and usually
        // the `main.ts` system_template wrote the import lines into), mirrored like ModularMC's
        let subs: Vec<(String, Body)> = self
            .eval
            .outputs
            .values()
            .filter(|o| is_st_script(&o.path, &esbuild_roots(self.eval)))
            .map(|o| (o.path["data/".len()..].to_string(), o.body.clone()))
            .collect();
        let mut roots = BTreeSet::new();
        for (r, body) in &subs {
            if let Some((root, _)) = r.split_once("/subscripts/") {
                roots.insert(root.to_string());
            }
            let Ok(to) = RelPath::parse(&format!("src/_scripts/{r}")) else {
                continue;
            };
            self.modules.entry("_scripts".into()).or_default();
            let change = match body {
                Body::Passthrough(f) => Change::Copy {
                    from: f.clone(),
                    to: to.clone(),
                },
                b => Change::Write {
                    to: to.clone(),
                    body: b.clone(),
                },
            };
            let pack_path = Some(format!("BP/scripts/{r}"));
            self.files.insert(
                to,
                File {
                    change,
                    pack_path,
                    fragment: false,
                },
            );
        }
        for root in roots {
            let has = |n: &str| subs.iter().any(|(r, _)| *r == format!("{root}/{n}"));
            if let Some(m) = ["main.ts", "main.js"].into_iter().find(|n| has(n)) {
                barrel.push_str(&format!(
                    "import \"../{root}/{m}\";
"
                ));
                continue;
            }
            let prefix = format!("{root}/subscripts/");
            for (r, _) in &subs {
                if let Some(n) = r.strip_prefix(&prefix)
                    && !n.contains('/')
                    && (n.ends_with(".ts") || n.ends_with(".js"))
                {
                    barrel.push_str(&format!(
                        "import \"../{r}\";
"
                    ));
                }
            }
        }
        if barrel.is_empty() {
            return false;
        }
        let to = rel("src/_scripts/scripts/main.ts");
        let body = format!(
            "// Script entry of the migrated project: the legacy bundle's entries, in bundle order.\n{barrel}"
        );
        self.files.insert(
            to.clone(),
            File {
                change: Change::Write {
                    to,
                    body: Body::Text(body),
                },
                pack_path: None,
                fragment: false,
            },
        );
        true
    }
}

trait Warn {
    fn warn(self) -> Self;
}

impl Warn for Diagnostic {
    fn warn(mut self) -> Self {
        self.severity = Severity::Warning;
        self
    }
}

/// `1.2.3` from `[1, 2, 3]` or `"1.2.3"`.
fn version_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let p: Option<Vec<String>> = a.iter().map(|n| Some(n.as_u64()?.to_string())).collect();
            p.filter(|p| !p.is_empty()).map(|p| p.join("."))
        }
        _ => None,
    }
}

/// `packMeta.manifest` extras of a legacy manifest: everything Beacon does not generate.
fn extras(m: &Value, own: &[&str]) -> Option<Value> {
    let mut m = m.as_object()?.clone();
    for k in ["format_version", "modules"] {
        m.remove(k);
    }
    if let Some(Value::Object(h)) = m.get_mut("header") {
        for k in ["uuid", "version", "min_engine_version"] {
            h.remove(k);
        }
        for (k, key) in [("name", "pack.name"), ("description", "pack.description")] {
            if h.get(k).and_then(Value::as_str) == Some(key) {
                h.remove(k);
            }
        }
        if h.is_empty() {
            m.remove("header");
        }
    }
    // dependencies: Beacon writes its own RP and script modules; others are kept
    if let Some(Value::Array(d)) = m.remove("dependencies") {
        let foreign: Vec<Value> = d
            .into_iter()
            .filter(|x| x.get("module_name").is_none())
            .filter(|x| !x["uuid"].as_str().is_some_and(|u| own.contains(&u)))
            .collect();
        if !foreign.is_empty() {
            m.insert("dependencies".into(), Value::Array(foreign));
        }
    }
    (!m.is_empty()).then_some(Value::Object(m))
}

fn identifier_ns(v: &Value, counts: &mut BTreeMap<String, usize>) {
    match v {
        Value::Object(o) => {
            if let Some(id) = o.get("identifier").and_then(Value::as_str)
                && let Some((ns, _)) = id.split_once(':')
                && crate::route::ns_ok(ns)
            {
                *counts.entry(ns.to_string()).or_default() += 1;
            }
            o.values().for_each(|x| identifier_ns(x, counts));
        }
        Value::Array(a) => a.iter().for_each(|x| identifier_ns(x, counts)),
        _ => {}
    }
}

/// `BP/functions/a/x/setup.mcfunction` from source `.../x.mcfunction`: a function the
/// subfunctions filter unpacked from an `UNPACK:SUBFUNCTION` source into the folder `x/`.
fn is_unpacked(out: &str, source: &RelPath) -> bool {
    source
        .file_name()
        .strip_suffix(".mcfunction")
        .is_some_and(|stem| out.contains(&format!("/{stem}/")) && out.ends_with(".mcfunction"))
}

/// `text_replacer` on a script source. The legacy filter ran on the finished bundle, where
/// imports are already resolved; here module specifiers must survive (`@shapescape/x` is a
/// package name), so `import` / `export ... from` lines are left alone.
/// ponytail: line-based; a specifier on a line of its own inside a multi-line import is
/// covered by the `from` check, a dynamic `import("...")` is not.
fn script_replace(path: &str, bytes: &[u8], batches: &[codemod::Batch]) -> codemod::Rewrite {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut entry = None;
    let out: String = text
        .split_inclusive('\n')
        .map(|l| {
            let t = l.trim_start();
            let spec = t.starts_with("import ")
                || t.starts_with("import\"")
                || t.starts_with("import'")
                || l.contains(" from \"")
                || l.contains(" from '")
                || l.contains("require(");
            match (spec, codemod::text_replace(path, l.as_bytes(), batches)) {
                (false, Some((b, e))) => {
                    entry = Some(e);
                    String::from_utf8(b).unwrap_or_else(|_| l.to_string())
                }
                _ => l.to_string(),
            }
        })
        .collect();
    let mut e = entry?;
    e.why = "text_replacer (module specifiers kept)".into();
    Some((out.into_bytes(), e))
}

/// system_template_esbuild roots in the evaluated tree: `data/<X>/` folders holding a
/// `subscripts/` folder (`data/system_template/` is the systems' input, not a root).
fn esbuild_roots(eval: &LegacyEval) -> BTreeSet<String> {
    eval.outputs
        .keys()
        .filter(|k| k.starts_with("data/") && !k.starts_with("data/system_template/"))
        .filter(|k| !k.contains("/node_modules/"))
        .filter_map(|k| k.split_once("/subscripts/").map(|(r, _)| format!("{r}/")))
        .collect()
}

/// A script source of the esbuild root (logged by system_template or not).
fn is_st_script(path: &str, roots: &BTreeSet<String>) -> bool {
    roots.iter().any(|r| path.starts_with(r.as_str())) && CODE_EXT.iter().any(|e| path.ends_with(e))
}

/// regolith_subfunctions syntax in an `.mcfunction` (its evaluator would rewrite the file).
fn has_subfunctions(text: &str) -> bool {
    text.lines().any(|l| {
        let t = l.trim_start();
        [
            "for <",
            "if <",
            "function <",
            "definefunction <",
            "var ",
            "UNPACK:",
        ]
        .iter()
        .any(|p| t.starts_with(p))
            || (!t.starts_with('#') && l.contains("`eval:"))
    })
}

/// Hint for unresolved imports of gitignored generated sources.
fn hint(mut d: Diagnostic) -> Diagnostic {
    if d.message.contains("cannot resolve") && d.message.contains("generated/") {
        d.message
            .push_str(" (generated by a pre-build script: run `npm run preregolith` first)");
    }
    d
}

pub(super) fn plan(
    project: &LegacyProject,
    eval: &LegacyEval,
    opts: &MigrateOptions,
) -> Result<MigrationPlan, Vec<Diagnostic>> {
    let errs: Vec<Diagnostic> = eval
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .cloned()
        .map(hint)
        .collect();
    if !errs.is_empty() {
        return Err(errs);
    }
    if let Some(n) = opts.namespace.as_deref()
        && !crate::route::ns_ok(n)
    {
        return Err(vec![note(
            Code::MigrateUnsupported,
            format!("--namespace {n:?} must be lowercase [a-z0-9_] and not `minecraft`"),
        )]);
    }
    let (idx, idx_diags) = legacy_index(project, opts);
    let data = data_rel(project);
    let (bp, rp) = base_paths(project);
    let legacy_roots = data
        .iter()
        .flat_map(|d| {
            LegacyKind::ALL
                .iter()
                .map(move |k| format!("{d}/{}", k.filter_name()))
        })
        .collect();

    // namespace: the most common identifier namespace of the evaluated JSON
    let mut counts = BTreeMap::new();
    let mut p0 = Planner {
        project,
        eval,
        idx: &idx,
        ns: String::new(),
        legacy_roots,
        base: [bp.clone(), rp.clone()],
        files: BTreeMap::new(),
        modules: BTreeMap::new(),
        report: Report::default(),
        notes: idx_diags.into_iter().map(|d| d.warn()).collect(),
        consumed: BTreeSet::new(),
        manifests: [None, None],
        pack_lang: BTreeMap::new(),
    };
    for o in eval.outputs.values().filter(|o| o.path.ends_with(".json")) {
        if let Some(v) = p0.json_of(&o.body) {
            identifier_ns(&v, &mut counts);
        }
    }
    p0.ns = match opts.namespace.clone() {
        Some(n) => n,
        None => {
            match counts.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) {
                Some((n, _)) => n.clone(),
                None => {
                    let name = project
                        .config
                        .name
                        .clone()
                        .unwrap_or_default()
                        .to_lowercase();
                    let slug: String = name
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                        .collect();
                    let n = if crate::route::ns_ok(&slug) {
                        slug
                    } else {
                        "migrated".into()
                    };
                    p0.notes.push(note(
                    Code::MigrateToPort,
                    format!("namespace `{n}` derived from the project name; pass --namespace to choose"),
                ));
                    n
                }
            }
        }
    };
    let mut p = p0;

    // outputs
    let mut skip_bundle: BTreeSet<String> = BTreeSet::new();
    let es_roots = esbuild_roots(eval);
    let has_subscripts = !es_roots.is_empty();
    let mut st_entry: Option<String> = None;
    if has_subscripts {
        // the legacy esbuild bundle of the subscripts: rebuilt from the barrel
        if let Some(o) = eval.outputs.values().find(|o| {
            o.contributors.is_empty()
                && o.path.starts_with("BP/scripts/")
                && o.path.ends_with(".js")
        }) {
            st_entry = Some(o.path["BP/".len()..].to_string());
            skip_bundle.insert(o.path.clone());
            skip_bundle.insert(format!("{}.map", o.path));
        }
    }
    let st_inputs: BTreeSet<&RelPath> = eval
        .outputs
        .values()
        .flat_map(|o| o.contributors.iter().map(|c| &c.source))
        .collect();
    for (path, o) in &eval.outputs {
        if skip_bundle.contains(path) {
            continue;
        }
        let Some((pack, rest)) = pack_of(path) else {
            if path.starts_with("data/") {
                if is_st_script(path, &es_roots) {
                    continue; // scripts, below
                }
                let consumed = data.as_ref().is_some_and(|d| {
                    RelPath::parse(&format!("{d}/{}", &path["data/".len()..]))
                        .is_ok_and(|r| st_inputs.contains(&r))
                });
                if consumed {
                    continue;
                }
                let why = "a `data/` target (input of later filters) is not migrated".to_string();
                p.notes
                    .push(note(Code::MigrateUnsupported, format!("{path}: {why}")).warn());
                p.report.dropped.push(Entry {
                    path: path.clone(),
                    why,
                });
            } else {
                p.drop(path, "not under BP/, RP/ or data/".into());
            }
            continue;
        };
        if RelPath::parse(rest).is_err() {
            p.notes.push(note(
                Code::MigrateUnsupported,
                format!("{path}: not a portable pack path; skipped"),
            ));
            continue;
        }
        if rest == "manifest.json" {
            p.manifests[pi(pack)] = p.json_of(&o.body);
            p.consumed.extend(p.base_file(pack, rest));
            continue;
        }
        p.output(o, pack, rest);
    }
    p.base_files();
    let scripted = match &data {
        Some(d) => p.scripts(d),
        None => false,
    };

    // codemods
    let steps = main_profile(project);
    let mmc_settings = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("modular_mc"))
        .and_then(|s| s.settings.clone());
    codemods(&mut p, &steps);

    // manifests -> lock + extras
    let read_manifest = |i: usize| -> Option<Value> {
        let m = p.manifests[i].clone();
        m.or_else(|| {
            let f = p.base[i].as_ref()?.join("manifest.json").ok()?;
            let b = p.read(&f)?;
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            serde_json::from_slice(&strip_comments(b)).ok()
        })
    };
    let (bpm, rpm) = (read_manifest(0), read_manifest(1));
    let (targets, tdiags) = map_profiles(&project.config, Some(&project.root));
    let editions: BTreeSet<&str> = targets
        .as_object()
        .into_iter()
        .flat_map(|t| t.values())
        .filter_map(|t| t["edition"].as_str())
        .collect();
    let shared = editions.len() > 1;
    let edition = if shared {
        None
    } else if editions.contains("education") {
        Some(Edition::Education)
    } else {
        Some(Edition::Bedrock)
    };
    let mut lock = Lockfile::default();
    if let Err(d) = lock.import_manifests(OUTPUT, edition, bpm.as_ref(), rpm.as_ref()) {
        return Err(vec![d]);
    }
    // identities not taken from a manifest: derived, so the plan is deterministic
    let mut known = BTreeSet::new();
    for m in [&bpm, &rpm].into_iter().flatten() {
        known.extend(m["header"]["uuid"].as_str().map(String::from));
        for x in m["modules"].as_array().into_iter().flatten() {
            known.extend(x["uuid"].as_str().map(String::from));
        }
    }
    let seed = project.config.name.clone().unwrap_or_default();
    if let Some(o) = lock.outputs.get_mut(OUTPUT) {
        let fix = |u: &mut String, what: &str| {
            if !known.contains(u.as_str()) {
                *u = det_uuid(&format!("beacon migrate/{seed}/{what}"));
            }
        };
        fix(&mut o.script_module, "script");
        for (name, e) in o
            .shared
            .iter_mut()
            .map(|e| ("shared".to_string(), e))
            .chain(o.editions.iter_mut().map(|(k, e)| (format!("{k:?}"), e)))
        {
            fix(&mut e.bp.header, &format!("{name}/bp/header"));
            fix(&mut e.bp.module, &format!("{name}/bp/module"));
            fix(&mut e.rp.header, &format!("{name}/rp/header"));
            fix(&mut e.rp.module, &format!("{name}/rp/module"));
        }
    }
    let own: Vec<&str> = [&bpm, &rpm]
        .into_iter()
        .flatten()
        .filter_map(|m| m["header"]["uuid"].as_str())
        .collect();
    let mut manifest_extra = Map::new();
    for (k, m) in [("BP", &bpm), ("RP", &rpm)] {
        if let Some(x) = m.as_ref().and_then(|m| extras(m, &own)) {
            manifest_extra.insert(k.into(), x);
        }
    }
    let script_modules: Map<String, Value> = bpm
        .as_ref()
        .and_then(|m| m["dependencies"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let n = d["module_name"].as_str()?;
            Some((n.to_string(), json!(version_string(&d["version"])?)))
        })
        .collect();
    let target_version = [&bpm, &rpm]
        .into_iter()
        .flatten()
        .find_map(|m| version_string(&m["header"]["min_engine_version"]))
        .unwrap_or_else(|| crate::init::DEFAULT_TARGET_VERSION.into());

    // config.json
    let cfg = &project.config;
    let name = cfg.name.clone().unwrap_or_else(|| {
        project
            .root
            .file_name()
            .map_or("migrated".into(), |n| n.to_string_lossy().into_owned())
    });
    let mut beacon = json!({ "sources": ["./src"], "targets": targets });
    let b = beacon.as_object_mut().expect("object");
    // legacy manifests naming themselves by lang key: the stripped lang values
    let mut meta = Map::new();
    for (field, key) in [("name", "pack.name"), ("description", "pack.description")] {
        let by_key = [&bpm, &rpm]
            .into_iter()
            .flatten()
            .any(|m| m["header"][field].as_str() == Some(key));
        if let (true, Some(v)) = (by_key, p.pack_lang.get(key)) {
            meta.insert(field.into(), json!(v));
        }
    }
    // pack dependency directions of the legacy manifests: `packMeta.packDependencies`
    // (`bp-rp`, Beacon's default, needs nothing)
    let depends = |m: &Option<Value>, on: &Option<Value>| {
        let Some(u) = on.as_ref().and_then(|o| o["header"]["uuid"].as_str()) else {
            return false;
        };
        m.as_ref()
            .and_then(|m| m["dependencies"].as_array())
            .is_some_and(|d| d.iter().any(|x| x["uuid"].as_str() == Some(u)))
    };
    match (depends(&bpm, &rpm), depends(&rpm, &bpm)) {
        (true, true) => {
            meta.insert("packDependencies".into(), json!("mutual"));
        }
        (false, true) => {
            meta.insert("packDependencies".into(), json!("rp-bp"));
        }
        _ => {}
    }
    if !manifest_extra.is_empty() {
        meta.insert("manifest".into(), Value::Object(manifest_extra));
    }
    if !meta.is_empty() {
        b.insert("packMeta".into(), Value::Object(meta));
    }
    if !script_modules.is_empty() {
        b.insert("scriptModules".into(), Value::Object(script_modules));
    }
    if shared {
        b.insert("sharedEditionUuids".into(), json!(true));
    }
    let entry = match (&st_entry, &mmc_settings) {
        (Some(e), _) => Some(e.clone()),
        (None, Some(s)) => s["esbuild"]["settings"]["outfile"].as_str().map(|o| {
            o.trim_start_matches("./")
                .trim_start_matches("BP/")
                .to_string()
        }),
        _ => None,
    };
    // a legacy bundle left in the base pack would collide with Beacon's own bundle
    if scripted {
        let e = entry
            .clone()
            .unwrap_or_else(|| crate::config::DEFAULT_SCRIPT_ENTRY.into());
        for x in [e.clone(), format!("{e}.map")] {
            if let Ok(to) = RelPath::parse(&format!("src/_base/bp/{x}"))
                && p.files.remove(&to).is_some()
            {
                p.drop(
                    &format!("BP/{x}"),
                    "a stale legacy bundle in the base pack; Beacon bundles the scripts".into(),
                );
            }
        }
    }
    if scripted && let Some(e) = entry.filter(|e| e != crate::config::DEFAULT_SCRIPT_ENTRY) {
        b.insert(
            "outputs".into(),
            json!([{ "id": OUTPUT, "include": ["src/**"], "scriptEntry": e }]),
        );
    }
    name_ninja_config(&mut p, &steps, b);
    if steps
        .iter()
        .any(|s| s.filter.as_deref() == Some("texture_list"))
    {
        b.insert("texturesList".into(), json!(true));
        if rpm.as_ref().is_some_and(|m| m.get("subpacks").is_some()) {
            p.report.to_port.push(ToPort {
                kind: ToPortKind::LegacyFilter,
                path: rel("config.json"),
                why: "texture_list with subpacks: Beacon lists the main RP only".into(),
            });
        }
    }
    let mut config = json!({
        "type": "minecraftBedrock",
        "name": name,
        "targetVersion": target_version,
        "namespace": p.ns,
        "packs": {
            "behaviorPack": cfg.packs.behavior_pack.clone().unwrap_or_else(|| "./packs/BP".into()),
            "resourcePack": cfg.packs.resource_pack.clone().unwrap_or_else(|| "./packs/RP".into()),
        },
        "beacon": beacon,
    });
    if let Some(a) = &cfg.author {
        config["authors"] = json!([a]);
    }
    if !cfg.worlds.is_empty() {
        config["worlds"] = json!(cfg.worlds);
    }

    // worlds referenced by targets or `worlds` globs are project content
    let mut world_dirs: BTreeSet<String> = cfg
        .worlds
        .iter()
        .map(|w| trim(w.trim_end_matches("/*").trim_end_matches('*')).to_string())
        .collect();
    for t in config["beacon"]["targets"]
        .as_object()
        .into_iter()
        .flatten()
    {
        world_dirs.extend(t.1["world"].as_str().map(|w| trim(w).to_string()));
    }
    for f in idx.files() {
        if world_dirs
            .iter()
            .any(|d| !d.is_empty() && f.as_str().starts_with(&format!("{d}/")))
        {
            p.files.insert(
                f.clone(),
                File {
                    change: Change::Copy {
                        from: f.clone(),
                        to: f.clone(),
                    },
                    pack_path: None,
                    fragment: false,
                },
            );
        }
    }

    // to port, notes
    if let Some(o) = &opts.oracle
        && steps
            .iter()
            .any(|s| s.filter.as_deref() == Some("subfunctions"))
    {
        freeze_subfunctions(&mut p, o);
    }
    to_port(&mut p, &steps, &tdiags);
    for d in tdiags {
        p.notes.push(d);
    }
    for (k, v) in &eval.env_read {
        let seen = v.as_deref().map_or("unset".into(), |v| format!("{v:?}"));
        p.notes.push(note(
            Code::MigrateEnvRead,
            format!("process.env.{k} was {seen} during migration; its branch was evaluated once"),
        ));
    }
    p.notes.extend(
        eval.diagnostics
            .iter()
            .filter(|d| d.severity != Severity::Error)
            .cloned(),
    );
    npm_deps(&mut p, data.as_ref());

    // module.json per module
    for (m, info) in &p.modules {
        let mut mj = Map::new();
        if !info.roots.is_empty() {
            let roots: Map<String, Value> = info
                .roots
                .iter()
                .map(|r| (pack_name(*r).to_string(), json!(root_dir(*r))))
                .collect();
            mj.insert("packRoots".into(), Value::Object(roots));
        }
        if !info.conflicts.is_empty() {
            let c: Map<String, Value> = info
                .conflicts
                .iter()
                .map(|g| (g.clone(), json!("merge")))
                .collect();
            mj.insert("conflicts".into(), Value::Object(c));
        }
        // registries come from the migrated files, never derived from sibling textures
        mj.insert("autoTextures".into(), json!(false));
        if let Ok(to) = RelPath::parse(&format!("src/{m}/module.json")) {
            let body = Body::Json(Value::Object(mj));
            let change = Change::Write {
                to: to.clone(),
                body,
            };
            let f = File {
                change,
                pack_path: None,
                fragment: false,
            };
            p.files.insert(to, f);
        }
    }

    // report
    let r = &mut p.report;
    r.modules = p
        .modules
        .keys()
        .filter_map(|m| RelPath::parse(&format!("src/{m}")).ok())
        .collect();
    r.source_hash = source_hash(&idx);
    r.files = p.files.len() + 1; // + MIGRATION.md
    r.flattened
        .sort_by(|a, b| (&a.path, &a.why).cmp(&(&b.path, &b.why)));
    r.dropped
        .sort_by(|a, b| (&a.path, &a.why).cmp(&(&b.path, &b.why)));
    r.codemods
        .sort_by(|a, b| (&a.path, &a.why).cmp(&(&b.path, &b.why)));
    r.to_port.sort_by(|a, b| {
        (a.path.as_str(), format!("{:?}", a.kind), &a.why).cmp(&(
            b.path.as_str(),
            format!("{:?}", b.kind),
            &b.why,
        ))
    });
    r.to_port.dedup();
    let md = render_report(r, &p.notes);
    p.files.insert(
        rel("MIGRATION.md"),
        File {
            change: Change::Write {
                to: rel("MIGRATION.md"),
                body: Body::Text(md),
            },
            pack_path: None,
            fragment: false,
        },
    );

    let mut changes = vec![Change::Config(config), Change::Lock(lock)];
    changes.extend(p.files.into_values().map(|f| f.change));
    changes.extend(p.notes.into_iter().map(Change::Note));
    Ok(MigrationPlan {
        changes,
        report: p.report,
    })
}

/// name_ninja -> `beacon.autoNames`, what it cannot express -> to port.
fn name_ninja_config(p: &mut Planner, steps: &[&FilterRun], b: &mut Map<String, Value>) {
    let Some(s) = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("name_ninja"))
        .map(|s| s.settings.clone().unwrap_or_default())
    else {
        return;
    };
    let on = |k: &str| s[k]["auto_name"].as_bool().unwrap_or(false);
    let mut an =
        json!({ "entities": on("entities"), "items": on("items"), "blocks": on("blocks") });
    if on("spawn_eggs") {
        an["spawnEggs"] = json!({
            "prefix": s["spawn_eggs"]["prefix"].as_str().unwrap_or(""),
            "postfix": s["spawn_eggs"]["postfix"].as_str().unwrap_or(""),
        });
    }
    let langs: Vec<&str> = s["languages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if let Some(l) = langs.first() {
        an["language"] = json!(l.trim_end_matches(".lang"));
    }
    b.insert("autoNames".into(), an);
    let mut gaps = Vec::new();
    if let Some(ig) = s.get("ignored_namespaces")
        && ig != &json!(["minecraft"])
    {
        gaps.push(format!("ignored_namespaces {ig}"));
    }
    if s.get("sort").is_some_and(|v| v != &json!(false)) {
        gaps.push("sort".into());
    }
    if langs.len() > 1 {
        gaps.push(format!("languages {langs:?} (only the first is generated)"));
    }
    for k in ["entities", "items", "blocks"] {
        for f in ["prefix", "postfix"] {
            if s[k].get(f).is_some() {
                gaps.push(format!("{k}.{f}"));
            }
        }
    }
    if s.to_string().contains("from_entity_name") {
        gaps.push("from_entity_name".into());
    }
    for g in gaps {
        let why = format!("name_ninja setting Beacon's autoNames lacks: {g}");
        p.notes.push(note(Code::MigrateToPort, why.clone()));
        p.report.to_port.push(ToPort {
            kind: ToPortKind::LegacyFilter,
            path: rel("config.json"),
            why,
        });
    }
}

fn auto_opts(steps: &[&FilterRun]) -> Option<AutoNameOpts> {
    let s = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("name_ninja"))?
        .settings
        .clone()
        .unwrap_or_default();
    let on = |k: &str| s[k]["auto_name"].as_bool().unwrap_or(false);
    Some(AutoNameOpts {
        entities: on("entities"),
        items: on("items"),
        blocks: on("blocks"),
        spawn_eggs: on("spawn_eggs").then(|| {
            let t = |k: &str| s["spawn_eggs"][k].as_str().unwrap_or("").to_string();
            (t("prefix"), t("postfix"))
        }),
        ignored_namespaces: s["ignored_namespaces"].as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        }),
    })
}

/// Codemods in profile order: per-file filters, then name_ninja's named assets, then `.lang`
/// line rewrites (dedupe, Beacon's `pack.name` lines, auto names Beacon regenerates).
fn codemods(p: &mut Planner, steps: &[&FilterRun]) {
    let data = data_rel(p.project);
    let scope = |path: &str, key: &str| -> Option<String> {
        let f = data.as_ref()?.join(trim(path)).ok()?;
        let b = p.idx.read(&f).ok()?;
        let v: Value = serde_json::from_slice(&strip_comments(&b)).ok()?;
        let mut cur = &v;
        for k in key.split('.') {
            cur = cur.get(k)?;
        }
        match cur {
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        }
    };
    let mut batches: Vec<Vec<codemod::Batch>> = Vec::new();
    for s in steps
        .iter()
        .filter(|s| s.filter.as_deref() == Some("text_replacer"))
    {
        match codemod::text_replacer_batches(&s.settings.clone().unwrap_or_default(), &scope) {
            Ok(b) => batches.push(b),
            Err(e) => p.notes.push(note(
                Code::MigrateToPort,
                format!("text_replacer settings: {e}; not applied"),
            )),
        }
    }
    let mut tr = batches.into_iter();
    let keys: Vec<RelPath> = p.files.keys().cloned().collect();
    let read = |p: &Planner, f: &File| -> Option<Vec<u8>> {
        match &f.change {
            Change::Copy { from, .. } => p.idx.read(from).ok(),
            Change::Write { body, .. } => body_bytes(body, p.idx).ok(),
            _ => None,
        }
    };
    let set = |p: &mut Planner, to: &RelPath, bytes: Vec<u8>, entries: Vec<Entry>| {
        if let Some(f) = p.files.get_mut(to) {
            f.change = Change::Write {
                to: to.clone(),
                body: text_or_bytes(bytes),
            };
        }
        p.report.codemods.extend(entries.into_iter().map(|mut e| {
            e.path = format!("{to} ({})", e.path);
            e
        }));
    };
    // per-file filters, in profile order
    let mut todo: Vec<(&str, Option<Vec<codemod::Batch>>)> = Vec::new();
    for s in steps {
        match s.filter.as_deref() {
            Some(f @ ("shapescape_default" | "chunker_block_mappings")) => todo.push((f, None)),
            Some("content_guide_generator" | "shapescape_content_guide_generator") => {
                todo.push(("content_guide_generator", None))
            }
            Some("text_replacer") => todo.push(("text_replacer", tr.next())),
            _ => {}
        }
    }
    if !todo.is_empty() {
        for to in &keys {
            let f = &p.files[to];
            let Some(pp) = f.pack_path.clone() else {
                continue;
            };
            let frag = f.fragment;
            let Some(mut bytes) = read(p, f) else {
                continue;
            };
            let mut entries = Vec::new();
            for (name, b) in &todo {
                let r = match (*name, b) {
                    // per fragment it would add its families once per fragment
                    ("shapescape_default", _) if frag => None,
                    ("shapescape_default", _) => codemod::entity_patch(&pp, &bytes),
                    ("chunker_block_mappings", _) => codemod::strip_converted_from(&pp, &bytes),
                    ("content_guide_generator", _) => codemod::strip_content_guide(&pp, &bytes),
                    ("text_replacer", Some(b)) if pp.starts_with("BP/scripts/") => {
                        script_replace(&pp, &bytes, b)
                    }
                    ("text_replacer", Some(b)) => codemod::text_replace(&pp, &bytes, b),
                    _ => None,
                };
                if let Some((nb, e)) = r {
                    bytes = nb;
                    entries.push(e);
                }
            }
            if !entries.is_empty() {
                set(p, to, bytes, entries);
            }
        }
    }
    let auto = auto_opts(steps);
    let lang_name = steps
        .iter()
        .find(|s| s.filter.as_deref() == Some("name_ninja"))
        .and_then(|s| {
            s.settings.as_ref()?["languages"][0]
                .as_str()
                .map(String::from)
        })
        .map_or("en_US".to_string(), |l| {
            l.trim_end_matches(".lang").to_string()
        });
    // name_ninja: explicit names leave the JSON for the module's lang file
    let mut generated = BTreeMap::new();
    if let Some(opts) = &auto {
        let explicit = AutoNameOpts {
            entities: false,
            items: false,
            blocks: false,
            spawn_eggs: None,
            ignored_namespaces: opts.ignored_namespaces.clone(),
        };
        let mut lines: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for to in &keys {
            let f = &p.files[to];
            let Some(pp) = f.pack_path.clone().filter(|pp| pp.ends_with(".json")) else {
                continue;
            };
            if !pp.starts_with("BP/") || f.fragment {
                continue;
            }
            let Some(bytes) = read(p, f) else { continue };
            let Ok(mut v) = serde_json::from_slice::<Value>(&strip_comments(&bytes)) else {
                continue;
            };
            let named = codemod::auto_name_keys(&v, &explicit);
            if !named.is_empty() {
                for root in ["minecraft:entity", "minecraft:item", "minecraft:block"] {
                    if let Some(Value::Object(d)) =
                        v.get_mut(root).and_then(|e| e.get_mut("description"))
                    {
                        d.remove("name");
                        d.remove("spawn_egg_name");
                    }
                }
                let module = to
                    .as_str()
                    .split('/')
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                lines.entry(module).or_default().extend(named);
                let e = Entry {
                    path: pp.clone(),
                    why: "explicit names moved from the JSON to the module's lang file".into(),
                };
                p.notes.push(note(
                    Code::MigrateFlattened,
                    format!("{}: {}", e.path, e.why),
                ));
                p.report.flattened.push(e);
                set(p, to, json_bytes(&v), vec![]);
            }
            generated.extend(codemod::auto_name_keys(&v, opts));
        }
        // keys already in the language name_ninja writes to (not in other languages)
        let defined: BTreeSet<String> = keys
            .iter()
            .filter(|k| k.file_name() == format!("{lang_name}.lang"))
            .filter_map(|k| String::from_utf8(read(p, &p.files[k])?).ok())
            .flat_map(|t| lang_map(&t).into_keys())
            .collect();
        for (module, pairs) in lines {
            let Ok(to) = RelPath::parse(&format!("src/{module}/texts/{lang_name}.lang")) else {
                continue;
            };
            let mut text = p
                .files
                .get(&to)
                .and_then(|f| read(p, f))
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_default();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            for (k, v) in pairs.iter().filter(|(k, _)| !defined.contains(k)) {
                text.push_str(&format!("{k}={v}\n"));
            }
            let pp = format!("RP/texts/{lang_name}.lang");
            p.commit(
                to.clone(),
                None,
                Change::Write {
                    to,
                    body: Body::Text(text),
                },
                &pp,
                false,
            );
        }
    }
    // `.lang` lines
    let keys: Vec<RelPath> = p.files.keys().cloned().collect();
    for to in &keys {
        let f = &p.files[to];
        let Some(pp) = f.pack_path.clone().filter(|pp| pp.ends_with(".lang")) else {
            continue;
        };
        let Some(mut bytes) = read(p, f) else {
            continue;
        };
        let mut entries = Vec::new();
        if let Some((b, e)) = codemod::lang_dedupe(&pp, &bytes) {
            bytes = b;
            entries.push(e);
        }
        // Beacon writes `pack.name` / `pack.description` itself (packMeta)
        if let Ok(t) = std::str::from_utf8(&bytes) {
            for (k, v) in lang_map(t) {
                if k == "pack.name" || k == "pack.description" {
                    p.pack_lang.entry(k).or_insert(v);
                }
            }
            let kept: String = t
                .split_inclusive('\n')
                .filter(|l| !matches!(lang_key(l), Some("pack.name" | "pack.description")))
                .collect();
            if kept.len() != t.len() {
                bytes = kept.into_bytes();
                entries.push(Entry {
                    path: pp.clone(),
                    why: "pack.name / pack.description removed: Beacon generates them".into(),
                });
            }
        }
        // autoNames writes one language: other languages keep their lines
        let auto_lang = pp
            .rsplit('/')
            .next()
            .is_some_and(|n| n == format!("{lang_name}.lang"));
        if auto_lang
            && !generated.is_empty()
            && let Some((b, e)) = codemod::strip_auto_names(&pp, &bytes, &generated)
        {
            bytes = b;
            entries.push(e);
        }
        if !entries.is_empty() {
            set(p, to, bytes, entries);
        }
    }
}

fn plain_file(change: Change) -> File {
    File {
        change,
        pack_path: None,
        fragment: false,
    }
}

/// `a/./b/../c` -> `a/c`; `None` when it climbs above the root.
fn normalize(p: &str) -> Option<RelPath> {
    let mut out: Vec<&str> = Vec::new();
    for s in p.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    RelPath::parse(&out.join("/")).ok()
}

const DEP_SECTIONS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

/// npm dependencies. The root `package.json` is copied; a data-dir `package.json` (where
/// system_template_esbuild / ModularMC's esbuild resolve packages) moves to the root, merged
/// into the root one. Its `file:` packages (local modules, `.yalc`) are copied under
/// `vendor/<legacy path>` and the specs rewritten to point there; `node_modules` is never
/// copied: the user runs `npm install` once.
fn npm_deps(p: &mut Planner, data: Option<&RelPath>) {
    let read_json = |p: &Planner, f: &RelPath| -> Option<Value> {
        let b = p.idx.read(f).ok()?;
        let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
        serde_json::from_slice(&strip_comments(b)).ok()
    };
    let root = rel("package.json");
    let root_pkg = p.idx.exists(&root).then(|| read_json(p, &root)).flatten();
    let data_pkg = data
        .and_then(|d| d.join("package.json").ok())
        .filter(|f| p.idx.exists(f));
    let Some(dp) = data_pkg else {
        if p.idx.exists(&root) {
            let change = Change::Copy {
                from: root.clone(),
                to: root.clone(),
            };
            p.files.insert(root.clone(), plain_file(change));
            p.notes.push(note(
                Code::MigrateToPort,
                "package.json copied: run `npm install` once in the new project",
            ));
        }
        return;
    };
    let Some(mut dv) = read_json(p, &dp) else {
        p.notes.push(
            note(
                Code::MigrateUnsupported,
                format!("{dp} is not valid JSON; npm dependencies not migrated"),
            )
            .warn(),
        );
        return;
    };
    let data = data.expect("data_pkg implies data");
    let mut why = vec![format!(
        "npm dependencies moved from {dp} to the project root"
    )];
    let files: Vec<RelPath> = p.idx.files().into_iter().cloned().collect();
    for sec in DEP_SECTIONS {
        let Some(deps) = dv.get_mut(sec).and_then(Value::as_object_mut) else {
            continue;
        };
        for (name, spec) in deps.iter_mut() {
            let Some(path) = spec
                .as_str()
                .and_then(|s| s.strip_prefix("file:"))
                .map(String::from)
            else {
                continue;
            };
            // absolute (`/x`, `\x`, `C:...`, `//server/...`): outside the project
            let fwd = path.replace('\\', "/");
            let absolute = fwd.starts_with('/')
                || fwd.as_bytes().get(1) == Some(&b':') && fwd.as_bytes()[0].is_ascii_alphabetic();
            let src = (!absolute)
                .then(|| normalize(&format!("{data}/{fwd}")))
                .flatten();
            let Some(src) = src else {
                p.notes.push(
                    note(
                        Code::MigrateUnsupported,
                        format!("{dp}: {name} `file:{path}` leaves the project; not copied"),
                    )
                    .warn(),
                );
                continue;
            };
            // a folder (local module, `.yalc`) or one file (`file:x.tgz`)
            let prefix = format!("{src}/");
            for f in files
                .iter()
                .filter(|f| **f == src || f.as_str().starts_with(&prefix))
            {
                if let Ok(to) = RelPath::parse(&format!("vendor/{f}")) {
                    let change = Change::Copy {
                        from: f.clone(),
                        to: to.clone(),
                    };
                    p.files.insert(to, plain_file(change));
                }
            }
            *spec = json!(format!("file:vendor/{src}"));
            why.push(format!(
                "{name}: `file:{path}` -> `file:vendor/{src}` (copied)"
            ));
        }
    }
    // merge into the root package.json: the data dir's specs are what the bundle used
    let merged = match root_pkg {
        None => dv,
        Some(mut rv) => {
            for sec in DEP_SECTIONS {
                let Some(add) = dv.get(sec).and_then(Value::as_object) else {
                    continue;
                };
                let slot = rv
                    .as_object_mut()
                    .map(|o| o.entry(sec).or_insert_with(|| json!({})));
                let Some(Value::Object(slot)) = slot else {
                    continue;
                };
                for (k, v) in add {
                    if let Some(old) = slot.get(k).filter(|old| *old != v) {
                        why.push(format!("{sec}.{k}: root {old} replaced by {v}"));
                    }
                    slot.insert(k.clone(), v.clone());
                }
            }
            why.push("merged into the existing root package.json".into());
            rv
        }
    };
    p.files.insert(
        root.clone(),
        plain_file(Change::Write {
            to: root.clone(),
            body: Body::Json(merged),
        }),
    );
    let lock = rel("package-lock.json");
    if let Ok(dl) = data.join("package-lock.json")
        && p.idx.exists(&dl)
    {
        if p.idx.exists(&lock) {
            why.push(format!(
                "{dl} not copied: the root package-lock.json is kept"
            ));
        } else {
            let change = Change::Copy {
                from: dl.clone(),
                to: lock.clone(),
            };
            p.files.insert(lock.clone(), plain_file(change));
            why.push(format!(
                "{dl} copied (its `file:` entries are refreshed by `npm install`)"
            ));
        }
    }
    why.push("node_modules is not copied: run `npm install` once in the new project".into());
    let why = why.join("; ");
    p.notes
        .push(note(Code::MigrateToPort, format!("package.json: {why}")));
    p.report.codemods.push(Entry {
        path: "package.json".into(),
        why,
    });
}

/// `function a/b` / `schedule function a/b 1t`: the pack paths of the functions a function
/// calls.
fn called_functions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for l in text.lines().filter(|l| !l.trim_start().starts_with('#')) {
        let mut words = l.split_whitespace();
        while let Some(w) = words.next() {
            if w == "function"
                && let Some(t) = words.next()
            {
                let t = t.trim_start_matches('/');
                if RelPath::parse(t).is_ok() {
                    out.push(format!("BP/functions/{t}.mcfunction"));
                }
            }
        }
    }
    out
}

/// Functions still in subfunctions syntax (it ran after the evaluated maps), with `--oracle`:
/// the expansion is taken from the legacy output, frozen. Generated functions it calls (or
/// under its own `X/` folder) that no migrated file accounts for come along.
fn freeze_subfunctions(p: &mut Planner, oracle: &std::path::Path) {
    let read = |p: &Planner, f: &File| -> Option<String> {
        let b = match &f.change {
            Change::Copy { from, .. } => p.idx.read(from).ok()?,
            Change::Write { body, .. } => body_bytes(body, p.idx).ok()?,
            _ => return None,
        };
        String::from_utf8(b).ok()
    };
    let mut accounted: BTreeSet<String> = p
        .files
        .values()
        .filter_map(|f| f.pack_path.clone())
        .collect();
    let todo: Vec<(RelPath, String)> = p
        .files
        .iter()
        .filter_map(|(to, f)| {
            let pp = f.pack_path.clone()?;
            (pp.starts_with("BP/functions/") && pp.ends_with(".mcfunction"))
                .then(|| read(p, f))
                .flatten()
                .filter(|t| has_subfunctions(t))
                .map(|_| (to.clone(), pp))
        })
        .collect();
    let why = "frozen subfunctions expansion (from the oracle)";
    for (to, pp) in todo {
        let Ok(expanded) = std::fs::read_to_string(oracle.join(&pp)) else {
            continue; // stays reported as subfunctions syntax
        };
        let module = to
            .as_str()
            .split('/')
            .nth(1)
            .unwrap_or_default()
            .to_string();
        if let Some(f) = p.files.get_mut(&to) {
            f.change = Change::Write {
                to: to.clone(),
                body: Body::Text(expanded.clone()),
            };
        }
        let mut frozen = vec![pp.clone()];
        // generated functions: `X/**` and whatever the expansion calls, transitively
        let stem = pp.trim_end_matches(".mcfunction").to_string();
        let mut queue: Vec<String> = called_functions(&expanded);
        let mut stack = vec![(oracle.join(&stem), stem.clone())];
        while let Some((dir, rel_dir)) = stack.pop() {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let r = format!("{rel_dir}/{name}");
                match e.file_type() {
                    Ok(t) if t.is_dir() => stack.push((e.path(), r)),
                    Ok(t) if t.is_file() && name.ends_with(".mcfunction") => queue.push(r),
                    _ => {}
                }
            }
        }
        while let Some(g) = queue.pop() {
            if accounted.contains(&g) || RelPath::parse(&g).is_err() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(oracle.join(&g)) else {
                continue;
            };
            let rest = &g["BP/".len()..];
            let Ok(gto) = RelPath::parse(&format!("src/{module}/bp/{rest}")) else {
                continue;
            };
            if p.files.contains_key(&gto) {
                continue;
            }
            accounted.insert(g.clone());
            queue.extend(called_functions(&text));
            let change = Change::Write {
                to: gto.clone(),
                body: Body::Text(text),
            };
            p.commit(gto, Some(Pack::Behavior), change, &g, false);
            frozen.push(g);
        }
        for f in &frozen {
            p.notes
                .push(note(Code::MigrateFlattened, format!("{f}: {why}")));
            p.report.flattened.push(Entry {
                path: f.clone(),
                why: why.into(),
            });
        }
        p.report.to_port.push(ToPort {
            kind: ToPortKind::SubfunctionSource,
            path: to.clone(),
            why: format!(
                "{why}: {} file(s) taken from the legacy output; edit the expanded functions or port them to a generator",
                frozen.len()
            ),
        });
    }
}

fn to_port(p: &mut Planner, steps: &[&FilterRun], tdiags: &[Diagnostic]) {
    let mut out = Vec::new();
    let scripted = p
        .files
        .keys()
        .any(|k| k.as_str().starts_with("src/_scripts/"));
    if scripted
        && steps
            .iter()
            .any(|s| s.filter.as_deref() == Some("text_replacer"))
    {
        out.push(ToPort {
            kind: ToPortKind::LegacyFilter,
            path: rel("config.json"),
            why: "text_replacer rewrote the legacy script bundle: its rules are applied to the migrated script sources (module specifiers kept), but code of npm packages the bundle inlines is not rewritten; check those packages for the replaced strings".into(),
        });
    }
    let files: Vec<RelPath> = p.idx.files().into_iter().cloned().collect();
    let read = |f: &RelPath| p.idx.read(f).ok().and_then(|b| String::from_utf8(b).ok());
    // pre-build scripts
    for pj in ["package.json", "filters_data/package.json"] {
        let f = rel(pj);
        let Some(v) = read(&f).and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
            continue;
        };
        for (k, cmd) in v["scripts"].as_object().into_iter().flatten() {
            if k.contains("preregolith") {
                out.push(ToPort {
                    kind: ToPortKind::PrebuildScript,
                    path: f.clone(),
                    why: format!("`npm run {k}` ({cmd}) writes generated sources; its output was imported as files"),
                });
            }
        }
    }
    for f in &files {
        let n = f.file_name();
        if n.starts_with("preregolith.") && CODE_EXT.iter().any(|e| n.ends_with(e)) {
            out.push(ToPort {
                kind: ToPortKind::PrebuildScript,
                path: f.clone(),
                why: "pre-build script; port it to a generator or plugin".into(),
            });
        }
        if n.ends_with(".py") && f.parent().is_some_and(|d| d.file_name() == "_plugins") {
            out.push(ToPort {
                kind: ToPortKind::PythonPlugin,
                path: f.clone(),
                why: "system_template Python plugin; port it to a generator".into(),
            });
        }
    }
    // maps: fan-out and env reads
    let roots = p.legacy_roots.clone();
    let maps: Vec<RelPath> = files
        .iter()
        .filter(|f| {
            matches!(f.file_name(), "_map.ts" | "_map.py" | "auto-map.ts")
                && roots
                    .iter()
                    .any(|r| f.as_str().starts_with(&format!("{r}/")))
        })
        .cloned()
        .collect();
    for m in &maps {
        let Some(src) = read(m) else { continue };
        let fan = if m.as_str().ends_with(".py") {
            src.contains(" for ")
        } else {
            src.contains(".map(") || src.contains("...")
        };
        if fan {
            out.push(ToPort {
                kind: ToPortKind::GeneratorCandidate,
                path: m.clone(),
                why: "one-to-many map (loop / spread); imported as its evaluated output, could become a generator".into(),
            });
        }
    }
    let mut per_source: BTreeMap<&RelPath, BTreeSet<&str>> = BTreeMap::new();
    for o in p.eval.outputs.values() {
        for c in &o.contributors {
            per_source.entry(&c.source).or_default().insert(&o.path);
        }
    }
    for o in p.eval.outputs.values() {
        for c in &o.contributors {
            if per_source[&c.source].len() > 1 {
                out.push(ToPort {
                    kind: ToPortKind::GeneratorCandidate,
                    path: c.map.clone(),
                    why: "one-to-many map (loop / spread); imported as its evaluated output, could become a generator".into(),
                });
            }
        }
    }
    for (k, v) in &p.eval.env_read {
        let needles = [
            format!("process.env.{k}"),
            format!("process.env[\"{k}\"]"),
            format!("process.env['{k}']"),
        ];
        let readers: Vec<&RelPath> = maps
            .iter()
            .filter(|m| read(m).is_some_and(|s| needles.iter().any(|n| s.contains(n.as_str()))))
            .collect();
        let why = format!(
            "process.env.{k} ({}) decided this map's output; port the branch to a target `define` or `ctx.edition`",
            v.as_deref()
                .map_or("unset".to_string(), |v| format!("{v:?}"))
        );
        if readers.is_empty() {
            out.extend(data_rel(p.project).map(|d| ToPort {
                kind: ToPortKind::EnvBranch,
                path: d,
                why: why.clone(),
            }));
        }
        for m in readers {
            out.push(ToPort {
                kind: ToPortKind::EnvBranch,
                path: m.clone(),
                why: why.clone(),
            });
        }
    }
    // subfunctions ran after the evaluated maps (ModularMC profiles): the migrated functions
    // still hold its syntax, which Beacon does not expand
    if steps
        .iter()
        .any(|s| s.filter.as_deref() == Some("subfunctions"))
    {
        for (to, f) in &p.files {
            if !f
                .pack_path
                .as_deref()
                .is_some_and(|pp| pp.ends_with(".mcfunction"))
            {
                continue;
            }
            let text = match &f.change {
                Change::Copy { from, .. } => read(from),
                Change::Write { body, .. } => body_bytes(body, p.idx)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok()),
                _ => None,
            };
            if text.as_deref().is_some_and(has_subfunctions) {
                out.push(ToPort {
                    kind: ToPortKind::SubfunctionSource,
                    path: to.clone(),
                    why: "still uses subfunctions syntax (`for <..>`, `if <..>`, `function <..>`, `eval:`), which Beacon does not expand: expand it by hand or port it to a generator".into(),
                });
            }
        }
    }
    // subfunctions: expanded functions were kept expanded
    if steps
        .iter()
        .any(|s| s.filter.as_deref() == Some("subfunctions"))
    {
        for o in p
            .eval
            .outputs
            .values()
            .filter(|o| o.path.ends_with(".mcfunction"))
        {
            for c in o
                .contributors
                .iter()
                .filter(|_| !matches!(o.body, Body::Passthrough(_)))
            {
                out.push(ToPort {
                    kind: ToPortKind::SubfunctionSource,
                    path: c.source.clone(),
                    why: "its expanded functions were kept as static files; port the subfunctions by hand".into(),
                });
            }
        }
    }
    for d in tdiags.iter().filter(|d| d.code == Code::MigrateToPort) {
        if d.message.starts_with("filter `") {
            out.push(ToPort {
                kind: ToPortKind::LegacyFilter,
                path: rel("config.json"),
                why: d.message.clone(),
            });
        }
    }
    out.sort_by(|a, b| (a.path.as_str(), &a.why).cmp(&(b.path.as_str(), &b.why)));
    out.dedup();
    for t in &out {
        p.notes
            .push(note(Code::MigrateToPort, format!("{}: {}", t.path, t.why)));
    }
    p.report.to_port.extend(out);
}

/// `MIGRATION.md`: deterministic (sorted report, no paths outside the project, no times).
pub fn render_report(r: &Report, notes: &[Diagnostic]) -> String {
    let mut s = String::from(
        "# Migration report\n\nGenerated once by `beacon migrate`; the legacy project was not modified.\n\n",
    );
    s.push_str(&format!(
        "- Modules: {}\n- Files written: {}\n- Legacy source hash (blake3): `{}`\n\n",
        r.modules
            .iter()
            .map(|m| format!("`{m}`"))
            .collect::<Vec<_>>()
            .join(", "),
        r.files,
        r.source_hash
    ));
    s.push_str(
        "Layout: one module per legacy module (`src/<module>`); files whose Beacon convention matches the legacy path stay at the module root, the rest sit under `packRoots` (`bp/`, `rp/`). `src/_base` holds the base packs, `src/_merged` files written pre-merged, `src/_generated` outputs of other legacy filters, `src/_scripts` the game scripts and their entry (`scripts/main.ts`).\n\n",
    );
    let list = |s: &mut String, title: &str, items: Vec<String>| {
        s.push_str(&format!("## {title}\n\n"));
        if items.is_empty() {
            s.push_str("None.\n\n");
        } else {
            for i in items {
                s.push_str(&format!("- {i}\n"));
            }
            s.push('\n');
        }
    };
    let entries = |v: &[Entry]| {
        v.iter()
            .map(|e| format!("`{}`: {}", e.path, e.why))
            .collect()
    };
    list(
        &mut s,
        "To port by hand",
        r.to_port
            .iter()
            .map(|t| format!("**{:?}** `{}`: {}", t.kind, t.path, t.why))
            .collect(),
    );
    list(
        &mut s,
        "Flattened (imported as evaluated output)",
        entries(&r.flattened),
    );
    list(&mut s, "Dropped", entries(&r.dropped));
    list(&mut s, "Codemods applied", entries(&r.codemods));
    let other: Vec<String> = notes
        .iter()
        .filter(|d| {
            !matches!(
                d.code,
                Code::MigrateFlattened | Code::MigrateDropped | Code::MigrateToPort
            ) || !d.message.contains(": ")
        })
        .map(|d| format!("{} {}", d.code.id(), d.message))
        .collect();
    list(&mut s, "Notes", other);
    s.push_str("## Equivalence (`--oracle`)\n\n");
    match &r.oracle {
        None => {
            s.push_str("Not checked: run `beacon migrate --oracle <legacy BP/RP>` to compare.\n")
        }
        Some(o) => {
            s.push_str(&format!(
                "{} files compared, {} ignored, {} differences. Normalization: manifests are compared without their identities (`header.uuid|version`, `modules[].uuid|version`, `dependencies[].uuid`: lockfile-owned, imported from the legacy manifests), `**/*.map` and root files are ignored, Beacon's own `pack.name` / `pack.description` lang lines and generated `languages.json` are not counted, script bundles are compared by module order with migrated paths mapped back to the legacy sources.\n\n",
                o.compared,
                o.ignored,
                o.diffs.len()
            ));
            for d in &o.diffs {
                s.push_str(&format!(
                    "- `{}`\n",
                    serde_json::to_string(d).unwrap_or_default()
                ));
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn det_uuid_is_v4_shaped_and_stable() {
        let a = det_uuid("x");
        assert_eq!(a, det_uuid("x"));
        assert_ne!(a, det_uuid("y"));
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
    }

    #[test]
    fn extras_keep_what_beacon_does_not_generate() {
        let m = json!({
            "format_version": 2,
            "header": {"name": "N", "description": "pack.description", "uuid": "u", "version": [1,0,0],
                       "min_engine_version": [1,26,50], "pack_scope": "world"},
            "modules": [{"type": "data"}],
            "metadata": {"authors": ["a"]},
            "dependencies": [{"uuid": "u"}, {"uuid": "rp"}, {"uuid": "other", "version": [1,0,0]},
                             {"module_name": "@minecraft/server", "version": "2.0.0"}]
        });
        assert_eq!(
            extras(&m, &["u", "rp"]).unwrap(),
            json!({"header": {"name": "N", "pack_scope": "world"}, "metadata": {"authors": ["a"]},
                   "dependencies": [{"uuid": "other", "version": [1,0,0]}]})
        );
        assert!(extras(&json!({"format_version": 2, "header": {"uuid": "u"}}), &[]).is_none());
    }

    #[test]
    fn subfunctions_syntax() {
        assert!(has_subfunctions(
            "say a\nfor <i 1..3 1>:\n    say `eval:i`\n"
        ));
        assert!(has_subfunctions("function <x>:\n    say a\n"));
        assert!(!has_subfunctions("# for <i>\nfunction a/b\nsay if <x>\n"));
        // a commented-out line is kept as is by the filter
        assert!(!has_subfunctions("#give @s x {\"a\":`eval:b`}\n"));
    }

    #[test]
    fn generated_import_hint() {
        let d = hint(note(
            Code::MigrateUnsupported,
            "data/m/_map.ts: cannot resolve `../generated/sounds.ts`",
        ));
        assert!(d.message.contains("npm run preregolith"));
    }
}
