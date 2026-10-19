//! `dev`: `development_*_packs/<name>-<worktree8> BP|RP` in the edition's com.mojang
//! (`BEACON_COM_MOJANG`, else detected; `--out` replaces it).
//!
//! With more than one `dev` target of the edition, the target joins the folder name
//! (`<name>-<target>-<worktree8>`): same-named folders would share a ledger, so each deploy
//! would delete the other's files.
//!
//! The managed dev world (4.3) lives at `minecraftWorlds/<name>[-<target>]-<worktree8>/` and is
//! seeded once. `targets.<t>.world` set: a copy of that project world (edition transform
//! applied). Absent (the default): a generated flat creative world with cheats on, named
//! after the project. `"world": false`: none.
//! After that only its two pack list files are written (ledger-owned, every deploy); `db/`,
//! `level.dat` and everything else belong to the game and the player. The source world is
//! only read. An existing world folder is written only if it is not a link and carries a
//! valid Beacon ledger (`.beacon-emit.json`); otherwise it is an error and nothing is
//! written. Seeding goes through `<stem>.beacon-seeding`, whose first file is a marker; a
//! stale staging folder is deleted only with that marker (the marker is removed before the
//! rename).
//!
//! Known residuals:
//! - Adding a second `dev` target of an edition renames the first one's folders (the target
//!   joins the name); the old folders are orphaned, not deleted.
//! - Two `dev` targets of one edition share dev UUIDs (derived from release UUID and
//!   worktree only), so deploying both at once gives the game duplicate pack UUIDs.

use super::{
    ArtifactCx, com_mojang, contained, escape, fail, link_ok, pack_name, safe_name, worktree8,
};
use crate::build::{Attempt, trim_rel};
use crate::config::{Artifact, WorldSetting};
use crate::diag::{Code, Diagnostic};
use crate::emit;
use crate::model::{Body, Pack};
use crate::nbt::{self, Tag};
use crate::packlist;
use crate::path::RelPath;
use crate::world::{self, LevelSpec};
use std::fs;
use std::path::Path;

/// The project world as seeded: its files and the transformed `level.dat`.
struct Source {
    files: Vec<(RelPath, RelPath)>,
    level: Vec<u8>,
    name: String,
}

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    let plan = cx.plan;
    let Some(base) = cx
        .out
        .map(|o| o.to_path_buf())
        .or_else(|| com_mojang(plan.edition))
    else {
        a.diags.push(fail(format!(
            "target {:?}: com.mojang for {:?} was not found; set BEACON_COM_MOJANG or pass --out",
            plan.target, plan.edition
        )));
        return;
    };
    // ponytail: the source world is collected and its level.dat transformed on every deploy
    // (in-memory index, small file); only needed when seeding. Cache if watch gets slow.
    let source = match plan.def.as_ref().map(|d| &d.world) {
        Some(WorldSetting::Dir(w)) => match source(cx, a, trim_rel(w)) {
            Some(s) => Some(s),
            None => return,
        },
        Some(WorldSetting::Default) => match generated(cx, a) {
            Some(s) => Some(s),
            None => return,
        },
        _ => None,
    };
    let wt = worktree8(cx.root);
    let dev_targets = cx
        .cfg
        .beacon
        .targets
        .values()
        .filter(|t| t.artifact == Artifact::Dev && t.edition == plan.edition)
        .count();
    let name = if dev_targets > 1 {
        format!("{}-{}", cx.name, safe_name(&plan.target))
    } else {
        cx.name.clone()
    };
    for ((o, p), files) in &cx.packs {
        let folder = if !cx.multi || cx.primary.as_ref() == Some(o) {
            format!("{name}-{wt} {}", pack_name(*p))
        } else {
            format!("{name}-{o}-{wt} {}", pack_name(*p))
        };
        let parent = match p {
            Pack::Behavior => "development_behavior_packs",
            Pack::Resource => "development_resource_packs",
        };
        cx.emit_dir(&base, base.join(parent).join(folder), files, a);
    }
    if let Some(src) = source {
        deploy_world(cx, a, &base, &format!("{name}-{wt}"), &src);
    }
}

/// Reads the project world `dir` through the VFS: `WorldNotFound` without a `level.dat`.
fn source(cx: &ArtifactCx, a: &mut Attempt, dir: &str) -> Option<Source> {
    let (plan, def) = (cx.plan, cx.plan.def.as_ref()?);
    let not_found = |msg: String| Diagnostic::new(Code::WorldNotFound, msg);
    let Ok(rel) = RelPath::parse(dir) else {
        a.diags.push(not_found(format!(
            "target {:?}: bad world folder {dir:?}",
            plan.target
        )));
        return None;
    };
    let files = world::collect(cx.vfs, &rel);
    if !files.iter().any(|(r, _)| r.as_str() == "level.dat") {
        a.diags.push(not_found(format!(
            "world folder {dir:?} does not exist or has no level.dat"
        )));
        return None;
    }
    let name = def.world_name.as_deref().unwrap_or(&cx.cfg.name);
    let level = world::read_level_dat(cx.vfs, &rel).and_then(|bytes| {
        world::transform_level_dat(
            &bytes,
            &LevelSpec {
                edition: plan.edition,
                target_version: &plan.target_version,
                name,
                experiments: &def.experiments,
                overrides: &def.level_dat,
            },
        )
    });
    match level {
        Ok(level) => Some(Source {
            files,
            level,
            name: name.to_owned(),
        }),
        Err(e) => {
            a.diags.push(Diagnostic::new(
                Code::LevelDatInvalid,
                format!("{dir}/level.dat: {e}"),
            ));
            None
        }
    }
}

/// The default world: no files but a generated `level.dat` (flat, creative, cheats on).
/// Fixed values only, so the output is deterministic.
fn generated(cx: &ArtifactCx, a: &mut Attempt) -> Option<Source> {
    let (plan, def) = (cx.plan, cx.plan.def.as_ref()?);
    let name = def.world_name.as_deref().unwrap_or(&cx.cfg.name);
    let v = world::version_parts(&plan.target_version).map(|n| Tag::Int(n as i32));
    let ver = |tail: [i32; 2]| {
        Tag::List(
            nbt::INT,
            v.iter().cloned().chain(tail.map(Tag::Int)).collect(),
        )
    };
    let layers = r#"{"biome_id":1,"block_layers":[{"block_name":"minecraft:bedrock","count":1},{"block_name":"minecraft:dirt","count":2},{"block_name":"minecraft:grass_block","count":1}],"encoding_version":6,"structure_options":null,"world_version":"version.post_1_18"}"#;
    let c = |v: Vec<(&str, Tag)>| {
        Tag::Compound(
            v.into_iter()
                .map(|(k, t)| (k.as_bytes().to_vec(), t))
                .collect(),
        )
    };
    let b = Tag::Byte;
    let root = c(vec![
        ("StorageVersion", Tag::Int(10)),
        ("NetworkVersion", Tag::Int(766)),
        ("LevelName", Tag::string(name)),
        ("GameType", Tag::Int(1)),
        ("Difficulty", Tag::Int(1)),
        ("Generator", Tag::Int(2)),
        ("RandomSeed", Tag::Long(0)),
        ("SpawnX", Tag::Int(0)),
        ("SpawnY", Tag::Int(4)),
        ("SpawnZ", Tag::Int(0)),
        ("LastPlayed", Tag::Long(0)),
        ("Time", Tag::Long(0)),
        ("currentTick", Tag::Long(0)),
        ("SizeOnDisk", Tag::Long(0)),
        ("ForceGameType", b(0)),
        ("commandsEnabled", b(1)),
        ("hasBeenLoadedInCreative", b(1)),
        ("educationFeaturesEnabled", b(0)),
        ("eduOffer", Tag::Int(0)),
        ("spawnMobs", b(1)),
        ("pvp", b(1)),
        ("lightningLevel", Tag::Float(0.0)),
        ("rainLevel", Tag::Float(0.0)),
        ("lightningTime", Tag::Int(100_000)),
        ("rainTime", Tag::Int(100_000)),
        ("FlatWorldLayers", Tag::string(layers)),
        ("InventoryVersion", Tag::string(&plan.target_version)),
        ("baseGameVersion", Tag::string("*")),
        ("MinimumCompatibleClientVersion", ver([0, 0])),
        ("lastOpenedWithVersion", ver([0, 0])),
        (
            "abilities",
            c(vec![
                ("flySpeed", Tag::Float(0.05)),
                ("walkSpeed", Tag::Float(0.1)),
                ("mayfly", b(0)),
                ("flying", b(0)),
                ("instabuild", b(0)),
                ("invulnerable", b(0)),
            ]),
        ),
        (
            "experiments",
            c(vec![
                ("experiments_ever_used", b(0)),
                ("saved_with_toggled_experiments", b(0)),
            ]),
        ),
        ("world_policies", c(vec![])),
    ]);
    let base = nbt::write_level_dat(10, &root);
    match world::transform_level_dat(
        &base,
        &LevelSpec {
            edition: plan.edition,
            target_version: &plan.target_version,
            name,
            experiments: &def.experiments,
            overrides: &def.level_dat,
        },
    ) {
        Ok(level) => Some(Source {
            files: Vec::new(),
            level,
            name: name.to_owned(),
        }),
        Err(e) => {
            a.diags.push(Diagnostic::new(
                Code::LevelDatInvalid,
                format!("target {:?}: generated level.dat: {e}", plan.target),
            ));
            None
        }
    }
}

/// Seeds `minecraftWorlds/<stem>` if it does not exist, then writes its two pack lists.
fn deploy_world(cx: &ArtifactCx, a: &mut Attempt, base: &Path, stem: &str, src: &Source) {
    let dir = base.join("minecraftWorlds").join(stem);
    if !contained(base, &dir) {
        a.diags.push(escape(base, &dir));
        return;
    }
    if !link_ok(cx.root, &dir, cx.trusted, &mut a.diags) {
        return;
    }
    let refs = packlist::refs(
        cx.cfg,
        Some(cx.lock),
        cx.vfs,
        &cx.plan.outputs,
        cx.plan.edition,
        Some(cx.root),
        &|o, p| cx.packs.contains_key(&(o.clone(), p)),
    );
    let (bp, rp) = packlist::lists(&refs);
    let rel = |s: &str| RelPath::parse(s).expect("static path");
    let (bp, rp) = (Body::Json(bp), Body::Json(rp));
    let lists = [
        (rel("world_behavior_packs.json"), &bp),
        (rel("world_resource_packs.json"), &rp),
    ];
    match fs::symlink_metadata(&dir) {
        Err(_) => {
            let tmp = dir.with_file_name(format!("{stem}.beacon-seeding"));
            if let Err(e) = seed(cx, a, &tmp, &dir, src, &lists) {
                a.diags
                    .push(fail(format!("seeding world {}: {e}", dir.display())));
                return;
            }
        }
        Ok(m) => {
            // an existing folder is the player's world: only ours (it has our ledger) and
            // never through a link
            let ours = !m.file_type().is_symlink()
                && fs::read(dir.join(emit::LEDGER_FILE))
                    .is_ok_and(|b| serde_json::from_slice::<emit::Ledger>(&b).is_ok());
            if !ours {
                a.diags.push(fail(format!(
                    "{} exists but is not a world Beacon seeded (link, or no valid {}); left untouched",
                    dir.display(),
                    emit::LEDGER_FILE
                )));
                return;
            }
        }
    }
    cx.emit_dir(base, dir, &lists, a);
}

/// First file of a staging folder: proves it is ours (see [`seed`]).
const STAGING_MARKER: &str = ".beacon-staging";

/// Builds the world in `tmp` (the pack lists first, through the ledger, so they are the only
/// files it owns), then renames it to `dir`: a crash never leaves a half-seeded world.
fn seed(
    cx: &ArtifactCx,
    a: &mut Attempt,
    tmp: &Path,
    dir: &Path,
    src: &Source,
    lists: &[(RelPath, &Body)],
) -> std::io::Result<()> {
    if let Ok(m) = fs::symlink_metadata(tmp) {
        // only a staging folder of ours (a real dir with the marker) is cleaned up
        if m.file_type().is_symlink() || !m.is_dir() || !tmp.join(STAGING_MARKER).is_file() {
            return Err(std::io::Error::other(format!(
                "{} exists and is not a Beacon staging folder; left untouched",
                tmp.display()
            )));
        }
        fs::remove_dir_all(tmp)?; // a crashed seeding of ours
    }
    let res = stage(cx, a, tmp, dir, src, lists);
    if res.is_err() {
        let _ = fs::remove_dir_all(tmp); // ours: we just created it with the marker
    }
    res
}

fn stage(
    cx: &ArtifactCx,
    a: &mut Attempt,
    tmp: &Path,
    dir: &Path,
    src: &Source,
    lists: &[(RelPath, &Body)],
) -> std::io::Result<()> {
    if let Some(p) = tmp.parent() {
        fs::create_dir_all(p)?;
    }
    fs::create_dir(tmp)?;
    fs::write(tmp.join(STAGING_MARKER), b"")?;
    let (_, d) = emit::emit(tmp, lists, cx.vfs);
    let failed = d.iter().any(|d| d.code == Code::EmitFailed);
    // the marker makes the folder non-empty: that is not a missing ledger
    a.diags
        .extend(d.into_iter().filter(|d| d.code != Code::LedgerMissing));
    if failed {
        return Err(std::io::Error::other("cannot write the pack lists"));
    }
    let at = |rel: &str| rel.split('/').fold(tmp.to_owned(), |p, s| p.join(s));
    for (rel, from) in &src.files {
        if matches!(
            rel.as_str(),
            "level.dat"
                | "levelname.txt"
                | "world_behavior_packs.json"
                | "world_resource_packs.json"
                | emit::LEDGER_FILE
                | STAGING_MARKER
        ) {
            continue;
        }
        let to = at(rel.as_str());
        if let Some(p) = to.parent() {
            fs::create_dir_all(p)?;
        }
        cx.vfs.stream(from, &mut emit::create_fresh(&to)?)?;
    }
    fs::write(at("level.dat"), &src.level)?;
    fs::write(at("levelname.txt"), &src.name)?;
    fs::remove_file(at(STAGING_MARKER))?; // staged: no longer needs the marker
    fs::rename(tmp, dir)
}
