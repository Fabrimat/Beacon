//! `dev`: `development_*_packs/<name>-<worktree8> BP|RP` in the edition's com.mojang
//! (`BEACON_COM_MOJANG`, else detected; `--out` replaces it).

use super::{ArtifactCx, com_mojang, fail, pack_name, worktree8};
use crate::build::Attempt;
use crate::model::Pack;

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
    let wt = worktree8(cx.root);
    let name = &cx.name;
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
}
