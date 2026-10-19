//! `skinpack`: the target's `skins` folder as a skin pack (`.mcpack`, 5.9). `manifest.json`
//! and `texts/languages.json` are generated; `skins.json`, textures and other files pass
//! through; `texts/en_US.lang` gets the base keys it lacks. Reads through the VFS only.

use super::{ArtifactCx, fail, has_errors, world};
use crate::build::{Attempt, trim_rel};
use crate::config::CONFIG_FILE;
use crate::diag::{Code, Diagnostic};
use crate::model::Body;
use crate::path::RelPath;
use crate::release;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(crate) fn write(cx: &ArtifactCx, a: &mut Attempt) {
    if let Some(entries) = assemble(cx, a, "") {
        world::publish(cx, a, "mcpack", &entries);
    }
}

/// The skin pack's zip entries under `prefix` (`""`, or `Content/skin_pack/` in a store zip),
/// `None` after a diagnostic.
pub(crate) fn assemble(
    cx: &ArtifactCx,
    a: &mut Attempt,
    prefix: &str,
) -> Option<Vec<(RelPath, Body)>> {
    if has_errors(&a.diags) {
        return None;
    }
    let plan = cx.plan;
    let invalid = |msg: String| Diagnostic::new(Code::StaticPackInvalid, msg);
    let Some(dir) = plan.def.as_ref()?.skins.as_deref().map(trim_rel) else {
        a.diags.push(
            Diagnostic::new(
                Code::ConfigField,
                format!("target {:?}: no `skins` folder", plan.target),
            )
            .at(RelPath::parse(CONFIG_FILE).expect("const"), None, None),
        );
        return None;
    };
    let src = RelPath::parse(dir).ok()?;
    let files = crate::world::collect(cx.vfs, &src);
    let has = |n: &str| files.iter().any(|(r, _)| r.as_str() == n);
    let skins: Option<Value> = has("skins.json")
        .then(|| cx.vfs.read(&src.join("skins.json").ok()?).ok())
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok());
    let Some(skins) = skins else {
        a.diags.push(invalid(format!(
            "{dir}/skins.json is missing or not valid JSON"
        )));
        return None;
    };
    let Some(ln) = skins["localization_name"]
        .as_str()
        .filter(|s| !s.is_empty())
    else {
        a.diags.push(invalid(format!(
            "{dir}/skins.json: `localization_name` is missing"
        )));
        return None;
    };
    for s in skins["skins"].as_array().into_iter().flatten() {
        if let Some(t) = s["texture"].as_str()
            && !has(t)
        {
            a.diags.push(invalid(format!(
                "{dir}/skins.json: texture {t:?} does not exist"
            )));
        }
    }
    let Some(ids) = cx
        .lock
        .skins
        .get(dir)
        .and_then(|w| w.editions.get(&plan.edition))
    else {
        a.diags.push(fail(format!(
            "no lock entry for skins {dir:?} ({:?})",
            plan.edition
        )));
        return None;
    };
    if has_errors(&a.diags) {
        return None;
    }

    let meta = cx.cfg.beacon.pack_meta.as_ref();
    let name = meta.and_then(|m| m.name.as_deref()).unwrap_or(&cx.cfg.name);
    let desc = meta.and_then(|m| m.description.as_deref()).unwrap_or("");
    let clean = |s: &str| s.replace(['\r', '\n'], " ");
    // placeholder: stamped with the settled version below (the hash blanks it)
    let version = [1, 0, 0];
    let mut entries: Vec<(String, Body)> = vec![(
        "manifest.json".into(),
        Body::Json(json!({
            "format_version": 2,
            "header": {
                "name": "pack.name",
                "description": "pack.description",
                "uuid": ids.header,
                "version": version,
            },
            "modules": [{"type": "skin_pack", "uuid": ids.module, "version": version}],
        })),
    )];
    // passthrough, except the derived files; en_US.lang is read to add the base keys
    let mut langs: BTreeSet<String> = BTreeSet::from(["en_US".into()]);
    let mut en = String::new();
    for (r, p) in &files {
        let r = r.as_str();
        if let Some(l) = r
            .strip_prefix("texts/")
            .and_then(|l| l.strip_suffix(".lang"))
            && !l.contains('/')
        {
            langs.insert(l.into());
        }
        match r {
            "manifest.json" | "texts/languages.json" => {}
            "texts/en_US.lang" => {
                en = String::from_utf8_lossy(&cx.vfs.read(p).unwrap_or_default()).into_owned();
            }
            _ => entries.push((r.into(), Body::Passthrough(p.clone()))),
        }
    }
    if !en.is_empty() && !en.ends_with('\n') {
        en.push('\n');
    }
    for (k, v) in [
        (format!("skinpack.{ln}"), name),
        ("pack.name".into(), name),
        ("pack.description".into(), desc),
    ] {
        if !en.lines().any(|l| l.split('=').next() == Some(&k)) {
            en += &format!("{k}={}\n", clean(v));
        }
    }
    entries.push(("texts/en_US.lang".into(), Body::Text(en)));
    entries.push(("texts/languages.json".into(), Body::Json(json!(langs))));
    let hash = release::content_hash(entries.iter().map(|(p, b)| (p.as_str(), b)), cx.vfs);
    let key = format!("skins:{dir}");
    let v = release::resolve(&mut a.rel, cx.lock, &mut a.diags, &key, plan.edition, hash)?;
    release::stamp(&mut entries[0].1, v);

    let mut out = Vec::new();
    for (p, b) in entries {
        match RelPath::parse(&format!("{prefix}{p}")) {
            Ok(p) => out.push((p, b)),
            Err(e) => a.diags.push(fail(e.to_string())),
        }
    }
    Some(out)
}
