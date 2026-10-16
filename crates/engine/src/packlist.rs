//! `world_behavior_packs.json` / `world_resource_packs.json` (4.3): the packs of a plan in
//! outputs-array order (first = highest priority), filtered to those present.

use crate::config::ProjectConfig;
use crate::diag::{Code, Diagnostic};
use crate::lock::{Lockfile, PackRef};
use crate::model::{Edition, OutputId, Pack};
use crate::path::Vfs;
use crate::{statics, world};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub(crate) type Refs = Vec<(OutputId, Pack, PackRef)>;

/// Pack refs in `outputs` order (BP before RP within an output) for the packs `present`
/// says exist. Static packs: UUID and version from their manifest. Generated packs: lock
/// UUID (the dev UUID when `dev` is the worktree root) and `version_of`; none without `lock`.
pub(crate) fn refs(
    cfg: &ProjectConfig,
    lock: Option<&Lockfile>,
    vfs: &dyn Vfs,
    outputs: &[OutputId],
    edition: Edition,
    dev: Option<&Path>,
    present: &dyn Fn(&OutputId, Pack) -> bool,
) -> Refs {
    let mut out = Vec::new();
    for o in outputs {
        let st = cfg
            .beacon
            .outputs
            .iter()
            .find(|d| &d.id == o)
            .and_then(|d| d.static_packs.as_ref());
        for p in [Pack::Behavior, Pack::Resource] {
            if !present(o, p) {
                continue;
            }
            let r = match st {
                Some(s) => match if p == Pack::Behavior { &s.bp } else { &s.rp } {
                    Some(f) => statics::header(vfs, f)
                        .ok()
                        .map(|(uuid, version)| PackRef { uuid, version }),
                    None => None,
                },
                None => lock.and_then(|l| l.pack_ref(o, p, edition, dev)),
            };
            out.extend(r.map(|r| (o.clone(), p, r)));
        }
    }
    out
}

/// The two list files' bodies (behavior, resource), entries `{pack_id, version}`.
pub(crate) fn lists(refs: &Refs) -> (Value, Value) {
    let pick = |p: Pack| -> Vec<PackRef> {
        refs.iter()
            .filter(|r| r.1 == p)
            .map(|r| r.2.clone())
            .collect()
    };
    world::pack_lists(&pick(Pack::Behavior), &pick(Pack::Resource))
}

/// `DuplicatePackUuid` for every UUID shared by two packs of one plan.
pub(crate) fn duplicates(refs: &Refs) -> Vec<Diagnostic> {
    let mut seen: BTreeMap<&str, (&str, Pack)> = BTreeMap::new();
    let mut out = Vec::new();
    for (o, p, r) in refs {
        if let Some((o0, p0)) = seen.insert(&r.uuid, (o, *p)) {
            out.push(Diagnostic::new(
                Code::DuplicatePackUuid,
                format!(
                    "pack {} of output {o:?} has the same UUID {} as pack {} of output {o0:?}; the game loads only one",
                    crate::artifact::pack_name(*p),
                    r.uuid,
                    crate::artifact::pack_name(p0),
                ),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{EditionLock, OutputLock, PackLock};
    use crate::path::RelPath;

    struct NoFiles;
    impl Vfs for NoFiles {
        fn exists(&self, _: &RelPath) -> bool {
            false
        }
        fn read(&self, _: &RelPath) -> std::io::Result<Vec<u8>> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn hash(&self, _: &RelPath) -> std::io::Result<crate::path::Hash> {
            Err(std::io::ErrorKind::NotFound.into())
        }
        fn list(&self, _: Option<&RelPath>) -> Vec<String> {
            vec![]
        }
        fn stream(&self, _: &RelPath, _: &mut dyn std::io::Write) -> std::io::Result<u64> {
            Err(std::io::ErrorKind::NotFound.into())
        }
    }

    fn lock(ids: &[&str]) -> Lockfile {
        let pl = |h: String| PackLock {
            header: h,
            module: String::new(),
            release: None,
        };
        let mut l = Lockfile::default();
        for id in ids {
            let mut o = OutputLock::default();
            o.editions.insert(
                Edition::Bedrock,
                EditionLock {
                    bp: pl(format!("{id}-bp")),
                    rp: pl(format!("{id}-rp")),
                },
            );
            l.outputs.insert((*id).into(), o);
        }
        l
    }

    /// Array order = stack order: first output = first entry = top of the stack.
    /// MANUAL CHECK NEEDED: that the client treats the first `world_resource_packs.json`
    /// entry as the highest priority is unverified; confirm in game before relying on it.
    #[test]
    fn list_order_follows_outputs_array() {
        let cfg: ProjectConfig = serde_json::from_str(
            r#"{"type":"minecraftBedrock","name":"x","targetVersion":"1.21.100","namespace":"d"}"#,
        )
        .unwrap();
        let outs: Vec<OutputId> = ["zeta", "alpha", "mid"].map(String::from).to_vec();
        let l = lock(&["alpha", "mid", "zeta"]);
        let present = |o: &OutputId, p: Pack| !(o == "mid" && p == Pack::Resource);
        let r = refs(
            &cfg,
            Some(&l),
            &NoFiles,
            &outs,
            Edition::Bedrock,
            None,
            &present,
        );
        let (bp, rp) = lists(&r);
        let ids = |v: &Value| -> Vec<String> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|e| e["pack_id"].as_str().unwrap().into())
                .collect()
        };
        assert_eq!(ids(&bp), ["zeta-bp", "alpha-bp", "mid-bp"]);
        assert_eq!(ids(&rp), ["zeta-rp", "alpha-rp"]);
        assert_eq!(bp[0]["version"], serde_json::json!([1, 0, 0]));
    }
}
