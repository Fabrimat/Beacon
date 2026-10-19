//! Writing a migration plan (5.6, phase 3 B1) and the `--oracle` comparison.
//!
//! Every destination is an `out`-relative [`RelPath`] (no `..`, nothing absolute), checked
//! for links below `out` before writing; copies are read through the legacy index, which
//! never follows links and re-checks each file's hash. Files are written to a temp name and
//! renamed into place.

use super::equiv::{self, Diff, EquivReport};
use super::plan::{body_bytes, legacy_index, source_hash};
use super::{Change, LegacyProject, MigrateOptions, MigrationPlan};
use crate::diag::{Code, Diagnostic};
use crate::emit::{create_fresh, link_below};
use crate::path::{RelPath, Vfs};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

fn invalid(m: String) -> Vec<Diagnostic> {
    vec![Diagnostic::new(Code::MigrateOutInvalid, m)]
}

fn fold(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn inside(a: &Path, b: &Path) -> bool {
    let (a, b) = (fold(a), fold(b));
    let b = b.trim_end_matches('/');
    a == b || a.starts_with(&format!("{b}/"))
}

/// `--out` must be outside the legacy project (canonical paths: not equal, not inside, not
/// containing it), reached through no symlink or junction, and empty unless `force`.
/// ponytail: links directly below the filesystem root (macOS `/var`, `/tmp`) are allowed.
pub fn check_out(root: &Path, out: &Path, force: bool) -> Result<(), Vec<Diagnostic>> {
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    let mut cur = PathBuf::new();
    let mut depth = 0;
    for c in abs.components() {
        cur.push(c);
        if matches!(c, Component::Normal(_)) {
            depth += 1;
        }
        if depth >= 2 && std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(invalid(format!(
                "--out {}: {} is a symlink or junction",
                out.display(),
                cur.display()
            )));
        }
    }
    let (r, o) = (crate::build::canon(root), crate::build::canon(&abs));
    if inside(&o, &r) || inside(&r, &o) {
        return Err(invalid(format!(
            "--out {} must be outside the legacy project {} (not equal, inside or containing it)",
            out.display(),
            root.display()
        )));
    }
    match std::fs::read_dir(&abs).map(|mut rd| rd.next().is_some()) {
        Ok(true) if !force => Err(invalid(format!(
            "--out {} is not empty; pass --force to write into it",
            out.display()
        ))),
        Err(_) if abs.exists() => Err(invalid(format!(
            "--out {} is not a directory",
            out.display()
        ))),
        _ => Ok(()),
    }
}

/// The first link (symlink/junction) between `out` and `dest`, if any: the CLI's own
/// writes under `--out` (oracle scratch dir, MIGRATION.md) refuse to go through one.
pub fn link_under(out: &Path, dest: &Path) -> Option<PathBuf> {
    link_below(out, dest)
}

/// [`put`] for a path under `out`, refusing to write through a link.
pub fn put_under(out: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    match link_below(out, dest) {
        Some(l) => Err(std::io::Error::other(format!(
            "refusing to write through link {}",
            l.display()
        ))),
        None => put(dest, bytes),
    }
}

/// Temp file in the same dir, then rename (an existing entry is replaced, never followed).
fn put(dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(p) = dest.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".beacon-tmp");
    let tmp = PathBuf::from(tmp);
    let res = (|| {
        let mut f = create_fresh(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, dest)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

pub(super) fn apply(
    project: &LegacyProject,
    plan: &MigrationPlan,
    opts: &MigrateOptions,
) -> Result<(), Vec<Diagnostic>> {
    check_out(&project.root, &opts.out, opts.force)?;
    let (idx, _) = legacy_index(project, opts);
    if source_hash(&idx) != plan.report.source_hash {
        return Err(vec![Diagnostic::new(
            Code::MigrateUnsupported,
            "the legacy project changed since the plan was made; plan again",
        )]);
    }
    if opts.dry_run {
        return Ok(());
    }
    let out = std::path::absolute(&opts.out).unwrap_or_else(|_| opts.out.clone());
    std::fs::create_dir_all(&out).map_err(|e| invalid(format!("--out {}: {e}", out.display())))?;
    let mut diags = Vec::new();
    for ch in &plan.changes {
        let (to, bytes): (RelPath, Result<Vec<u8>, (&RelPath, std::io::Error)>) = match ch {
            Change::Copy { from, to } => (to.clone(), idx.read(from).map_err(|e| (from, e))),
            Change::Write { to, body } => (to.clone(), body_bytes(body, &idx).map_err(|e| (to, e))),
            Change::Config(v) => {
                let mut b = serde_json::to_vec_pretty(v).expect("Value serializes");
                b.push(b'\n');
                (RelPath::parse("config.json").expect("const"), Ok(b))
            }
            Change::Lock(l) => {
                let v = serde_json::to_value(l).expect("lockfile json");
                let to = RelPath::parse(crate::config::LOCK_FILE).expect("const");
                (to, Ok(crate::canonical::to_vec(&v)))
            }
            Change::Note(_) => continue,
        };
        let dest = out.join(to.as_str());
        let res = match (bytes, link_below(&out, &dest)) {
            (Err((p, e)), _) => Err(format!(
                "cannot read {p} ({e}); links and files changed since indexing are refused"
            )),
            (_, Some(l)) => Err(format!("{} is a link; not written through", l.display())),
            (Ok(b), None) => put(&dest, &b).map_err(|e| e.to_string()),
        };
        if let Err(m) = res {
            diags.push(Diagnostic::new(Code::EmitFailed, format!("{to}: {m}")));
        }
    }
    if diags.is_empty() { Ok(()) } else { Err(diags) }
}

/// Root-relative globs never compared with `--oracle` (see `render_report`).
pub const ORACLE_IGNORES: [&str; 3] = [
    "**/*.map",
    // files at the root of the oracle (e.g. `system_template_log.json`) and the legacy
    // `data/` tree are not packs
    "*", "data/**",
];

fn lang_keys(p: &Path) -> Option<Vec<String>> {
    let t = std::fs::read_to_string(p).ok()?;
    Some(
        t.lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with("##"))
            .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().to_string()))
            .collect(),
    )
}

fn is_manifest(path: &str) -> bool {
    path.split('/').count() == 2 && path.ends_with("/manifest.json")
}

/// `/header/uuid|version`, `/modules/N/uuid|version`, `/dependencies/N/uuid` (a dependency's
/// `version` stays compared: for script modules it is the API version).
fn identity(pointer: &str) -> bool {
    let p: Vec<&str> = pointer.trim_start_matches('/').split('/').collect();
    match p.as_slice() {
        ["header", "uuid" | "version"] | ["header", "version", _] => true,
        ["modules", n, "uuid" | "version"] | ["modules", n, "version", _] => {
            n.parse::<usize>().is_ok()
        }
        ["dependencies", n, "uuid"] => n.parse::<usize>().is_ok(),
        _ => false,
    }
}

/// esbuild `// <path>` module headers, as in `equiv` (leading `./` / `../` removed).
fn headers(s: &str) -> Vec<String> {
    s.lines()
        .filter_map(|l| l.strip_prefix("// "))
        // a module path; a comment at column 0 (`// per-window accumulator`) is not one
        .filter(|p| {
            let p = p.trim();
            let code = [".ts", ".js", ".mjs", ".cjs", ".mts", ".cts", ".json"];
            !p.contains(char::is_whitespace)
                && (p.contains('/') || code.iter().any(|e| p.ends_with(e)))
        })
        .map(|p| {
            let mut p = p.trim().replace('\\', "/");
            while let Some(r) = p.strip_prefix("./").or_else(|| p.strip_prefix("../")) {
                p = r.to_string();
            }
            p
        })
        .collect()
}

/// `equiv::compare(oracle, built)` with the migration's normalization: [`ORACLE_IGNORES`];
/// Beacon's own `pack.name` / `pack.description` lang lines (a built-only lang file holding
/// only those, a key missing from the oracle) and generated `texts/languages.json` are not
/// differences; script bundles are compared by module order after mapping each migrated
/// source back to its legacy path (the plan's copies).
pub fn oracle_compare(oracle: &Path, built: &Path, plan: &MigrationPlan) -> EquivReport {
    let ig: Vec<String> = ORACLE_IGNORES.iter().map(|s| s.to_string()).collect();
    let mut rep = equiv::compare(oracle, built, &ig);
    let back: BTreeMap<&str, &str> = plan
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::Copy { from, to } => Some((to.as_str(), from.as_str())),
            _ => None,
        })
        .collect();
    const OWN: [&str; 2] = ["pack.name", "pack.description"];
    rep.diffs.retain(|d| match d {
        // manifests: identities (lockfile-owned, imported) are not compared; anything else
        // (a dropped dependency, a module, metadata) is
        Diff::Json { path, pointer, .. } if is_manifest(path) => !identity(pointer),
        Diff::Extra { path } if path.ends_with("texts/languages.json") => false,
        Diff::Extra { path } if path.ends_with(".lang") => !lang_keys(&built.join(path))
            .is_some_and(|k| k.iter().all(|k| OWN.contains(&k.as_str()))),
        Diff::Lang {
            key, left: None, ..
        } if OWN.contains(&key.as_str()) => false,
        Diff::Bundle { path, .. } => {
            let read = |r: &Path| std::fs::read_to_string(r.join(path)).ok();
            let (Some(a), Some(b)) = (read(oracle), read(built)) else {
                return true;
            };
            // a migrated script maps back to its legacy source: exactly (a plain copy), or
            // by its path below `src/_scripts/` = below the legacy data dir (rewritten
            // copies; system_template bundles in Regolith's tmp, so `data/...`)
            let same = |o: &String, h: &String| {
                // the legacy entry (ModularMC's `.temp_esbuild_entry_<time>.ts`) is the barrel
                if h == "src/_scripts/scripts/main.ts" && o.contains(".temp_esbuild_entry_") {
                    return true;
                }
                let h = match back.get(h.as_str()) {
                    Some(s) => s,
                    None => h.strip_prefix("src/_scripts/").unwrap_or(h),
                };
                if o == h || o.ends_with(&format!("/{h}")) {
                    return true;
                }
                // a `file:` package: npm links it into `node_modules/<pkg>/`, the migrated
                // project resolves it in `vendor/` (ponytail: matched by the path below the
                // package folder)
                let pkg_tail = o.rsplit_once("node_modules/").map(|(_, r)| {
                    let skip = if r.starts_with('@') { 2 } else { 1 };
                    r.splitn(skip + 1, '/').nth(skip).unwrap_or("")
                });
                match pkg_tail {
                    Some(t) if !t.is_empty() && h.starts_with("vendor/") => {
                        h.ends_with(&format!("/{t}"))
                    }
                    _ => false,
                }
            };
            let (ha, hb) = (headers(&a), headers(&b));
            ha.len() != hb.len() || !ha.iter().zip(&hb).all(|(o, h)| same(o, h))
        }
        _ => true,
    });
    rep
}

/// The packs of an archive Beacon built (`.mcworld`, `.mctemplate`, `.mcaddon`, `.mcpack`)
/// as `into/BP` and `into/RP`, told apart by their manifest's module type; the world itself is
/// not extracted. Two passes per archive: the `manifest.json` roots first, then only the
/// files below them, streamed from disk (the outer archive is never buffered; a nested
/// `.mcpack`, one level deep at most, is). Entry names are checked (`enclosed_name`): nothing
/// lands outside `into`. `taken` holds the packs already extracted (across calls): a second
/// BP or RP is not written over the first, it is returned as a diagnostic.
pub fn extract_packs(
    archive: &Path,
    into: &Path,
    taken: &mut std::collections::BTreeSet<&'static str>,
) -> std::io::Result<Vec<Diagnostic>> {
    let f = std::io::BufReader::new(std::fs::File::open(archive)?);
    let mut z = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
    let mut diags = Vec::new();
    let label = archive.display().to_string();
    extract_zip(&mut z, into, taken, &mut diags, &label, true)?;
    Ok(diags)
}

fn pack_kind(manifest: &[u8]) -> Option<&'static str> {
    let v: serde_json::Value = serde_json::from_slice(manifest).ok()?;
    let kinds: Vec<&str> = v["modules"]
        .as_array()?
        .iter()
        .filter_map(|m| m["type"].as_str())
        .collect();
    if kinds.contains(&"resources") {
        Some("RP")
    } else if kinds.iter().any(|k| matches!(*k, "data" | "script")) {
        Some("BP")
    } else {
        None // world template, skin pack
    }
}

fn extract_zip<R: std::io::Read + std::io::Seek>(
    z: &mut zip::ZipArchive<R>,
    into: &Path,
    taken: &mut std::collections::BTreeSet<&'static str>,
    diags: &mut Vec<Diagnostic>,
    label: &str,
    nested_ok: bool,
) -> std::io::Result<()> {
    use std::io::Read;
    let err = std::io::Error::other;
    // pass 1: pack roots (folders holding a pack manifest) and nested packs
    let (mut roots, mut nested) = (Vec::new(), Vec::new());
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(err)?;
        let Some(name) = e.enclosed_name() else {
            continue;
        };
        if e.is_dir() {
            continue;
        }
        if name.file_name().is_some_and(|n| n == "manifest.json") {
            let mut b = Vec::new();
            e.read_to_end(&mut b)?;
            if let Some(kind) = pack_kind(&b) {
                roots.push((name.parent().unwrap_or(Path::new("")).to_path_buf(), kind));
            }
        } else if name.extension().is_some_and(|x| x == "mcpack") {
            if nested_ok {
                nested.push(i);
            } else {
                diags.push(Diagnostic::new(
                    Code::MigrateOracleDiff,
                    format!(
                        "{label}: {} is nested more than one level deep; not compared",
                        name.display()
                    ),
                ));
            }
        }
    }
    let mut claimed = Vec::new();
    for (root, kind) in roots {
        if taken.insert(kind) {
            claimed.push((root, kind));
        } else {
            diags.push(Diagnostic::new(
                Code::MigrateOracleDiff,
                format!(
                    "{label}: a second {kind} ({}) would overwrite the first; it is not compared",
                    root.display()
                ),
            ));
        }
    }
    // pass 2: only the files below a claimed root
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(err)?;
        let Some(name) = e.enclosed_name() else {
            continue;
        };
        if e.is_dir() {
            continue;
        }
        let Some((root, kind)) = claimed.iter().find(|(r, _)| name.starts_with(r)) else {
            continue;
        };
        let Ok(rest) = name.strip_prefix(root) else {
            continue;
        };
        let dest = into.join(kind).join(rest);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::io::copy(&mut e, &mut std::fs::File::create(dest)?)?;
    }
    for i in nested {
        let mut b = Vec::new();
        z.by_index(i).map_err(err)?.read_to_end(&mut b)?;
        let mut inner = zip::ZipArchive::new(std::io::Cursor::new(b)).map_err(err)?;
        extract_zip(&mut inner, into, taken, diags, label, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_folds_and_respects_segments() {
        assert!(inside(Path::new("/a/b"), Path::new("/a")));
        assert!(inside(Path::new("/a"), Path::new("/a/")));
        assert!(!inside(Path::new("/ab"), Path::new("/a")));
    }
}
