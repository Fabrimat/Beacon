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
//! `mcpack`/`mcaddon` to `dist/` (or `--out`). Each artifact kind is written by its module in
//! `artifact/`.

use crate::artifact::{self, ArtifactCx};
use crate::cache::GenCache;
use crate::config::{Artifact, CONFIG_FILE, ProjectConfig, STATE_DIR, TargetDef};
use crate::diag::{Code, Diagnostic, Severity};
use crate::emit::Ledger;
use crate::event::{BuildReport, PackVersion};
use crate::host::{BundleRequest, Bundler, GeneratorHost};
use crate::lock::Lockfile;
use crate::manifest::{self, ManifestInput};
use crate::model::{Body, ContentItem, Dest, Edition, GenCtx, Merge, OutputId, Pack, Version};
use crate::path::{Hash, RelPath, Vfs};
use crate::release::{self, OutGen};
use crate::vfs::Index;
use crate::{merge, packlist, registry, route, sarif, statics};
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
    /// Release build (`beacon pack`, 4.2.1). `None` = pack versions are never touched.
    pub release: Option<ReleaseOptions>,
}

/// Version changes of a release build (4.2.1).
#[derive(Debug, Clone, Default)]
pub struct ReleaseOptions {
    /// `--bump`: bump the targeted packs (all, or `only`) by this much even if unchanged;
    /// changed packs not targeted follow the policy.
    pub bump: Option<Bump>,
    /// `--set-version` (`manual` policy).
    pub set_version: Option<Version>,
    /// `--pack <id>`: only this pack.
    pub only: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bump {
    Patch,
    Minor,
    Major,
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
/// Re-index and retry when a file changes while the build reads it.
const ATTEMPTS: usize = 3;

pub struct Session {
    root: PathBuf,
    opts: BuildOptions,
    host: Box<dyn GeneratorHost>,
    bundler: Option<Box<dyn Bundler>>,
    index: Option<Index>,
    cache: GenCache,
    bundles: HashMap<BundleKey, BundleMemo>,
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
    externals: Vec<String>,
    out: Bundled,
}

/// (output, sourcemap, hash of the target's `define`).
type BundleKey = (OutputId, bool, Hash);

/// One target to build (or the packs build).
pub(crate) struct Plan {
    pub target: String,
    /// `None` = the packs build (no `--target`).
    pub artifact: Option<Artifact>,
    pub edition: Edition,
    pub outputs: Vec<OutputId>,
    /// The target's `targetVersion`, else the project's.
    pub target_version: String,
    pub define: BTreeMap<String, Value>,
    /// The target's definition; `None` for the packs build.
    #[allow(dead_code)] // read by the phase-2 writers (world, skin, store)
    pub def: Option<TargetDef>,
}

impl Plan {
    /// Dev-like builds carry source maps.
    fn sourcemap(&self) -> bool {
        self.artifact.is_none_or(|a| a == Artifact::Dev)
    }

    fn define_hash(&self) -> Hash {
        *blake3::hash(&serde_json::to_vec(&self.define).expect("define json")).as_bytes()
    }
}

#[derive(Default)]
pub(crate) struct Attempt {
    pub diags: Vec<Diagnostic>,
    pub emitted: usize,
    pub artifacts: Vec<String>,
    /// Release builds: the version of every pack written.
    pub packs: Vec<PackVersion>,
    /// Release build: the versions decided so far and the lock they produce.
    pub rel: Option<release::State>,
    pub inputs: BTreeSet<RelPath>,
    pub generators_run: usize,
    pub changed: bool,
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

/// `./packs/BP/` -> `packs/BP`.
pub(crate) fn trim_rel(s: &str) -> &str {
    let mut s = s;
    while let Some(r) = s.strip_prefix("./") {
        s = r;
    }
    s.trim_end_matches('/')
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
        let edition = if bedrock {
            Edition::Bedrock
        } else {
            Edition::Education
        };
        // no target: the first targetVersion given for this edition, else the project's
        let target_version = targets
            .values()
            .filter(|t| t.edition == edition)
            .find_map(|t| t.target_version.clone())
            .unwrap_or_else(|| cfg.target_version.clone());
        let plan = Plan {
            target: PACKS_TARGET.into(),
            artifact: None,
            edition,
            outputs: all.to_vec(),
            target_version,
            define: BTreeMap::new(),
            def: None,
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
            artifact: Some(t.artifact),
            edition: t.edition,
            outputs,
            target_version: t
                .target_version
                .clone()
                .unwrap_or_else(|| cfg.target_version.clone()),
            define: t.define.clone(),
            def: Some(t.clone()),
        });
    }
    (out, diags)
}

/// Adds `texts/languages.json` to every pack that has `texts/*.lang` and no such file
/// (static packs are included as-is: never).
fn languages(items: &mut Vec<ContentItem>, is_static: &dyn Fn(&OutputId) -> bool) {
    let mut langs: BTreeMap<(OutputId, Pack), Vec<String>> = BTreeMap::new();
    let mut present = BTreeSet::new();
    for i in items.iter().filter(|i| !is_static(&i.dest.output)) {
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
                packs: a.packs,
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
        let mut routed = route::route(&cfg, &vfs);
        a.diags.append(&mut routed.diagnostics);
        let (st, d) = statics::items(&cfg, &vfs);
        routed.items.extend(st);
        a.diags.extend(d);
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

        // script bundles, once per (output, sourcemap, define)
        let mut bundles: BTreeMap<BundleKey, Bundled> = BTreeMap::new();
        if trusted {
            let paths = {
                let mut h = blake3::Hasher::new();
                for f in index.files() {
                    h.update(f.as_str().as_bytes());
                    h.update(&[0]);
                }
                *h.finalize().as_bytes()
            };
            // the declared script modules (what manifest.rs writes as dependencies)
            let externals: Vec<String> = cfg
                .beacon
                .script_modules
                .keys()
                .cloned()
                .chain(["@minecraft/server".to_owned()])
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            for plan in &plans {
                let (sourcemap, define) = (plan.sourcemap(), plan.define_hash());
                for o in &plan.outputs {
                    let Some(entries) = routed.script_entries.get(o) else {
                        continue;
                    };
                    let key = (o.clone(), sourcemap, define);
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
                        &plan.define,
                        &externals,
                    );
                    a.diags.extend(b.diagnostics.iter().cloned());
                    a.inputs.extend(b.inputs.iter().cloned());
                    bundles.insert(key, b);
                }
            }
        }

        // world template and skin pack identities, from every target (like editions)
        let (mut worlds, mut skins) = (Vec::new(), Vec::new());
        for t in cfg.beacon.targets.values() {
            if let (Artifact::Marketplace, Some(d)) = (t.artifact, &t.skins) {
                skins.push((trim_rel(d).to_owned(), t.edition)); // store zip embeds the skin pack
            }
            let (list, dir) = match t.artifact {
                Artifact::McTemplate | Artifact::Marketplace => (&mut worlds, &t.world),
                Artifact::SkinPack => (&mut skins, &t.skins),
                _ => continue,
            };
            if let Some(d) = dir {
                list.push((trim_rel(d).to_owned(), t.edition));
            }
        }
        // static outputs keep their own UUIDs and manifests (4.3): nothing to mint
        let is_static = |id: &OutputId| {
            cfg.beacon
                .outputs
                .iter()
                .any(|o| &o.id == id && o.static_packs.is_some())
        };
        let minted: Vec<OutputId> = outputs.iter().filter(|o| !is_static(o)).cloned().collect();
        let lock = if emit {
            match Lockfile::ensure(&self.root, &cfg, &minted, &worlds, &skins, self.opts.frozen) {
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

        if let (Some(l), Some(r)) = (&lock, &self.opts.release) {
            if !plans.is_empty() && plans.iter().all(|p| p.artifact == Some(Artifact::Dev)) {
                a.diags.push(Diagnostic::new(
                    Code::ConfigField,
                    "no release targets: dev targets never change versions; pass an archive target or run `beacon pack` without --target",
                ));
            }
            // `--pack` ids must name a pack of a release plan
            let mut ids = BTreeSet::new();
            for plan in plans.iter().filter(|p| p.artifact != Some(Artifact::Dev)) {
                for o in plan.outputs.iter().filter(|o| !is_static(o)) {
                    ids.extend([o.clone(), format!("{o}/BP"), format!("{o}/RP")]);
                }
                let (world, skins) = match (plan.artifact, plan.def.as_ref()) {
                    (Some(Artifact::McTemplate), Some(d)) => (d.world.as_ref(), None),
                    (Some(Artifact::Marketplace), Some(d)) => (d.world.as_ref(), d.skins.as_ref()),
                    (Some(Artifact::SkinPack), Some(d)) => (None, d.skins.as_ref()),
                    _ => (None, None),
                };
                ids.extend(world.map(|d| format!("world:{}", trim_rel(d))));
                ids.extend(skins.map(|d| format!("skins:{}", trim_rel(d))));
            }
            if let Some(bad) = r.only.iter().find(|o| !ids.contains(*o)) {
                let valid: Vec<&str> = ids.iter().map(String::as_str).collect();
                a.diags.push(Diagnostic::new(
                    Code::ConfigField,
                    format!(
                        "--pack {bad:?} matches no pack of this release; valid ids: {}",
                        valid.join(", ")
                    ),
                ));
            }
            a.rel = Some(release::State::new(
                r.clone(),
                &cfg,
                self.opts.frozen,
                l.clone(),
            ));
        }
        let mut built = Vec::with_capacity(plans.len());
        let mut blocked = Vec::with_capacity(plans.len());
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
            let key = |o: &OutputId| (o.clone(), plan.sourcemap(), plan.define_hash());
            let mut scripted = BTreeSet::new();
            for o in &plan.outputs {
                let Some(b) = bundles.get(&key(o)) else {
                    continue;
                };
                let Some(code) = &b.code else { continue };
                let entry = &routed.script_entries[o][0];
                let module = entry
                    .parent()
                    .and_then(|p| p.parent())
                    .unwrap_or(entry.clone());
                let entry_path = cfg.script_entry(o);
                // the bundler names its output `main.js`: keep the map comment in step
                let code = &code.replace(
                    "sourceMappingURL=main.js.map",
                    &format!(
                        "sourceMappingURL={}.map",
                        entry_path.rsplit('/').next().unwrap_or(entry_path)
                    ),
                );
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
                add(entry_path.into(), code);
                if let Some(m) = &b.map {
                    add(format!("{entry_path}.map"), m);
                }
                scripted.insert(o.clone());
            }
            let mut gens = Vec::new();
            if let Some(lock) = &lock {
                for o in plan.outputs.iter().filter(|o| !is_static(o)) {
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
                        target_version: &plan.target_version,
                        dev_worktree: (plan.artifact == Some(Artifact::Dev))
                            .then_some(self.root.as_path()),
                        script_entry: scripted.contains(o).then(|| cfg.script_entry(o)),
                        has_bp,
                        has_rp,
                    }));
                    gens.push(OutGen {
                        output: o.clone(),
                        has_bp,
                        has_rp,
                        script_entry: scripted.contains(o).then(|| cfg.script_entry(o).to_owned()),
                    });
                }
            }
            let (derived, d) = registry::derive(&cfg, &routed, plan, &items, &vfs);
            items.extend(derived);
            a.diags.extend(d);
            let (mut merged, d) = merge::resolve(items, &vfs);
            a.diags.extend(d);
            registry::after_merge(&cfg, &mut merged, &vfs, &is_static);
            languages(&mut merged, &is_static);
            let mut lf_failed = false;
            if trusted {
                lf_failed = crate::legacy_filter::apply(
                    &self.root,
                    &cfg.beacon.plugins,
                    &mut merged,
                    &vfs,
                    &is_static,
                    &mut a.diags,
                );
            }
            // release builds decide versions; archive builds only report a stale release
            let archive = plan.artifact.is_some_and(|x| x != Artifact::Dev);
            if let Some(lock) = &lock
                && (archive || (a.rel.is_some() && plan.artifact != Some(Artifact::Dev)))
            {
                release::settle_packs(
                    &mut a.rel,
                    lock,
                    &mut a.diags,
                    &cfg,
                    plan,
                    &gens,
                    &mut merged,
                    &vfs,
                );
            }
            // two packs with one UUID: report it, and write nothing for this plan
            let present: BTreeSet<(&OutputId, Pack)> = merged
                .iter()
                .map(|i| (&i.dest.output, i.dest.pack))
                .collect();
            let refs = packlist::refs(
                &cfg,
                lock.as_ref(),
                &vfs,
                &plan.outputs,
                plan.edition,
                (plan.artifact == Some(Artifact::Dev)).then_some(self.root.as_path()),
                &|o, p| present.contains(&(o, p)),
            );
            let dups = packlist::duplicates(&refs);
            blocked.push(!dups.is_empty() || lf_failed);
            a.diags.extend(
                dups.into_iter()
                    .map(|d| d.at(RelPath::parse(CONFIG_FILE).expect("const"), None, None)),
            );
            built.push(merged);
        }
        // only an attempt that saw a consistent snapshot writes anything
        if let Some(lock) = lock.as_ref().filter(|_| emit)
            && !vfs.changed.load(Ordering::Relaxed)
        {
            // a failed release writes nothing and records nothing
            let skip = a.rel.is_some() && artifact::has_errors(&a.diags);
            let releasing = a.rel.is_some(); // dev targets are not part of a release
            for ((plan, merged), _) in
                plans
                    .iter()
                    .zip(&built)
                    .zip(&blocked)
                    .filter(|((plan, _), blocked)| {
                        !(**blocked || skip || releasing && plan.artifact == Some(Artifact::Dev))
                    })
            {
                let work = a.rel.as_ref().map(|s| s.work.clone());
                let cx = ArtifactCx::new(
                    &self.root,
                    plan,
                    &cfg,
                    work.as_ref().unwrap_or(lock),
                    &outputs,
                    merged,
                    &vfs,
                    self.opts.trusted,
                    self.opts.out.as_deref(),
                    &mut self.ledgers,
                );
                artifact::write(&cx, &mut a);
            }
            // the lock changes only after every artifact succeeded
            if let Some(st) = a.rel.take()
                && !skip
                && !artifact::has_errors(&a.diags)
            {
                a.packs = st.packs;
                if st.work != *lock
                    && let Err(e) = st.work.save(&self.root)
                {
                    a.packs.clear();
                    a.diags.push(Diagnostic::new(
                        Code::EmitFailed,
                        format!("cannot write {}: {e}", crate::config::LOCK_FILE),
                    ));
                }
            }
        }
        a.changed = vfs.changed.load(Ordering::Relaxed);
        self.index = Some(index);
        a
    }
}

#[allow(clippy::too_many_arguments)]
fn bundle(
    memo: &mut HashMap<BundleKey, BundleMemo>,
    bundler: &dyn Bundler,
    root: &Path,
    index: &Index,
    paths: Hash,
    key: &BundleKey,
    entries: &[RelPath],
    define: &BTreeMap<String, Value>,
    externals: &[String],
) -> Bundled {
    if let Some(m) = memo.get(key)
        && m.entries == entries
        && m.externals == externals
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
        define,
        externals,
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
                    externals: externals.to_vec(),
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
