//! `mcpack` (one zip per pack, or one when there is a single pack) and `mcaddon` (every pack
//! in its own top-level folder) into `dist/` (or `--out`).

use super::{ArtifactCx, contained, escape, fail, has_errors, link_ok, pack_name, safe_name};
use crate::build::Attempt;
use crate::config::Artifact;
use crate::model::{Body, Pack};
use crate::path::RelPath;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    if has_errors(&a.diags) {
        return; // never ship an artifact from a failed build
    }
    let (root, trusted) = (cx.root, cx.trusted);
    let dir = cx
        .out
        .map_or_else(|| root.join("dist"), |o| o.to_path_buf());
    if !link_ok(root, &dir, trusted, &mut a.diags) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        a.diags.push(fail(format!("{}: {e}", dir.display())));
        return;
    }
    let (name, multi) = (&cx.name, cx.multi);
    let label = |o: &str, p: Pack| {
        if multi {
            format!("{o}-{}", pack_name(p))
        } else {
            pack_name(p).to_owned()
        }
    };
    let base = format!("{name}-{}", safe_name(&cx.plan.target));
    let mut zips: Vec<(String, Vec<(RelPath, &Body)>)> = Vec::new();
    if cx.plan.artifact == Some(Artifact::McPack) {
        for ((o, p), files) in &cx.packs {
            let file = if cx.packs.len() == 1 {
                format!("{base}.mcpack")
            } else {
                format!("{base}-{}.mcpack", label(o, *p))
            };
            zips.push((file, files.clone()));
        }
    } else {
        let mut all = Vec::new();
        for ((o, p), files) in &cx.packs {
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
        if !link_ok(root, &path, trusted, &mut a.diags) {
            continue;
        }
        match crate::archive::write_zip(&path, &entries, cx.vfs) {
            Ok(()) => a.artifacts.push(path.display().to_string()),
            Err(e) => a.diags.push(fail(format!("{}: {e}", path.display()))),
        }
    }
}
