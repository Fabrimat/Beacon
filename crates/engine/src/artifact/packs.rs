//! The packs build (no `--target`): the primary output projected to `packs.*`, the others to
//! `.beacon/out/<id>/BP|RP`; `--out <dir>` = `<dir>/<id>/BP|RP`.

use super::{ArtifactCx, pack_name};
use crate::build::{Attempt, trim_rel};
use crate::config::STATE_DIR;
use crate::model::Pack;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    for ((o, p), files) in &cx.packs {
        let projected = match p {
            Pack::Behavior => &cx.cfg.packs.behavior_pack,
            Pack::Resource => &cx.cfg.packs.resource_pack,
        };
        let state_out = cx.root.join(STATE_DIR).join("out");
        let (base, dir) = match (cx.out, projected) {
            (Some(out), _) => (out.to_path_buf(), out.join(o.as_str()).join(pack_name(*p))),
            (None, Some(rel)) if cx.primary.as_ref() == Some(o) => {
                (cx.root.to_path_buf(), cx.root.join(trim_rel(rel)))
            }
            _ => (
                state_out.clone(),
                state_out.join(o.as_str()).join(pack_name(*p)),
            ),
        };
        cx.emit_dir(&base, dir, files, a);
    }
}
