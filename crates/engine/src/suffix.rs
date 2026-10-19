//! Contract: convention routing table (5.3 concept 1), versioned. Folder = module; the file
//! suffix picks the pack and folder. Matching is longest suffix first.
//!
//! A file's module-relative path is `<first>/<rest>` (e.g. `functions/game/start.mcfunction`:
//! `first` = `functions`, an organizational folder; `rest` = `game/start.mcfunction`). A file
//! at the module root has no `first`; then `rest` is its name. Layouts:
//! - `Flat`: `<dir>/<name>` (file name only; ids are global, a cross-module collision is a
//!   diagnostic).
//! - `Module`: `<dir>/<module>/<rest>`: kinds referenced by path whose names are only
//!   module-unique (functions: `/function <module>/game/start`, sounds, loot, trades).
//! - `Namespace`: `<dir>/<namespace>/<rest>`: structures (`<namespace>:<name>`).
//!
//! `rename_to_json` replaces the matched suffix with `.json` (`sword.item.json` ->
//! `sword.json`); otherwise the name is kept.
//!
//! Rules not expressible as a plain suffix:
//! - Textures ([`TEXTURE_SUFFIXES`]) route by the **nearest ancestor** folder listed in
//!   [`TEXTURE_FOLDERS`], keeping the path below it: `entities/car/body.png` ->
//!   `RP/textures/entity/car/body.png`. No such ancestor: `Code::UnknownSuffix`.
//! - Any `.ts` / `.js` / `.d.ts` inside a module is code (`scripts/` entries, generator
//!   helpers): never routed and never `UnknownSuffix`. Files with a [`CONTENT_SUFFIXES`]
//!   suffix (`*.gen.ts`, content files such as `*.entity.ts`) are generators.
//! - `*.data.json` are data modules and `module.json` is config: never routed.

use crate::model::{Merge, Pack};

pub const TABLE_VERSION: u32 = 2;

/// Generator files: `*.gen.ts` and content files (one `*.entity.ts` = one entity, 5.3).
pub const CONTENT_SUFFIXES: &[&str] = &[".gen.ts", ".entity.ts", ".item.ts", ".block.ts"];

/// The file is evaluated by the JS host ([`CONTENT_SUFFIXES`]).
pub fn is_generator(name: &str) -> bool {
    CONTENT_SUFFIXES.iter().any(|s| name.ends_with(s))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Flat,
    Module,
    Namespace,
}

pub struct Rule {
    pub suffix: &'static str,
    pub pack: Pack,
    pub dir: &'static str,
    pub layout: Layout,
    pub rename_to_json: bool,
    pub merge: Merge,
}

const fn r(
    suffix: &'static str,
    pack: Pack,
    dir: &'static str,
    layout: Layout,
    rename_to_json: bool,
) -> Rule {
    Rule {
        suffix,
        pack,
        dir,
        layout,
        rename_to_json,
        merge: Merge::Error,
    }
}

use Layout::{Flat, Module, Namespace};
use Pack::{Behavior as BP, Resource as RP};

pub const RULES: &[Rule] = &[
    r(".item.json", BP, "items", Flat, true),
    r(".block.json", BP, "blocks", Flat, true),
    r(".entity.json", BP, "entities", Flat, true),
    r(".loot.json", BP, "loot_tables", Module, true),
    r(".trade.json", BP, "trading", Module, true),
    r(".recipe.json", BP, "recipes", Flat, true),
    r(".spawn_rule.json", BP, "spawn_rules", Flat, true),
    r(".dialogue.json", BP, "dialogue", Flat, true),
    r(".bp_animation.json", BP, "animations", Flat, true),
    r(".bp_ac.json", BP, "animation_controllers", Flat, true),
    r(".client_entity.json", RP, "entity", Flat, true),
    r(".attachable.json", RP, "attachables", Flat, true),
    r(".fog.json", RP, "fogs", Flat, true),
    r(".geo.json", RP, "models/entity", Flat, false),
    r(".animation.json", RP, "animations", Flat, false),
    r(".ac.json", RP, "animation_controllers", Flat, false),
    r(".rc.json", RP, "render_controllers", Flat, false),
    r(".particle.json", RP, "particles", Flat, false),
    r(".mcfunction", BP, "functions", Module, false),
    r(".mcstructure", BP, "structures", Namespace, false),
    r(".ogg", RP, "sounds", Module, false),
    r(".wav", RP, "sounds", Module, false),
    Rule {
        suffix: ".lang",
        pack: RP,
        dir: "texts",
        layout: Flat,
        rename_to_json: false,
        merge: Merge::LangLines,
    },
];

/// Ancestor folder name -> `RP/textures/<dir>/` (nearest ancestor wins, remainder kept).
pub const TEXTURE_FOLDERS: &[(&str, &str)] = &[
    ("items", "items"),
    ("blocks", "blocks"),
    ("entities", "entity"),
    ("entity", "entity"),
    ("particles", "particle"),
    ("models", "models"),
    ("ui", "ui"),
    ("gui", "gui"),
    ("environment", "environment"),
    ("misc", "misc"),
];

/// `.texture_set.json` (PBR) sits beside its png and routes the same way.
pub const TEXTURE_SUFFIXES: &[&str] = &[".png", ".tga", ".jpg", ".jpeg", ".texture_set.json"];

/// Longest matching rule for a file name.
pub fn rule_for(file_name: &str) -> Option<&'static Rule> {
    RULES
        .iter()
        .filter(|r| file_name.ends_with(r.suffix))
        .max_by_key(|r| r.suffix.len())
}

/// Never routed, never `UnknownSuffix` (code, data modules, config, template scope).
pub fn is_unrouted(file_name: &str) -> bool {
    file_name == crate::config::MODULE_FILE
        || file_name.ends_with(".ts")
        || file_name.ends_with(".js")
        || file_name.ends_with(".data.json")
        || file_name == crate::template::SCOPE_FILE
}

/// Convention destination of a file: `module` = module name, `rel` = module-relative path
/// (`/`-separated). Returns pack, pack-relative path and merge policy, or `None` when no rule
/// applies (`UnknownSuffix`, unless [`is_unrouted`]). Shared by routing and by generators
/// that reference assets (e.g. `sound()` needs the emitted path of an `.ogg`).
pub fn route_file(module: &str, namespace: &str, rel: &str) -> Option<(Pack, String, Merge)> {
    let segs: Vec<&str> = rel.split('/').collect();
    let name = *segs.last()?;
    if TEXTURE_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        let (i, dir) = segs[..segs.len() - 1]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, s)| {
                TEXTURE_FOLDERS
                    .iter()
                    .find(|(f, _)| f == s)
                    .map(|(_, d)| (i, *d))
            })?;
        return Some((
            Pack::Resource,
            format!("textures/{dir}/{}", segs[i + 1..].join("/")),
            Merge::Error,
        ));
    }
    let rule = rule_for(name)?;
    let rest = if segs.len() > 1 {
        segs[1..].join("/")
    } else {
        name.to_owned()
    };
    let rename = |s: &str| {
        if rule.rename_to_json {
            format!("{}.json", &s[..s.len() - rule.suffix.len()])
        } else {
            s.to_owned()
        }
    };
    let path = match rule.layout {
        Layout::Flat => format!("{}/{}", rule.dir, rename(name)),
        Layout::Module => format!("{}/{module}/{}", rule.dir, rename(&rest)),
        Layout::Namespace => format!("{}/{namespace}/{}", rule.dir, rename(&rest)),
    };
    Some((rule.pack, path, rule.merge))
}

#[cfg(test)]
mod tests {
    use super::route_file;
    use crate::model::Pack;

    #[test]
    fn routes_by_layout() {
        let p = |rel| route_file("vehicles", "demo", rel).map(|(_, p, _)| p);
        assert_eq!(p("items/sword.item.json").unwrap(), "items/sword.json");
        assert_eq!(p("sword.item.json").unwrap(), "items/sword.json");
        assert_eq!(
            p("functions/game/start.mcfunction").unwrap(),
            "functions/vehicles/game/start.mcfunction"
        );
        assert_eq!(p("sounds/beep.ogg").unwrap(), "sounds/vehicles/beep.ogg");
        assert_eq!(
            p("structures/house.mcstructure").unwrap(),
            "structures/demo/house.mcstructure"
        );
        assert_eq!(
            p("entities/car/body.png").unwrap(),
            "textures/entity/car/body.png"
        );
        assert_eq!(p("items/sword.png").unwrap(), "textures/items/sword.png");
        assert_eq!(p("texts/en_US.lang").unwrap(), "texts/en_US.lang");
        assert_eq!(
            route_file("m", "d", "car.geo.json").unwrap().0,
            Pack::Resource
        );
        assert!(p("loose.png").is_none());
        assert!(p("notes.txt").is_none());
    }

    #[test]
    fn longest_suffix_wins() {
        assert_eq!(
            super::rule_for("x.client_entity.json").unwrap().dir,
            "entity"
        );
        assert_eq!(super::rule_for("x.entity.json").unwrap().dir, "entities");
        assert_eq!(
            super::rule_for("x.bp_ac.json").unwrap().dir,
            "animation_controllers"
        );
        assert!(super::rule_for("x.json").is_none());
    }
}
