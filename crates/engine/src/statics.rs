//! Static outputs (4.3): prebuilt packs (`outputs[].static`) included as-is, UUIDs and
//! versions kept. Runs right after routing; its items join the routed ones. Every file is a
//! byte-identical passthrough (the pack's own `manifest.json` included, never regenerated).

use crate::build::trim_rel;
use crate::config::{CONFIG_FILE, ProjectConfig};
use crate::diag::{Code, Diagnostic};
use crate::merge::strip_comments;
use crate::model::{Body, ContentItem, Dest, Merge, Pack, Version};
use crate::path::{RelPath, Vfs};

/// Header UUID and version of the pack in project folder `folder`, from its `manifest.json`
/// (UTF-8 BOM and JSONC accepted).
pub fn header(vfs: &dyn Vfs, folder: &str) -> Result<(String, Version), String> {
    let dir = RelPath::parse(trim_rel(folder)).map_err(|e| format!("{folder:?}: {e}"))?;
    let path = dir.join("manifest.json").map_err(|e| e.to_string())?;
    let bytes = vfs
        .read(&path)
        .map_err(|_| format!("{dir} does not exist or has no manifest.json"))?;
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let v: serde_json::Value =
        serde_json::from_slice(&strip_comments(bytes)).map_err(|e| format!("{path}: {e}"))?;
    let h = &v["header"];
    let uuid = h["uuid"].as_str().filter(|u| !u.is_empty());
    let version = h["version"]
        .as_array()
        .filter(|a| a.len() == 3)
        .and_then(|a| {
            let n = |i: usize| a[i].as_u64().and_then(|n| u32::try_from(n).ok());
            Some([n(0)?, n(1)?, n(2)?])
        });
    match (uuid, version) {
        (Some(u), Some(v)) => Ok((u.to_owned(), v)),
        _ => Err(format!(
            "{path}: header.uuid and header.version ([major, minor, patch]) are required"
        )),
    }
}

/// Every file below `dir` as (path relative to `dir`, VFS path), sorted.
fn walk(vfs: &dyn Vfs, dir: &RelPath) -> Vec<(RelPath, RelPath)> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.clone(), String::new())];
    while let Some((d, rel)) = stack.pop() {
        for name in vfs.list(Some(&d)) {
            let (Ok(p), rel) = (d.join(&name), format!("{rel}{name}")) else {
                continue;
            };
            if vfs.list(Some(&p)).is_empty() {
                out.extend(RelPath::parse(&rel).ok().map(|r| (r, p)));
            } else {
                stack.push((p, format!("{rel}/")));
            }
        }
    }
    out.sort();
    out
}

/// Passthrough items (own `manifest.json` included) for every static output's packs, plus
/// `StaticPackInvalid` diagnostics (a pack with a missing folder or invalid manifest
/// contributes nothing).
pub fn items(cfg: &ProjectConfig, vfs: &dyn Vfs) -> (Vec<ContentItem>, Vec<Diagnostic>) {
    let (mut items, mut diags) = (Vec::new(), Vec::new());
    for o in &cfg.beacon.outputs {
        let Some(s) = &o.static_packs else { continue };
        for (pack, folder) in [(Pack::Behavior, &s.bp), (Pack::Resource, &s.rp)] {
            let Some(folder) = folder else { continue };
            if let Err(e) = header(vfs, folder) {
                diags.push(
                    Diagnostic::new(
                        Code::StaticPackInvalid,
                        format!("static output {:?}: {e}", o.id),
                    )
                    .at(
                        RelPath::parse(CONFIG_FILE).expect("const"),
                        None,
                        None,
                    ),
                );
                continue;
            }
            let dir = RelPath::parse(trim_rel(folder)).expect("header parsed it");
            items.extend(walk(vfs, &dir).into_iter().map(|(path, src)| ContentItem {
                dest: Dest {
                    output: o.id.clone(),
                    pack,
                    path,
                },
                body: Body::Passthrough(src.clone()),
                merge: Merge::Error,
                origin: src,
                module: dir.clone(),
            }));
        }
    }
    items.sort_by(|a, b| a.dest.cmp(&b.dest));
    (items, diags)
}
