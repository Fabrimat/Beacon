//! Workstream B1: build orchestration (5.1). One [`Session`] per project; `build` (CLI build
//! and watch) and `check` reuse its previous index (hash fast path), generator cache and
//! script bundles.
//!
//! ```text
//! load -> index -> route -> (trusted: generators via cache, bundle per BP output)
//!   -> lockfile -> per target: manifests + base lang -> merge -> languages.json
//!   -> emit (dir via ledger) | archive
//! ```
//!
//! Destinations: no target = the "packs" build (primary output projected to `packs.*`, the
//! others to `.beacon/out/<id>/BP|RP`; `--out <dir>` = `<dir>/<id>/BP|RP`). `dev` targets go to
//! the edition's `com.mojang` (`BEACON_COM_MOJANG`, else detected; `--out` replaces it);
//! `mcpack`/`mcaddon` to `dist/` (or `--out`).

use crate::cache::GenCache;
use crate::config::{CONFIG_FILE, ProjectConfig, STATE_DIR};
use crate::diag::{Code, Diagnostic, Severity};
use crate::emit::Ledger;
use crate::event::BuildReport;
use crate::host::{BundleRequest, Bundler, GeneratorHost};
use crate::lock::Lockfile;
use crate::manifest::{self, ManifestInput};
use crate::model::{Body, ContentItem, Dest, Edition, GenCtx, Merge, OutputId, Pack};
use crate::path::{Hash, RelPath, Vfs};
use crate::vfs::Index;
use crate::{archive, emit, merge, route, sarif};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// One target of `beacon.targets`; `None` (and not `all`) = the packs build.
    pub target: Option<String>,
    /// Every target (the packs build when none are defined).
    pub all: bool,
    pub out: Option<PathBuf>,
    /// Fail instead of changing `beacon.lock.json`.
    pub frozen: bool,
    /// Run generators and the bundler. Untrusted = restricted mode (5.5).
    pub trusted: bool,
}

pub struct BuildOutcome {
    pub report: BuildReport,
    pub diagnostics: Vec<Diagnostic>,
    /// Increments per `build`/`check` call, starting at 1.
    pub generation: u64,
    /// Project-relative files the build depended on (index + script bundle inputs).
    pub inputs: BTreeSet<RelPath>,
    /// Generator evaluations actually run (cache misses).
    pub generators_run: usize,
}

/// GenCtx target of the packs build (no `--target`).
pub const PACKS_TARGET: &str = "packs";
const SCRIPT_ENTRY: &str = "scripts/main.js";
/// Re-index and retry when a file changes while the build reads it.
const ATTEMPTS: usize = 3;

pub struct Session {
    root: PathBuf,
    opts: BuildOptions,
    host: Box<dyn GeneratorHost>,
    bundler: Option<Box<dyn Bundler>>,
    index: Option<Index>,
    cache: GenCache,
    bundles: HashMap<(OutputId, bool), BundleMemo>,
    generation: u64,
    /// Index excludes of the current index (a change means a rescan).
    excl: Vec<RelPath>,
    /// Paths reported changed since the last build (see [`Session::changed`]).
    dirty: BTreeSet<PathBuf>,
    /// The caller reports changes: the next build applies `dirty` instead of walking the tree.
    delta: bool,
    /// Re-index opening every file (a file changed under a build).
    strict: bool,
    /// Diagnostics of the index walk(s), kept across delta updates.
    idx_diags: Vec<Diagnostic>,
    /// Last ledger written per pack dir; lets rebuilds skip unchanged outputs unstatted.
    ledgers: HashMap<PathBuf, Ledger>,
}

#[derive(Clone)]
struct Bundled {
    code: Option<String>,
    map: Option<String>,
    diagnostics: Vec<Diagnostic>,
    inputs: BTreeSet<RelPath>,
}

/// A successful bundle, reused while its entries, every input hash and the set of indexed
/// paths (new files can change import resolution) are unchanged.
struct BundleMemo {
    entries: Vec<RelPath>,
    paths: Hash,
    inputs: Vec<(RelPath, Hash)>,
    out: Bundled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Packs,
    Dev,
    McPack,
    McAddon,
}

struct Plan {
    target: String,
    kind: Kind,
    edition: Edition,
    outputs: Vec<OutputId>,
    define: BTreeMap<String, Value>,
}

#[derive(Default)]
struct Attempt {
    diags: Vec<Diagnostic>,
    emitted: usize,
    artifacts: Vec<String>,
    inputs: BTreeSet<RelPath>,
    generators_run: usize,
    changed: bool,
}

/// The index as the build sees it; remembers whether a read found a file changed since
/// indexing (the caller then re-indexes and retries).
struct Live<'a> {
    idx: &'a Index,
    changed: AtomicBool,
}

impl Live<'_> {
    fn note<T>(&self, r: io::Result<T>) -> io::Result<T> {
        if r.as_ref()
            .is_err_and(|e| e.kind() == ErrorKind::InvalidData)
        {
            self.changed.store(true, Ordering::Relaxed);
        }
        r
    }
}

impl Vfs for Live<'_> {
    fn exists(&self, p: &RelPath) -> bool {
        self.idx.exists(p)
    }
    fn read(&self, p: &RelPath) -> io::Result<Vec<u8>> {
        self.note(self.idx.read(p))
    }
    fn hash(&self, p: &RelPath) -> io::Result<Hash> {
        self.idx.hash(p)
    }
    fn list(&self, dir: Option<&RelPath>) -> Vec<String> {
        self.idx.list(dir)
    }
    fn stream(&self, p: &RelPath, out: &mut dyn io::Write) -> io::Result<u64> {
        self.note(self.idx.stream(p, out))
    }
}

/// The real path of `p` as the OS reports it in watcher events (macOS FSEvents gives
/// `/private/var/...` for `/var/...`): the longest existing ancestor canonicalized (without
/// the verbatim `\\?\` prefix on Windows), plus the not-yet-existing rest. Absolute as given
/// if nothing resolves.
pub fn canon(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.into());
    for a in abs.ancestors() {
        if let Ok(c) = dunce::canonicalize(a) {
            return match abs.strip_prefix(a) {
                Ok(rest) if !rest.as_os_str().is_empty() => c.join(rest),
                _ => c,
            };
        }
    }
    abs
}

/// Defence in depth (config validation already rejects path-like ids and `packs.*` escapes):
/// `dir` must be strictly below `base`, lexically, with no `..`/root/prefix components.
fn contained(base: &Path, dir: &Path) -> bool {
    dir.strip_prefix(base).is_ok_and(|r| {
        r.components().next().is_some()
            && r.components()
                .all(|c| matches!(c, std::path::Component::Normal(_)))
    })
}

/// Links inside the project on the way to an output (`packs/BP -> ~/.config`, a linked
/// `.beacon`, `dist` or in-project `--out`) would let a committed project redirect writes
/// anywhere. Untrusted: refuse (error). Trusted: allowed with a warning naming the target
/// (pack folders are often junctioned into com.mojang). Outputs outside the project
/// (com.mojang, an outside `--out`) are paths the user chose; the project cannot plant links
/// there, so they are not checked.
fn link_ok(root: &Path, dir: &Path, trusted: bool, diags: &mut Vec<Diagnostic>) -> bool {
    if !contained(root, dir) {
        return true;
    }
    let Some(link) = emit::link_below(root, dir) else {
        return true;
    };
    let target = std::fs::read_link(&link).map_or_else(|_| "?".into(), |t| t.display().to_string());
    let mut d = Diagnostic::new(
        Code::OutputIsLink,
        format!("output dir {} is a link to {target}", link.display()),
    );
    if !trusted {
        d.severity = Severity::Error;
        d.message += "; refusing to write through it in an untrusted project";
    }
    diags.push(d);
    trusted
}

fn escape(base: &Path, dir: &Path) -> Diagnostic {
    Diagnostic::new(
        Code::Internal,
        format!(
            "refusing to write {}: not inside {}",
            dir.display(),
            base.display()
        ),
    )
}

fn has_errors(d: &[Diagnostic]) -> bool {
    d.iter().any(|d| d.severity == Severity::Error)
}

fn pack_name(p: Pack) -> &'static str {
    match p {
        Pack::Behavior => "BP",
        Pack::Resource => "RP",
    }
}

/// `./packs/BP/` -> `packs/BP`.
fn trim_rel(s: &str) -> &str {
    let mut s = s;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.trim_end_matches('/')
}

/// File/folder name from the project name: `[A-Za-z0-9 ._-]`, others become `_`.
fn safe_name(s: &str) -> String {
    let n: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " ._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let n = n.trim_matches(|c| c == '.' || c == ' ');
    if n.is_empty() {
        "pack".into()
    } else {
        n.into()
    }
}

/// `com.mojang` of an edition: `BEACON_COM_MOJANG`, else the GDK then UWP install (Bedrock),
/// or the Education install. `None` when not found.
fn com_mojang(edition: Edition) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("BEACON_COM_MOJANG") {
        return Some(p.into());
    }
    let roaming = dirs::data_dir()?;
    let candidates = match edition {
        Edition::Bedrock => vec![
            roaming.join("Minecraft Bedrock/Users/Shared/games/com.mojang"),
            dirs::data_local_dir()?
                .join("Packages/Microsoft.MinecraftUWP_8wekyb3d8bbwe/LocalState/games/com.mojang"),
        ],
        Edition::Education => vec![roaming.join("Minecraft Education Edition/games/com.mojang")],
    };
    candidates.into_iter().find(|p| p.is_dir())
}

/// First 8 hex chars of a hash of the worktree path (dev folder suffix, 4.2).
fn worktree8(root: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.into());
    let s = root.to_string_lossy();
    let s = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    blake3::hash(s.as_bytes()).to_hex()[..8].to_owned()
}

fn plans(
    cfg: &ProjectConfig,
    opts: &BuildOptions,
    all: &[OutputId],
) -> (Vec<Plan>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let targets = &cfg.beacon.targets;
    let names: Vec<&String> = match &opts.target {
        Some(t) if targets.contains_key(t) => vec![t],
        Some(t) => {
            diags.push(
                Diagnostic::new(
                    Code::ConfigField,
                    format!("unknown target {t:?} (not in beacon.targets)"),
                )
                .at(RelPath::parse(CONFIG_FILE).expect("const"), None, None),
            );
            return (vec![], diags);
        }
        None if opts.all => targets.keys().collect(),
        None => vec![],
    };
    if names.is_empty() {
        let bedrock = targets.is_empty() || targets.values().any(|t| t.edition == Edition::Bedrock);
        let plan = Plan {
            target: PACKS_TARGET.into(),
            kind: Kind::Packs,
            edition: if bedrock {
                Edition::Bedrock
            } else {
                Edition::Education
            },
            outputs: all.to_vec(),
            define: BTreeMap::new(),
        };
        return (vec![plan], diags);
    }
    let mut out = Vec::new();
    for name in names {
        let t = &targets[name];
        let bad = |msg: String| {
            Diagnostic::new(Code::ConfigField, format!("target {name:?}: {msg}")).at(
                RelPath::parse(CONFIG_FILE).expect("const"),
                None,
                None,
            )
        };
        let kind = match t.artifact.as_str() {
            "dev" => Kind::Dev,
            "mcpack" => Kind::McPack,
            "mcaddon" => Kind::McAddon,
            a => {
                diags.push(bad(format!("artifact {a:?} is not supported yet")));
                continue;
            }
        };
        let outputs = match &t.outputs {
            Some(list) if !list.iter().any(|o| o == "*") => {
                for o in list.iter().filter(|o| !all.contains(o)) {
                    diags.push(bad(format!("unknown output {o:?}")));
                }
                all.iter().filter(|o| list.contains(o)).cloned().collect()
            }
            _ => all.to_vec(),
        };
        out.push(Plan {
            target: name.clone(),
            kind,
            edition: t.edition,
            outputs,
            define: t.define.clone(),
        });
    }
    (out, diags)
}

/// Adds `texts/languages.json` to every pack that has `texts/*.lang` and no such file.
fn languages(items: &mut Vec<ContentItem>) {
    let mut langs: BTreeMap<(OutputId, Pack), Vec<String>> = BTreeMap::new();
    let mut present = BTreeSet::new();
    for i in items.iter() {
        let p = i.dest.path.as_str();
        if p == "texts/languages.json" {
            present.insert((i.dest.output.clone(), i.dest.pack));
        } else if let Some(l) = p
            .strip_prefix("texts/")
            .and_then(|n| n.strip_suffix(".lang"))
            .filter(|n| !n.contains('/'))
        {
            langs
                .entry((i.dest.output.clone(), i.dest.pack))
                .or_default()
                .push(l.to_owned());
        }
    }
    let origin = RelPath::parse(CONFIG_FILE).expect("const");
    for ((output, pack), l) in langs {
        if present.contains(&(output.clone(), pack)) {
            continue;
        }
        items.push(ContentItem {
            dest: Dest {
                output,
                pack,
                path: RelPath::parse("texts/languages.json").expect("const"),
            },
            body: Body::Json(Value::from(l)), // sorted: items are sorted by dest
            merge: Merge::Error,
            origin: origin.clone(),
            module: origin.clone(),
        });
    }
    items.sort_by(|a, b| a.dest.cmp(&b.dest));
}

impl Session {
    pub fn new(
        root: &Path,
        mut opts: BuildOptions,
        host: Box<dyn GeneratorHost>,
        bundler: Option<Box<dyn Bundler>>,
    ) -> Self {
        let given = std::path::absolute(root).unwrap_or_else(|_| root.into());
        let root = canon(root);
        // `--out` inside the project, re-spelled under the canonical root without resolving
        // anything below it (links there must stay visible to the link check); outside the
        // project, canonical like the root (watch compares real paths).
        opts.out = opts.out.map(|o| {
            let abs = std::path::absolute(&o).unwrap_or(o);
            match abs.strip_prefix(&given) {
                Ok(rest) => root.join(rest),
                Err(_) => canon(&abs),
            }
        });
        Session {
            cache: GenCache::new(&root),
            root,
            opts,
            host,
            bundler,
            index: None,
            bundles: HashMap::new(),
            generation: 0,
            excl: Vec::new(),
            dirty: BTreeSet::new(),
            delta: false,
            strict: false,
            idx_diags: Vec::new(),
            ledgers: HashMap::new(),
        }
    }

    /// Watch: absolute paths that changed. The next `build` re-indexes only these (no walk)
    /// and trusts the ledgers it wrote itself (no stat of unchanged outputs). Without this
    /// (or after [`Session::rescan`]) a build walks the tree stat-only, verifies every output
    /// on disk again and so restores hand-deleted files.
    pub fn changed(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        // spelled like the canonical root (symlinked roots, 8.3 names, macOS /private)
        let root = &self.root;
        self.dirty.extend(
            paths
                .into_iter()
                .map(|p| if p.starts_with(root) { p } else { canon(&p) }),
        );
        self.delta = true;
    }

    /// Watcher overflow or unknown changes: the next `build` is a full one.
    pub fn rescan(&mut self) {
        self.delta = false;
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Re-indexes (fast path via the previous index), routes, runs generators (cached),
    /// bundles scripts, merges, writes manifests and emits or archives.
    pub fn build(&mut self) -> BuildOutcome {
        self.run(true)
    }

    /// Restricted unless trusted: same pipeline up to merge; never writes anything (no
    /// lockfile, manifests or emit).
    pub fn check(&mut self) -> BuildOutcome {
        self.run(false)
    }

    /// Absolute directories this session writes inside or outside the project (for watch
    /// to ignore): `.beacon`, `dist`, `packs.*`, `--out`.
    pub fn output_dirs(&self) -> Vec<PathBuf> {
        let mut v = vec![self.root.join(STATE_DIR), self.root.join("dist")];
        if let Ok(cfg) = route::load(&self.root) {
            for p in [&cfg.packs.behavior_pack, &cfg.packs.resource_pack]
                .into_iter()
                .flatten()
            {
                v.push(self.root.join(trim_rel(p)));
            }
        }
        if let Some(o) = &self.opts.out {
            v.push(o.clone());
        }
        v
    }

    fn run(&mut self, emit: bool) -> BuildOutcome {
        let t0 = Instant::now();
        self.generation += 1;
        let delta = std::mem::take(&mut self.delta);
        if !delta {
            self.ledgers.clear();
        }
        let (mut emitted, mut generators_run) = (0, 0);
        let mut a = Attempt::default();
        for _ in 0..ATTEMPTS {
            self.strict = a.changed;
            a = self.attempt(emit, delta && !self.strict);
            emitted += a.emitted;
            generators_run += a.generators_run;
            if !a.changed {
                break;
            }
        }
        self.strict = false;
        if a.changed {
            a.diags.push(Diagnostic::new(
                Code::EmitFailed,
                "files kept changing during the build; outputs may be partly written or stale (build again when edits settle)",
            ));
        }
        // one message per problem even when several targets hit it
        let mut seen = HashSet::new();
        a.diags
            .retain(|d| seen.insert(serde_json::to_string(d).unwrap_or_default()));
        BuildOutcome {
            report: BuildReport {
                ms: t0.elapsed().as_secs_f64() * 1000.0,
                emitted,
                artifacts: a.artifacts,
                sarif: sarif::to_sarif(&a.diags),
            },
            diagnostics: a.diags,
            generation: self.generation,
            inputs: a.inputs,
            generators_run,
        }
    }

    /// Excluded from the index: `dist`, `packs.*` and `--out` when inside the root.
    fn excludes(&self, cfg: &ProjectConfig) -> Vec<RelPath> {
        let mut v: Vec<RelPath> = ["dist"]
            .into_iter()
            .chain(
                [&cfg.packs.behavior_pack, &cfg.packs.resource_pack]
                    .into_iter()
                    .flatten()
                    .map(|p| trim_rel(p)),
            )
            .filter_map(|p| RelPath::parse(p).ok())
            .collect();
        if let Some(rel) = self
            .opts
            .out
            .as_ref()
            .and_then(|o| o.strip_prefix(&self.root).ok().map(Path::to_path_buf))
        {
            let s: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            v.extend(RelPath::parse(&s.join("/")).ok());
        }
        v
    }

    /// Records the diagnostics of a full index walk.
    fn walk(&mut self, (i, d): (Index, Vec<Diagnostic>)) -> Index {
        self.idx_diags = d;
        i
    }

    fn attempt(&mut self, emit: bool, delta: bool) -> Attempt {
        let mut a = Attempt::default();
        let cfg = match route::load(&self.root) {
            Ok(c) => c,
            Err(d) => {
                a.diags = d;
                return a;
            }
        };
        let outputs: Vec<OutputId> = if cfg.beacon.outputs.is_empty() {
            vec!["main".into()]
        } else {
            cfg.beacon.outputs.iter().map(|o| o.id.clone()).collect()
        };
        let (plans, d) = plans(&cfg, &self.opts, &outputs);
        a.diags.extend(d);

        let ex = self.excludes(&cfg);
        let index = match self.index.take() {
            Some(_) if self.strict => self.walk(Index::build(&self.root, &ex, None)),
            Some(mut i) if delta && ex == self.excl => {
                let dirty: Vec<PathBuf> = std::mem::take(&mut self.dirty).into_iter().collect();
                let root = &self.root;
                self.idx_diags.retain(|d| {
                    d.span.as_ref().is_some_and(|s| {
                        !dirty
                            .iter()
                            .any(|p| root.join(s.path.as_str()).starts_with(p))
                    })
                });
                let d = i.update(root, &ex, &dirty);
                self.idx_diags.extend(d);
                i
            }
            Some(prev) => self.walk(Index::rescan(&self.root, &ex, &prev)),
            None => self.walk(Index::build(&self.root, &ex, None)),
        };
        self.excl = ex;
        self.dirty.clear();
        a.diags.extend(self.idx_diags.iter().cloned());
        let vfs = Live {
            idx: &index,
            changed: AtomicBool::new(false),
        };
        let routed = route::route(&cfg, &vfs);
        a.diags.extend(routed.diagnostics);
        a.inputs = index.files().into_iter().cloned().collect();

        let has_code = !routed.generators.is_empty()
            || !routed.script_entries.is_empty()
            || !cfg.beacon.plugins.is_empty();
        let trusted = self.opts.trusted;
        let persist = emit; // check never writes, not even the generator cache
        let mut emit = emit;
        if has_code && !trusted {
            let mut d = Diagnostic::new(
                Code::Untrusted,
                "project has generators, scripts or plugins and is not trusted; run `beacon trust` or pass --trust",
            );
            if emit {
                emit = false; // never deploy a build with its code missing
            } else {
                d.severity = Severity::Note;
                d.message = "project has generators, scripts or plugins; checked in restricted mode (no JS was run)".into();
            }
            a.diags.push(d);
        }

        // script bundles, once per (output, sourcemap)
        let mut bundles: BTreeMap<(OutputId, bool), Bundled> = BTreeMap::new();
        if trusted {
            let paths = {
                let mut h = blake3::Hasher::new();
                for f in index.files() {
                    h.update(f.as_str().as_bytes());
                    h.update(&[0]);
                }
                *h.finalize().as_bytes()
            };
            for plan in &plans {
                let sourcemap = matches!(plan.kind, Kind::Packs | Kind::Dev);
                for o in &plan.outputs {
                    let Some(entries) = routed.script_entries.get(o) else {
                        continue;
                    };
                    let key = (o.clone(), sourcemap);
                    if bundles.contains_key(&key) {
                        continue;
                    }
                    let Some(bundler) = self.bundler.as_deref() else {
                        a.diags.push(Diagnostic::new(
                            Code::BundlerMissing,
                            Code::BundlerMissing.doc().trim(),
                        ));
                        continue;
                    };
                    let b = bundle(
                        &mut self.bundles,
                        bundler,
                        &self.root,
                        &index,
                        paths,
                        &key,
                        entries,
                    );
                    a.diags.extend(b.diagnostics.iter().cloned());
                    a.inputs.extend(b.inputs.iter().cloned());
                    bundles.insert(key, b);
                }
            }
        }

        let lock = if emit {
            match Lockfile::ensure(&self.root, &cfg, &outputs, self.opts.frozen) {
                Ok(mut l) => {
                    let version = self.bundler.as_ref().and_then(|b| b.version());
                    if !bundles.is_empty() && version.is_some() && l.bundler != version {
                        if self.opts.frozen {
                            a.diags.push(Diagnostic::new(
                                Code::LockfileFrozen,
                                format!(
                                    "{} would record bundler {} but --frozen was given",
                                    crate::config::LOCK_FILE,
                                    version.as_deref().unwrap_or_default()
                                ),
                            ));
                        } else {
                            l.set_bundler(version);
                            if let Err(e) = l.save(&self.root) {
                                a.diags.push(Diagnostic::new(
                                    Code::EmitFailed,
                                    format!("cannot write {}: {e}", crate::config::LOCK_FILE),
                                ));
                            }
                        }
                    }
                    Some(l)
                }
                Err(d) => {
                    a.diags.push(d);
                    None
                }
            }
        } else {
            None
        };

        let mut built = Vec::with_capacity(plans.len());
        for plan in &plans {
            let mut items: Vec<ContentItem> = routed
                .items
                .iter()
                .filter(|i| plan.outputs.contains(&i.dest.output))
                .cloned()
                .collect();
            if trusted {
                for g in routed
                    .generators
                    .iter()
                    .filter(|g| plan.outputs.contains(&g.output))
                {
                    let ctx = GenCtx {
                        module: g.module.file_name().to_owned(),
                        namespace: g.namespace.clone(),
                        edition: plan.edition,
                        target: plan.target.clone(),
                        define: plan.define.clone(),
                    };
                    let (it, d, ran) = self.cache.run(&*self.host, g, &ctx, &vfs, persist);
                    a.generators_run += ran as usize;
                    items.extend(it);
                    a.diags.extend(d);
                }
            }
            let sourcemap = matches!(plan.kind, Kind::Packs | Kind::Dev);
            let mut scripted = BTreeSet::new();
            for o in &plan.outputs {
                let Some(b) = bundles.get(&(o.clone(), sourcemap)) else {
                    continue;
                };
                let Some(code) = &b.code else { continue };
                let entry = &routed.script_entries[o][0];
                let module = entry
                    .parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(entry.clone());
                let mut add = |path: String, text: &String| {
                    items.push(ContentItem {
                        dest: Dest {
                            output: o.clone(),
                            pack: Pack::Behavior,
                            path: RelPath::parse(&path).expect("static path"),
                        },
                        body: Body::Text(text.clone()),
                        merge: Merge::Error,
                        origin: entry.clone(),
                        module: module.clone(),
                    })
                };
                add(SCRIPT_ENTRY.into(), code);
                if let Some(m) = &b.map {
                    add(format!("{SCRIPT_ENTRY}.map"), m);
                }
                scripted.insert(o.clone());
            }
            if let Some(lock) = &lock {
                for o in &plan.outputs {
                    let has = |p: Pack| {
                        items
                            .iter()
                            .any(|i| &i.dest.output == o && i.dest.pack == p)
                    };
                    let (has_bp, has_rp) = (has(Pack::Behavior), has(Pack::Resource));
                    if !(has_bp || has_rp) {
                        continue;
                    }
                    items.extend(manifest::generate(&ManifestInput {
                        cfg: &cfg,
                        lock,
                        output: o,
                        edition: plan.edition,
                        dev_worktree: (plan.kind == Kind::Dev).then_some(self.root.as_path()),
                        script_entry: scripted.contains(o).then_some(SCRIPT_ENTRY),
                        has_bp,
                        has_rp,
                    }));
                }
            }
            let (mut merged, d) = merge::resolve(items, &vfs);
            a.diags.extend(d);
            languages(&mut merged);
            built.push(merged);
        }
        // only an attempt that saw a consistent snapshot writes anything
        if emit && lock.is_some() && !vfs.changed.load(Ordering::Relaxed) {
            for (plan, merged) in plans.iter().zip(&built) {
                self.write(plan, &cfg, &outputs, merged, &vfs, &mut a);
            }
        }
        a.changed = vfs.changed.load(Ordering::Relaxed);
        self.index = Some(index);
        a
    }

    fn write(
        &mut self,
        plan: &Plan,
        cfg: &ProjectConfig,
        outputs: &[OutputId],
        merged: &[ContentItem],
        vfs: &(dyn Vfs + Sync),
        a: &mut Attempt,
    ) {
        let mut packs: BTreeMap<(&OutputId, Pack), Vec<(RelPath, &Body)>> = BTreeMap::new();
        for i in merged {
            packs
                .entry((&i.dest.output, i.dest.pack))
                .or_default()
                .push((i.dest.path.clone(), &i.body));
        }
        // projected output (4.3): `primaryOutput`, else the first with a BP (else any pack)
        let primary = cfg.beacon.primary_output.clone().or_else(|| {
            let with = |p: Option<Pack>| {
                outputs.iter().find(|o| {
                    packs
                        .keys()
                        .any(|(k, kp)| k == o && p.is_none_or(|p| p == *kp))
                })
            };
            with(Some(Pack::Behavior)).or(with(None)).cloned()
        });
        let name = safe_name(&cfg.name);
        let multi = packs.keys().map(|k| k.0).collect::<BTreeSet<_>>().len() > 1;
        let fail = |what: String| Diagnostic::new(Code::EmitFailed, what);
        let ledgers = &mut self.ledgers;
        let (root, trusted) = (&self.root, self.opts.trusted);
        let mut dir_emit =
            |base: &Path, dir: PathBuf, files: &[(RelPath, &Body)], a: &mut Attempt| {
                if !contained(base, &dir) {
                    a.diags.push(escape(base, &dir));
                    return;
                }
                if !link_ok(root, &dir, trusted, &mut a.diags) {
                    return;
                }
                let (r, d, ledger) = emit::emit_known(&dir, files, vfs, ledgers.get(&dir));
                if d.iter().any(|d| d.code == Code::EmitFailed) {
                    ledgers.remove(&dir); // verify the disk again next time
                } else {
                    ledgers.insert(dir, ledger);
                }
                a.emitted += r.written;
                a.diags.extend(d);
            };
        match plan.kind {
            Kind::Packs => {
                for ((o, p), files) in &packs {
                    let projected = match p {
                        Pack::Behavior => &cfg.packs.behavior_pack,
                        Pack::Resource => &cfg.packs.resource_pack,
                    };
                    let state_out = self.root.join(STATE_DIR).join("out");
                    let (base, dir) = match (&self.opts.out, projected) {
                        (Some(out), _) => (out.clone(), out.join(o.as_str()).join(pack_name(*p))),
                        (None, Some(rel)) if primary.as_ref() == Some(*o) => {
                            (self.root.clone(), self.root.join(trim_rel(rel)))
                        }
                        _ => (
                            state_out.clone(),
                            state_out.join(o.as_str()).join(pack_name(*p)),
                        ),
                    };
                    dir_emit(&base, dir, files, a);
                }
            }
            Kind::Dev => {
                let Some(base) = self.opts.out.clone().or_else(|| com_mojang(plan.edition)) else {
                    a.diags.push(fail(format!(
                        "target {:?}: com.mojang for {:?} was not found; set BEACON_COM_MOJANG or pass --out",
                        plan.target, plan.edition
                    )));
                    return;
                };
                let wt = worktree8(&self.root);
                for ((o, p), files) in &packs {
                    let folder = if !multi || primary.as_ref() == Some(*o) {
                        format!("{name}-{wt} {}", pack_name(*p))
                    } else {
                        format!("{name}-{o}-{wt} {}", pack_name(*p))
                    };
                    let parent = match p {
                        Pack::Behavior => "development_behavior_packs",
                        Pack::Resource => "development_resource_packs",
                    };
                    dir_emit(&base, base.join(parent).join(folder), files, a);
                }
            }
            Kind::McPack | Kind::McAddon => {
                if has_errors(&a.diags) {
                    return; // never ship an artifact from a failed build
                }
                let dir = self
                    .opts
                    .out
                    .clone()
                    .unwrap_or_else(|| self.root.join("dist"));
                if !link_ok(&self.root, &dir, trusted, &mut a.diags) {
                    return;
                }
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    a.diags.push(fail(format!("{}: {e}", dir.display())));
                    return;
                }
                let label = |o: &str, p: Pack| {
                    if multi {
                        format!("{o}-{}", pack_name(p))
                    } else {
                        pack_name(p).to_owned()
                    }
                };
                let base = format!("{name}-{}", safe_name(&plan.target));
                let mut zips: Vec<(String, Vec<(RelPath, &Body)>)> = Vec::new();
                if plan.kind == Kind::McPack {
                    for ((o, p), files) in &packs {
                        let file = if packs.len() == 1 {
                            format!("{base}.mcpack")
                        } else {
                            format!("{base}-{}.mcpack", label(o, *p))
                        };
                        zips.push((file, files.clone()));
                    }
                } else {
                    let mut all = Vec::new();
                    for ((o, p), files) in &packs {
                        let folder = format!("{name} {}", label(o, *p));
                        for (path, body) in files {
                            match RelPath::parse(&folder).and_then(|f| f.join(path.as_str())) {
                                Ok(zp) => all.push((zp, *body)),
                                Err(e) => a.diags.push(fail(e.to_string())),
                            }
                        }
                    }
                    zips.push((format!("{base}.mcaddon"), all));
                }
                for (file, entries) in zips {
                    let path = dir.join(file);
                    if !contained(&dir, &path) {
                        a.diags.push(escape(&dir, &path));
                        continue;
                    }
                    if !link_ok(&self.root, &path, trusted, &mut a.diags) {
                        continue;
                    }
                    match archive::write_zip(&path, &entries, vfs) {
                        Ok(()) => a.artifacts.push(path.display().to_string()),
                        Err(e) => a.diags.push(fail(format!("{}: {e}", path.display()))),
                    }
                }
            }
        }
    }
}

fn bundle(
    memo: &mut HashMap<(OutputId, bool), BundleMemo>,
    bundler: &dyn Bundler,
    root: &Path,
    index: &Index,
    paths: Hash,
    key: &(OutputId, bool),
    entries: &[RelPath],
) -> Bundled {
    if let Some(m) = memo.get(key)
        && m.entries == entries
        && m.paths == paths
        && m.inputs.iter().all(|(p, h)| index.hash(p).ok() == Some(*h))
    {
        return m.out.clone();
    }
    let r = bundler.bundle(&BundleRequest {
        root,
        entries,
        sourcemap: key.1,
        minify: false,
    });
    let external = r.external_inputs;
    let out = Bundled {
        code: r.code,
        map: r.map,
        diagnostics: r.diagnostics,
        inputs: r.inputs,
    };
    // only successful bundles whose inputs are all indexed are reused: never with inputs in
    // node_modules (not indexed) or outside the root (not even listed)
    let hashed: Option<Vec<(RelPath, Hash)>> = out
        .inputs
        .iter()
        .map(|p| index.hash(p).ok().map(|h| (p.clone(), h)))
        .collect();
    match hashed {
        Some(inputs) if !external && out.code.is_some() && out.diagnostics.is_empty() => {
            memo.insert(
                key.clone(),
                BundleMemo {
                    entries: entries.to_vec(),
                    paths,
                    inputs,
                    out: out.clone(),
                },
            );
        }
        _ => {
            memo.remove(key);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::contained;
    use std::path::Path;

    #[test]
    fn containment_is_lexical_and_strict() {
        let b = Path::new("/r/base");
        assert!(contained(b, &b.join("main").join("BP")));
        assert!(!contained(b, b));
        assert!(!contained(b, &b.join("..").join("x")));
        assert!(!contained(b, &b.join("a").join("..").join("..").join("x")));
        assert!(!contained(b, Path::new("/r/other")));
        assert!(!contained(b, Path::new("/r/basement")));
    }
}
