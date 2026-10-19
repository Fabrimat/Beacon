//! Generator result cache keyed by observed inputs (5.2). In memory per [`crate::build::Session`]
//! and on disk in `<root>/.beacon/cache/<key>.json`.
//!
//! Key = blake3(engine version, `CANONICAL_VERSION`, `TABLE_VERSION`, [`FORMAT_VERSION`],
//! `GeneratorHost::version`, the `Generator` (path, module, output, namespace) and the full
//! `GenCtx`). An entry is reused
//! only if every recorded input still matches the current index: file hashes, directory
//! listings, negatives still absent, defines and env values equal. Uncacheable results and
//! nondeterministic failures (timeout, OOM, internal) are never stored.
//!
//! ponytail: no cross-process locking and no GC. Concurrent writers are safe (temp + rename,
//! last writer wins, entries are self-validating) but the dir grows without bound; add an
//! LRU size cap + a lock file when the shared content-addressed cache (5.2) lands.

use crate::config::STATE_DIR;
use crate::diag::{Code, Diagnostic};
use crate::host::GeneratorHost;
use crate::model::{ContentItem, GenCtx, Generator, ObservedInputs};
use crate::path::{Hash, Vfs};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Bump when the on-disk entry format changes (host behavior is `GeneratorHost::version`).
const FORMAT_VERSION: u32 = 1;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Entry {
    inputs: ObservedInputs,
    items: Vec<ContentItem>,
    diagnostics: Vec<Diagnostic>,
}

pub(crate) struct GenCache {
    dir: PathBuf,
    mem: HashMap<String, Entry>,
}

/// Hash of a directory listing as the JS host records it: blake3 of the sorted names joined
/// by `\n`.
pub(crate) fn dir_hash(names: &[String]) -> Hash {
    *blake3::hash(names.join("\n").as_bytes()).as_bytes()
}

fn key(host: &str, g: &Generator, ctx: &GenCtx) -> String {
    let mut h = blake3::Hasher::new();
    h.update(env!("CARGO_PKG_VERSION").as_bytes());
    h.update(&[0]);
    h.update(host.as_bytes());
    h.update(&[0]);
    for v in [
        crate::canonical::CANONICAL_VERSION,
        crate::suffix::TABLE_VERSION,
        FORMAT_VERSION,
    ] {
        h.update(&v.to_le_bytes());
    }
    h.update(&serde_json::to_vec(g).expect("generator json"));
    h.update(&[0]);
    h.update(&serde_json::to_vec(ctx).expect("ctx json"));
    h.finalize().to_hex().to_string()
}

fn valid(e: &Entry, g: &Generator, ctx: &GenCtx, vfs: &dyn Vfs) -> bool {
    let i = &e.inputs;
    // the generator itself must be an input, whatever the host recorded
    i.files.contains_key(&g.path)
        && i.files.iter().all(|(p, h)| vfs.hash(p).ok() == Some(*h))
        && i.dirs
            .iter()
            .all(|(d, h)| dir_hash(&vfs.list(Some(d))) == *h)
        && i.negatives.iter().all(|p| !vfs.exists(p))
        && i.defines
            .iter()
            .all(|(k, v)| ctx.define.get(k).unwrap_or(&serde_json::Value::Null) == v)
        && i.env
            .iter()
            .all(|(k, v)| std::env::var(k).ok().as_deref() == Some(v))
}

fn storable(e: &Entry) -> bool {
    !e.inputs.uncacheable
        && !e.diagnostics.iter().any(|d| {
            matches!(
                d.code,
                Code::GeneratorTimeout | Code::GeneratorOutOfMemory | Code::Internal
            )
        })
}

impl GenCache {
    pub fn new(root: &Path) -> Self {
        GenCache {
            dir: root.join(STATE_DIR).join("cache"),
            mem: HashMap::new(),
        }
    }

    /// Cached result if still valid, else runs `host` and stores the result when allowed.
    /// `persist: false` (check) never writes the disk cache. Returns (items, diagnostics, ran).
    pub fn run(
        &mut self,
        host: &dyn GeneratorHost,
        g: &Generator,
        ctx: &GenCtx,
        vfs: &dyn Vfs,
        persist: bool,
    ) -> (Vec<ContentItem>, Vec<Diagnostic>, bool) {
        let k = key(&host.version(), g, ctx);
        let file = self.dir.join(format!("{k}.json"));
        match self.mem.get(&k) {
            Some(e) if valid(e, g, ctx, vfs) => {
                return (e.items.clone(), e.diagnostics.clone(), false);
            }
            Some(_) => {}
            // a missing or corrupt file is a miss, never an error
            None => {
                let disk = std::fs::read(&file)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Entry>(&b).ok())
                    .filter(|e| valid(e, g, ctx, vfs));
                if let Some(e) = disk {
                    let r = (e.items.clone(), e.diagnostics.clone(), false);
                    self.mem.insert(k, e);
                    return r;
                }
            }
        }
        let r = host.run(g, ctx, vfs);
        let e = Entry {
            inputs: r.inputs,
            items: r.items,
            diagnostics: r.diagnostics,
        };
        if storable(&e) {
            if persist {
                // best effort: a failed cache write only costs a rerun next time
                let _ = write_atomic(&file, &serde_json::to_vec(&e).expect("entry json"));
            }
            self.mem.insert(k, e.clone());
        } else {
            self.mem.remove(&k);
        }
        (e.items, e.diagnostics, true)
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().expect("cache dir"))?;
    let mut rnd = [0u8; 8];
    getrandom::fill(&mut rnd).map_err(std::io::Error::other)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{:x}.tmp", u64::from_le_bytes(rnd)));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
