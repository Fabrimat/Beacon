//! `mcworld` / `mctemplate` into `dist/` (or `--out`): the target's world folder with the
//! edited `level.dat`, `levelname.txt`, the pack lists and (unless `embedPacks: false`) the
//! target's generated packs under `behavior_packs/` and `resource_packs/`. A template adds
//! `manifest.json` and `texts/`. Reads the world through the VFS only.

use super::{ArtifactCx, contained, escape, fail, has_errors, link_ok, pack_name, safe_name};
use crate::build::{Attempt, trim_rel};
use crate::config::{Artifact, CONFIG_FILE};
use crate::diag::{Code, Diagnostic};
use crate::model::{Body, Pack};
use crate::path::RelPath;
use crate::world::{self, LevelSpec};
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    if has_errors(&a.diags) {
        return; // never ship an artifact from a failed build
    }
    let plan = cx.plan;
    let (Some(def), Some(kind)) = (&plan.def, plan.artifact) else {
        return;
    };
    let cfg_at = |d: Diagnostic| d.at(RelPath::parse(CONFIG_FILE).expect("const"), None, None);
    let not_found = |msg: String| Diagnostic::new(Code::WorldNotFound, msg);
    let Some(world_dir) = def.world.as_deref().map(trim_rel) else {
        a.diags.push(cfg_at(not_found(format!(
            "target {:?}: no `world` folder",
            plan.target
        ))));
        return;
    };
    let Ok(world_rel) = RelPath::parse(world_dir) else {
        a.diags.push(cfg_at(not_found(format!(
            "target {:?}: bad world folder {world_dir:?}",
            plan.target
        ))));
        return;
    };
    let files = world::collect(cx.vfs, &world_rel);
    if !files.iter().any(|(r, _)| r.as_str() == "level.dat") {
        a.diags.push(not_found(format!(
            "world folder {world_dir:?} does not exist or has no level.dat"
        )));
        return;
    }
    let level = world::read_level_dat(cx.vfs, &world_rel).and_then(|bytes| {
        world::transform_level_dat(
            &bytes,
            &LevelSpec {
                edition: plan.edition,
                target_version: &plan.target_version,
                name: def.world_name.as_deref().unwrap_or(&cx.cfg.name),
                experiments: &def.experiments,
                overrides: &def.level_dat,
            },
        )
    });
    let level = match level {
        Ok(b) => b,
        Err(e) => {
            a.diags.push(Diagnostic::new(
                Code::LevelDatInvalid,
                format!("{world_dir}/level.dat: {e}"),
            ));
            return;
        }
    };

    // pack lists (output order) and the embedded packs' zip folders
    let (mut bps, mut rps) = (Vec::new(), Vec::new());
    let mut embedded: Vec<(RelPath, &Body)> = Vec::new();
    for o in &plan.outputs {
        for p in [Pack::Behavior, Pack::Resource] {
            let Some(pack_files) = cx.packs.get(&(o.clone(), p)) else {
                continue;
            };
            // ponytail: static outputs have no lock entry yet (workstream B1 adds their refs)
            if let Some(r) = cx.lock.pack_ref(o, p, plan.edition, None) {
                if p == Pack::Behavior {
                    &mut bps
                } else {
                    &mut rps
                }
                .push(r);
            }
            if !def.embed_packs {
                continue;
            }
            let label = if cx.multi {
                format!("{o}-{}", pack_name(p))
            } else {
                pack_name(p).to_owned()
            };
            let top = if p == Pack::Behavior {
                "behavior_packs"
            } else {
                "resource_packs"
            };
            for (path, body) in pack_files {
                match RelPath::parse(&format!("{top}/{} {label}/{path}", cx.name)) {
                    Ok(zp) => embedded.push((zp, *body)),
                    Err(e) => a.diags.push(fail(e.to_string())),
                }
            }
        }
    }
    if !def.embed_packs && (!bps.is_empty() || !rps.is_empty()) {
        a.diags.push(cfg_at(Diagnostic::new(
            Code::EmbedPacksMissing,
            format!(
                "target {:?}: embedPacks is false; the world lists packs the artifact does not contain",
                plan.target
            ),
        )));
    }

    // generated world files; they replace same-named world files
    let rel = |s: &str| RelPath::parse(s).expect("static path");
    let (bp_list, rp_list) = world::pack_lists(&bps, &rps);
    let mut gen_files: Vec<(RelPath, Body)> = vec![
        (rel("level.dat"), Body::Bytes(level)),
        (
            rel("levelname.txt"),
            Body::Text(
                def.world_name
                    .clone()
                    .unwrap_or_else(|| cx.cfg.name.clone()),
            ),
        ),
        (rel("world_behavior_packs.json"), Body::Json(bp_list)),
        (rel("world_resource_packs.json"), Body::Json(rp_list)),
    ];
    if kind == Artifact::McTemplate {
        let Some(ids) = cx
            .lock
            .worlds
            .get(world_dir)
            .and_then(|w| w.editions.get(&plan.edition))
        else {
            a.diags.push(fail(format!(
                "no lock entry for world {world_dir:?} ({:?})",
                plan.edition
            )));
            return;
        };
        let meta = cx.cfg.beacon.pack_meta.as_ref();
        let name = meta.and_then(|m| m.name.as_deref()).unwrap_or(&cx.cfg.name);
        let desc = meta.and_then(|m| m.description.as_deref()).unwrap_or("");
        let clean = |s: &str| s.replace(['\r', '\n'], " ");
        let t = &def.template;
        // ponytail: release version semantics (4.2.1) come with `beacon pack`; always 1.0.0
        let version = [1, 0, 0];
        let base = world::version_parts(
            t.base_game_version
                .as_deref()
                .unwrap_or(&plan.target_version),
        );
        gen_files.extend([
            (
                rel("manifest.json"),
                Body::Json(json!({
                    "format_version": 2,
                    "header": {
                        "name": "pack.name",
                        "description": "pack.description",
                        "uuid": ids.header,
                        "version": version,
                        "base_game_version": base,
                        "lock_template_options": t.lock_template_options,
                        "allow_random_seed": t.allow_random_seed,
                    },
                    "modules": [{
                        "type": "world_template",
                        "uuid": ids.module,
                        "version": version,
                    }],
                })),
            ),
            (
                rel("texts/en_US.lang"),
                Body::Text(format!(
                    "pack.name={}\npack.description={}\n",
                    clean(name),
                    clean(desc)
                )),
            ),
            (rel("texts/languages.json"), Body::Json(json!(["en_US"]))),
        ]);
    }

    // world files, then generated ones and the embedded packs replace by path
    let src: Vec<(RelPath, Body)> = files
        .into_iter()
        .map(|(r, p)| (r, Body::Passthrough(p)))
        .collect();
    let mut entries: BTreeMap<&RelPath, &Body> = BTreeMap::new();
    for (p, b) in src.iter().chain(&gen_files) {
        entries.insert(p, b);
    }
    for (p, b) in &embedded {
        entries.insert(p, b);
    }
    let entries: Vec<(RelPath, &Body)> = entries.into_iter().map(|(p, b)| (p.clone(), b)).collect();

    let dir = cx
        .out
        .map_or_else(|| cx.root.join("dist"), |o| o.to_path_buf());
    if !link_ok(cx.root, &dir, cx.trusted, &mut a.diags) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        a.diags.push(fail(format!("{}: {e}", dir.display())));
        return;
    }
    let ext = if kind == Artifact::McTemplate {
        "mctemplate"
    } else {
        "mcworld"
    };
    let path = dir.join(format!("{}-{}.{ext}", cx.name, safe_name(&plan.target)));
    if !contained(&dir, &path) {
        a.diags.push(escape(&dir, &path));
        return;
    }
    if !link_ok(cx.root, &path, cx.trusted, &mut a.diags) {
        return;
    }
    match crate::archive::write_zip(&path, &entries, cx.vfs) {
        Ok(()) => a.artifacts.push(path.display().to_string()),
        Err(e) => a.diags.push(fail(format!("{}: {e}", path.display()))),
    }
}
