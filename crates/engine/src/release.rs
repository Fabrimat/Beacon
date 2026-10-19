//! Release builds (4.2.1): content hashes, version decisions, the in-memory lock of a
//! `beacon pack` run. Nothing here writes: the build saves the lock after every artifact.

use crate::artifact::pack_name;
use crate::build::{Bump, Plan, ReleaseOptions};
use crate::canonical;
use crate::config::{LOCK_FILE, ProjectConfig, VersionPolicy};
use crate::diag::{Code, Diagnostic};
use crate::event::PackVersion;
use crate::lock::{Lockfile, Release};
use crate::manifest::{self, ManifestInput};
use crate::model::{Body, ContentItem, Edition, OutputId, Pack, Version};
use crate::path::Vfs;
use serde_json::{Value, json};

/// Content hash (hex) of a pack's logical content: blake3 over the sorted (path, blake3)
/// pairs; `manifest.json` is hashed with its version and UUID fields blanked (header and
/// modules), so the pack's own identity never changes its hash but its dependencies do.
pub fn content_hash<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a Body)>,
    vfs: &dyn Vfs,
) -> String {
    let mut pairs: Vec<(&str, [u8; 32])> = files
        .into_iter()
        .map(|(p, b)| (p, body_hash(p, b, vfs)))
        .collect();
    pairs.sort();
    let mut h = blake3::Hasher::new();
    for (p, d) in pairs {
        h.update(p.as_bytes());
        h.update(&[0]);
        h.update(&d);
    }
    h.finalize().to_hex().to_string()
}

fn body_hash(path: &str, body: &Body, vfs: &dyn Vfs) -> [u8; 32] {
    let b = |v: &[u8]| *blake3::hash(v).as_bytes();
    match body {
        // an unreadable file fails the artifact write itself
        Body::Passthrough(src) => vfs.hash(src).unwrap_or_default(),
        Body::Json(v) if path == "manifest.json" => b(&canonical::to_vec(&blanked(v))),
        Body::Json(v) => b(&canonical::to_vec(v)),
        Body::Text(t) => b(t.as_bytes()),
        Body::Bytes(x) => b(x),
    }
}

fn blanked(v: &Value) -> Value {
    let mut v = v.clone();
    let blank = |o: &mut Value| {
        if let Some(o) = o.as_object_mut() {
            o.remove("version");
            o.remove("uuid");
        }
    };
    if let Some(h) = v.get_mut("header") {
        blank(h);
    }
    for m in v
        .get_mut("modules")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        blank(m);
    }
    v
}

/// Sets `header.version` and every `modules[].version` of a manifest body.
pub(crate) fn stamp(body: &mut Body, v: Version) {
    let Body::Json(j) = body else { return };
    if let Some(h) = j.get_mut("header").and_then(Value::as_object_mut) {
        h.insert("version".into(), json!(v));
    }
    for m in j
        .get_mut("modules")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if let Some(m) = m.as_object_mut() {
            m.insert("version".into(), json!(v));
        }
    }
}

fn bump(v: Version, b: Bump) -> Version {
    match b {
        Bump::Patch => [v[0], v[1], v[2] + 1],
        Bump::Minor => [v[0], v[1] + 1, 0],
        Bump::Major => [v[0] + 1, 0, 0],
    }
}

/// One `beacon pack` run: the lock with this run's releases applied (`work`) and the
/// versions decided so far.
pub(crate) struct State {
    opts: ReleaseOptions,
    policy: VersionPolicy,
    frozen: bool,
    /// `sharedEditionUuids`: one lock entry for both editions; releases are kept per edition.
    shared: bool,
    pub work: Lockfile,
    pub packs: Vec<PackVersion>,
}

impl State {
    pub fn new(opts: ReleaseOptions, cfg: &ProjectConfig, frozen: bool, work: Lockfile) -> Self {
        State {
            opts,
            policy: cfg.beacon.version_policy,
            frozen,
            shared: cfg.beacon.shared_edition_uuids,
            work,
            packs: Vec::new(),
        }
    }

    /// Decides the version of pack `key` from its content `hash` and records the release.
    /// A pack already decided in this run (another target of the same edition) keeps that
    /// version; different content for the same release entry is an error.
    fn settle(&mut self, key: &str, ed: Edition, hash: String) -> Result<Version, Diagnostic> {
        if let Some(p) = self.packs.iter().find(|p| p.pack == key && p.edition == ed) {
            if p.hash != hash {
                return Err(Diagnostic::new(
                    Code::ConfigField,
                    format!(
                        "{key} ({ed:?}): targets produce different content for the same release entry; give them different editions or make their content equal"
                    ),
                ));
            }
            return Ok(p.version);
        }
        let err = |c: Code, m: String| Diagnostic::new(c, format!("{key}: {m}"));
        let Some(cur) = self.work.entry(key, ed).map(|e| e.release_of(ed).cloned()) else {
            return Err(err(Code::EmitFailed, "no lock entry".into()));
        };
        let base = cur.as_ref().map_or([1, 0, 0], |r| r.version);
        let changed = cur.as_ref().is_some_and(|r| r.hash != hash);
        let only = &self.opts.only;
        let mine = only.is_empty()
            || only
                .iter()
                .any(|o| o == key || key.rsplit_once('/').is_some_and(|(out, _)| out == o));
        let stale = || {
            err(
                Code::PackVersionStale,
                format!(
                    "content changed since its last release but the version is still {}.{}.{}; pass `beacon pack --set-version <x.y.z>` (archives written earlier in this run may remain in dist)",
                    base[0], base[1], base[2]
                ),
            )
        };
        let v = match (
            self.opts.set_version.filter(|_| mine),
            self.opts.bump.filter(|_| mine),
        ) {
            (Some(s), _) if cur.is_some() && s < base => {
                return Err(err(
                    Code::PackVersionNotMonotonic,
                    format!(
                        "{}.{}.{} is lower than the released {}.{}.{}",
                        s[0], s[1], s[2], base[0], base[1], base[2]
                    ),
                ));
            }
            (Some(s), _) if changed && s == base => return Err(stale()),
            (Some(s), _) => s,
            (None, Some(b)) => bump(base, b),
            (None, None) if changed && self.policy == VersionPolicy::Manual => {
                return Err(stale());
            }
            (None, None) if changed => bump(base, Bump::Patch),
            (None, None) => base,
        };
        if self.frozen && (cur.is_none() || v != base) {
            return Err(err(
                Code::LockfileFrozen,
                format!("{LOCK_FILE} would change but --frozen was given"),
            ));
        }
        let shared = self.shared && !key.contains(':'); // worlds and skins are per edition
        if let Some(e) = self.work.entry_mut(key, ed) {
            let r = Release {
                version: v,
                hash: hash.clone(),
            };
            if shared {
                e.releases.insert(ed, r);
            } else {
                e.release = Some(r);
            }
        }
        self.packs.push(PackVersion {
            pack: key.into(),
            edition: ed,
            version: v,
            bumped: cur.is_some() && v != base,
            hash,
        });
        Ok(v)
    }
}

/// The version a pack ships with. Release build: decided now. Otherwise the last release,
/// with `PackReleaseBehind` when `hash` differs from it. `None` after an error diagnostic.
pub(crate) fn resolve(
    rel: &mut Option<State>,
    lock: &Lockfile,
    diags: &mut Vec<Diagnostic>,
    key: &str,
    ed: Edition,
    hash: String,
) -> Option<Version> {
    if let Some(st) = rel {
        return st.settle(key, ed, hash).map_err(|d| diags.push(d)).ok();
    }
    let Some(r) = lock.entry(key, ed).and_then(|e| e.release_of(ed)) else {
        return Some([1, 0, 0]);
    };
    if r.hash != hash {
        diags.push(Diagnostic::new(
            Code::PackReleaseBehind,
            format!(
                "{key} changed since its last release ({}.{}.{}); `beacon pack` will bump it",
                r.version[0], r.version[1], r.version[2]
            ),
        ));
    }
    Some(r.version)
}

/// A generated output of a plan: which packs exist and the script entry in its manifest.
pub(crate) struct OutGen {
    pub output: OutputId,
    pub has_bp: bool,
    pub has_rp: bool,
    pub script_entry: Option<&'static str>,
}

/// Settles the BP and RP of every generated output of `plan` (RP first: the BP manifest pins
/// the RP version, so its hash depends on the RP decision). In a release build the manifests
/// in `merged` are regenerated with the settled versions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle_packs(
    rel: &mut Option<State>,
    lock: &Lockfile,
    diags: &mut Vec<Diagnostic>,
    cfg: &ProjectConfig,
    plan: &Plan,
    gens: &[OutGen],
    merged: &mut [ContentItem],
    vfs: &dyn Vfs,
) {
    for g in gens {
        for pack in [Pack::Resource, Pack::Behavior] {
            if !(if pack == Pack::Resource {
                g.has_rp
            } else {
                g.has_bp
            }) {
                continue;
            }
            let files = merged
                .iter()
                .filter(|i| i.dest.output == g.output && i.dest.pack == pack)
                .map(|i| (i.dest.path.as_str(), &i.body));
            let hash = content_hash(files, vfs);
            let key = format!("{}/{}", g.output, pack_name(pack));
            if resolve(rel, lock, diags, &key, plan.edition, hash).is_none() {
                return;
            }
            let Some(st) = rel.as_ref() else { continue };
            let generated = manifest::generate(&ManifestInput {
                cfg,
                lock: &st.work,
                output: &g.output,
                edition: plan.edition,
                target_version: &plan.target_version,
                dev_worktree: None,
                script_entry: g.script_entry,
                has_bp: g.has_bp,
                has_rp: g.has_rp,
            });
            for n in generated
                .into_iter()
                .filter(|i| i.dest.path.as_str() == "manifest.json")
            {
                if let Some(m) = merged.iter_mut().find(|m| m.dest == n.dest) {
                    m.body = n.body;
                }
            }
        }
    }
}
