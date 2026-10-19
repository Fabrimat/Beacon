//! Per-artifact writers (4.4). [`write`] dispatches on the plan's artifact; each kind lives in
//! its own module with the same signature, so phase-2 kinds fill their bodies there.
//!
//! - packs build (no target): [`packs`]; `dev`: [`dev`]; `mcpack`/`mcaddon`: [`archive`]
//! - `mcworld`/`mctemplate`: [`world`]; `skinpack`: [`skin`]; `marketplace`: [`store`]

mod archive;
mod dev;
mod packs;
mod skin;
mod store;
mod world;

use crate::build::{Attempt, Plan};
use crate::config::ProjectConfig;
use crate::diag::{Code, Diagnostic, Severity};
use crate::emit::{self, Ledger};
use crate::lock::Lockfile;
use crate::model::{Body, ContentItem, Edition, OutputId, Pack};
use crate::path::{RelPath, Vfs};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Everything an artifact writer needs for one plan.
pub(crate) struct ArtifactCx<'a> {
    /// Canonical project root.
    pub root: &'a Path,
    pub plan: &'a Plan,
    pub cfg: &'a ProjectConfig,
    pub lock: &'a Lockfile,
    /// Every output id, in config order.
    #[allow(dead_code)] // pack order of worlds (4.3)
    pub outputs: &'a [OutputId],
    /// Merged items of the plan per pack: pack-relative path -> body.
    pub packs: BTreeMap<(OutputId, Pack), Vec<(RelPath, &'a Body)>>,
    pub vfs: &'a (dyn Vfs + Sync),
    pub trusted: bool,
    /// `--out`.
    pub out: Option<&'a Path>,
    /// Projected output (4.3): `primaryOutput`, else the first with a BP (else any pack).
    pub primary: Option<OutputId>,
    /// File/folder-safe project name.
    pub name: String,
    /// More than one output has packs.
    pub multi: bool,
    /// Last ledger written per pack dir (session state).
    pub ledgers: RefCell<&'a mut HashMap<PathBuf, Ledger>>,
}

impl<'a> ArtifactCx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        root: &'a Path,
        plan: &'a Plan,
        cfg: &'a ProjectConfig,
        lock: &'a Lockfile,
        outputs: &'a [OutputId],
        merged: &'a [ContentItem],
        vfs: &'a (dyn Vfs + Sync),
        trusted: bool,
        out: Option<&'a Path>,
        ledgers: &'a mut HashMap<PathBuf, Ledger>,
    ) -> Self {
        let mut packs: BTreeMap<(OutputId, Pack), Vec<(RelPath, &Body)>> = BTreeMap::new();
        for i in merged {
            packs
                .entry((i.dest.output.clone(), i.dest.pack))
                .or_default()
                .push((i.dest.path.clone(), &i.body));
        }
        let primary = cfg.beacon.primary_output.clone().or_else(|| {
            let with = |p: Option<Pack>| {
                outputs.iter().find(|o| {
                    packs
                        .keys()
                        .any(|(k, kp)| k == *o && p.is_none_or(|p| p == *kp))
                })
            };
            with(Some(Pack::Behavior)).or(with(None)).cloned()
        });
        let multi = packs.keys().map(|k| &k.0).collect::<BTreeSet<_>>().len() > 1;
        ArtifactCx {
            root,
            plan,
            cfg,
            lock,
            outputs,
            packs,
            vfs,
            trusted,
            out,
            primary,
            name: safe_name(&cfg.name),
            multi,
            ledgers: RefCell::new(ledgers),
        }
    }

    /// Emits `files` into `dir` (strictly below `base`) through the ledger, after the
    /// containment and link checks.
    pub fn emit_dir(&self, base: &Path, dir: PathBuf, files: &[(RelPath, &Body)], a: &mut Attempt) {
        if !contained(base, &dir) {
            a.diags.push(escape(base, &dir));
            return;
        }
        if !link_ok(self.root, &dir, self.trusted, &mut a.diags) {
            return;
        }
        let mut ledgers = self.ledgers.borrow_mut();
        let (r, d, ledger) = emit::emit_known(&dir, files, self.vfs, ledgers.get(&dir));
        if d.iter().any(|d| d.code == Code::EmitFailed) {
            ledgers.remove(&dir); // verify the disk again next time
        } else {
            ledgers.insert(dir, ledger);
        }
        a.emitted += r.written;
        a.diags.extend(d);
    }
}

/// Writes one plan's artifact.
pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    use crate::config::Artifact::*;
    match cx.plan.artifact {
        None => packs::write(cx, a),
        Some(Dev) => dev::write(cx, a),
        Some(McPack | McAddon) => archive::write(cx, a),
        Some(McWorld | McTemplate) => world::write(cx, a),
        Some(SkinPack) => skin::write(cx, a),
        Some(Marketplace) => store::write(cx, a),
    }
}

/// Defence in depth (config validation already rejects path-like ids and `packs.*` escapes):
/// `dir` must be strictly below `base`, lexically, with no `..`/root/prefix components.
pub(crate) fn contained(base: &Path, dir: &Path) -> bool {
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
pub(crate) fn link_ok(root: &Path, dir: &Path, trusted: bool, diags: &mut Vec<Diagnostic>) -> bool {
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

pub(crate) fn escape(base: &Path, dir: &Path) -> Diagnostic {
    Diagnostic::new(
        Code::Internal,
        format!(
            "refusing to write {}: not inside {}",
            dir.display(),
            base.display()
        ),
    )
}

pub(crate) fn has_errors(d: &[Diagnostic]) -> bool {
    d.iter().any(|d| d.severity == Severity::Error)
}

pub(crate) fn fail(what: String) -> Diagnostic {
    Diagnostic::new(Code::EmitFailed, what)
}

pub(crate) fn pack_name(p: Pack) -> &'static str {
    match p {
        Pack::Behavior => "BP",
        Pack::Resource => "RP",
    }
}

/// File/folder name from the project name: `[A-Za-z0-9 ._-]`, others become `_`.
pub(crate) fn safe_name(s: &str) -> String {
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
pub(crate) fn com_mojang(edition: Edition) -> Option<PathBuf> {
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
pub(crate) fn worktree8(root: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.into());
    let s = root.to_string_lossy();
    let s = if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.into_owned()
    };
    blake3::hash(s.as_bytes()).to_hex()[..8].to_owned()
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
