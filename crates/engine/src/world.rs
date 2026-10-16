//! Worlds (5.9): collecting a world folder through the VFS, the `level.dat` edition
//! transforms and overrides, and the pack list files. Pure functions; the source world is
//! only ever read.

use crate::lock::PackRef;
use crate::model::Edition;
use crate::nbt::{self, Tag};
use crate::path::{RelPath, Vfs};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Left out of every packaged world.
const EXCLUDED: [&str; 2] = ["level.dat_old", "db/LOCK"];

/// Every file below `world` (a VFS folder) as (path relative to the world, VFS path),
/// without the excluded files. Sorted.
pub fn collect(vfs: &dyn Vfs, world: &RelPath) -> Vec<(RelPath, RelPath)> {
    let mut out = Vec::new();
    let mut stack = vec![(world.clone(), String::new())];
    while let Some((dir, rel)) = stack.pop() {
        for name in vfs.list(Some(&dir)) {
            let (Ok(p), rel) = (dir.join(&name), format!("{rel}{name}")) else {
                continue;
            };
            if vfs.list(Some(&p)).is_empty() {
                if !EXCLUDED.contains(&rel.as_str())
                    && let Ok(r) = RelPath::parse(&rel)
                {
                    out.push((r, p));
                }
            } else {
                stack.push((p, format!("{rel}/")));
            }
        }
    }
    out.sort();
    out
}

/// What a target changes in `level.dat`.
pub struct LevelSpec<'a> {
    pub edition: Edition,
    /// `targetVersion`, `1.21.50`.
    pub target_version: &'a str,
    /// `LevelName`.
    pub name: &'a str,
    /// Education only: `experiments.<x> = 1`.
    pub experiments: &'a [String],
    /// Dotted path -> value; applied last, so it wins over the edition table.
    pub overrides: &'a BTreeMap<String, Value>,
}

/// `1.21.50` -> `[1, 21, 50]` (missing or non-numeric parts are 0).
pub fn version_parts(v: &str) -> [u32; 3] {
    let mut p = v.split('.').map(|s| s.parse().unwrap_or(0));
    std::array::from_fn(|_| p.next().unwrap_or(0))
}

/// Edition table (5.9), `MinimumCompatibleClientVersion`, `LevelName`, then the overrides.
/// Existing tags keep their type; missing ones are created (table: as documented below,
/// overrides: from the JSON value, see [`from_json`]). Idempotent. The storage version is kept
/// and the header length recomputed by the writer.
pub fn transform_level_dat(data: &[u8], spec: &LevelSpec) -> Result<Vec<u8>, String> {
    let (version, mut root) = nbt::parse_level_dat(data)?;
    let edu = spec.edition == Edition::Education;
    let on = i64::from(edu);
    // new tags: educationFeaturesEnabled/experiments are Bytes, eduOffer an Int
    assign(&mut root, "educationFeaturesEnabled", json!(on), nbt::BYTE)?;
    if !edu {
        assign(&mut root, "eduOffer", json!(0), nbt::INT)?;
        for k in [
            "cameras",
            "experiments_ever_used",
            "saved_with_toggled_experiments",
        ] {
            assign(&mut root, &format!("experiments.{k}"), json!(0), nbt::BYTE)?;
        }
    } else if !spec.experiments.is_empty() {
        for k in spec
            .experiments
            .iter()
            .map(String::as_str)
            .chain(["experiments_ever_used", "saved_with_toggled_experiments"])
        {
            assign(&mut root, &format!("experiments.{k}"), json!(1), nbt::BYTE)?;
        }
    }
    let min = version_parts(spec.target_version);
    let min = [min[0] as i32, min[1] as i32, min[2] as i32, 0, 0];
    nbt::set(
        &mut root,
        "MinimumCompatibleClientVersion",
        Tag::List(nbt::INT, min.map(Tag::Int).to_vec()),
    )?;
    nbt::set(&mut root, "LevelName", Tag::string(spec.name))?;
    for (path, v) in spec.overrides {
        match nbt::get(&root, path) {
            Some(_) => assign(&mut root, path, v.clone(), 0)?,
            None => nbt::set(&mut root, path, from_json(v)?)?,
        }
    }
    Ok(nbt::write_level_dat(version, &root))
}

/// Sets `path` to the number/bool `v`: as the existing tag's type, else as `new_type`
/// (0 = infer from the JSON value).
fn assign(root: &mut Tag, path: &str, v: Value, new_type: u8) -> Result<(), String> {
    match nbt::get_mut(root, path) {
        Some(t) => {
            *t = coerce(t, &v).map_err(|e| format!("{path}: {e}"))?;
            Ok(())
        }
        None if new_type == 0 => nbt::set(root, path, from_json(&v)?),
        None => nbt::set(root, path, coerce(&empty(new_type), &v)?),
    }
}

fn empty(id: u8) -> Tag {
    match id {
        nbt::BYTE => Tag::Byte(0),
        _ => Tag::Int(0),
    }
}

/// A new tag from an override value: bool -> Byte, integer -> Int (Long when it does not
/// fit), other number -> Float, string -> String. Anything else is an error.
pub fn from_json(v: &Value) -> Result<Tag, String> {
    match v {
        Value::Bool(b) => Ok(Tag::Byte(*b as i8)),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Ok(i32::try_from(i).map_or(Tag::Long(i), Tag::Int)),
            None => Ok(Tag::Float(n.as_f64().unwrap_or(0.0) as f32)),
        },
        Value::String(s) => Ok(Tag::string(s)),
        _ => Err(format!("cannot create an NBT tag from {v}")),
    }
}

/// `v` as a tag of the same type as `like`.
fn coerce(like: &Tag, v: &Value) -> Result<Tag, String> {
    let int = match v {
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Number(n) => n.as_i64(),
        _ => None,
    };
    let bad = || format!("{v} does not fit a tag of type {}", like.id());
    Ok(match like {
        Tag::Byte(_) => Tag::Byte(int.and_then(|i| i.try_into().ok()).ok_or_else(bad)?),
        Tag::Short(_) => Tag::Short(int.and_then(|i| i.try_into().ok()).ok_or_else(bad)?),
        Tag::Int(_) => Tag::Int(int.and_then(|i| i.try_into().ok()).ok_or_else(bad)?),
        Tag::Long(_) => Tag::Long(int.ok_or_else(bad)?),
        Tag::Float(_) => Tag::Float(v.as_f64().ok_or_else(bad)? as f32),
        Tag::Double(_) => Tag::Double(v.as_f64().ok_or_else(bad)?),
        Tag::String(_) => Tag::string(v.as_str().ok_or_else(bad)?),
        _ => {
            return Err(format!(
                "overriding a tag of type {} is not supported",
                like.id()
            ));
        }
    })
}

/// `world_behavior_packs.json` / `world_resource_packs.json` bodies from the ordered pack refs
/// (4.3: output order). Static outputs are appended by the caller once they have refs (B1).
pub fn pack_lists(behavior: &[PackRef], resource: &[PackRef]) -> (Value, Value) {
    let list = |p: &[PackRef]| {
        Value::Array(
            p.iter()
                .map(|r| json!({"pack_id": r.uuid, "version": r.version}))
                .collect(),
        )
    };
    (list(behavior), list(resource))
}

/// The world's `level.dat` bytes.
pub fn read_level_dat(vfs: &dyn Vfs, world: &RelPath) -> Result<Vec<u8>, String> {
    let p = world.join("level.dat").map_err(|e| e.to_string())?;
    vfs.read(&p).map_err(|e| format!("{p}: {e}"))
}
