//! `marketplace`: the store submission zip (`Content/world_template/...`, optional
//! `Content/skin_pack/...`, `Store Art`, `Marketing Art`). The template is the same assembly
//! as the standalone `mctemplate`; the art folders are copied as-is.

use super::{ArtifactCx, skin, world};
use crate::build::{Attempt, trim_rel};
use crate::config::Artifact;
use crate::diag::{Code, Diagnostic};
use crate::model::Body;
use crate::path::RelPath;

const ART: [&str; 2] = ["Store Art", "Marketing Art"];

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    let Some(def) = cx.plan.def.as_ref() else {
        return;
    };
    let Some(template) = world::assemble(cx, a, Artifact::McTemplate) else {
        return;
    };
    let mut entries: Vec<(RelPath, Body)> = template
        .into_iter()
        .filter_map(|(p, b)| {
            Some((
                RelPath::parse(&format!("Content/world_template/{p}")).ok()?,
                b,
            ))
        })
        .collect();
    if def.skins.is_some() {
        let Some(s) = skin::assemble(cx, a, "Content/skin_pack/") else {
            return;
        };
        entries.extend(s);
    }
    let art = def
        .art
        .as_deref()
        .map(trim_rel)
        .and_then(|d| RelPath::parse(d).ok());
    for folder in ART {
        let files = art
            .as_ref()
            .and_then(|d| d.join(folder).ok())
            .map(|d| crate::world::collect(cx.vfs, &d))
            .unwrap_or_default();
        if files.is_empty() {
            a.diags.push(Diagnostic::new(
                Code::ArtMissing,
                format!(
                    "target {:?}: art folder {folder:?} is missing or empty (set `art`)",
                    cx.plan.target
                ),
            ));
        }
        for (r, p) in files {
            if let Ok(zp) = RelPath::parse(&format!("{folder}/{r}")) {
                entries.push((zp, Body::Passthrough(p)));
            }
        }
    }
    world::publish(cx, a, "zip", &entries);
}
