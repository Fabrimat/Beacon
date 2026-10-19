//! E1: the symbol table (5.7) and `rename`, on `fixtures/rename` and small inline texts.

use beacon_engine::config::ProjectConfig;
use beacon_engine::diag::{Code, Diagnostic, Fix, Severity};
use beacon_engine::model::{Body, ContentItem, Dest, Merge, Pack};
use beacon_engine::path::{RelPath, Vfs};
use beacon_engine::route::{self, Routed};
use beacon_engine::statics;
use beacon_engine::symbols::{
    self, FileClass, Occurrence, Role, SYMBOLS_JSON_SCHEMA, SymbolKind, SymbolTable,
};
use beacon_engine::vfs::Index;
use serde_json::json;
use std::path::{Path, PathBuf};

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rename")
}

fn load(root: &Path) -> (ProjectConfig, Routed, Index) {
    let cfg = route::load(root).unwrap();
    let (idx, d) = Index::build(root, &[], None);
    assert!(d.is_empty(), "{d:?}");
    let mut r = route::route(&cfg, &idx);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let (st, d) = statics::items(&cfg, &idx);
    assert!(d.is_empty(), "{d:?}");
    r.items.extend(st);
    (cfg, r, idx)
}

fn table_with(root: &Path, items: &[ContentItem]) -> (SymbolTable, Vec<Diagnostic>) {
    let (cfg, r, idx) = load(root);
    SymbolTable::build(&cfg, &r, items, &idx, false)
}

fn table() -> SymbolTable {
    let (t, d) = table_with(&fixture(), &[]);
    assert!(d.is_empty(), "{d:?}");
    t
}

/// The text each occurrence's range covers.
fn slices<'a>(text: &'a str, occ: &[Occurrence]) -> Vec<(&'a str, Option<SymbolKind>, Role)> {
    occ.iter()
        .map(|o| {
            let r = o.range.unwrap();
            (&text[r.start..r.end], o.kind, o.role)
        })
        .collect()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(fixture().join(rel)).unwrap()
}

// ------------------------------------------------------------------ scan

#[test]
fn classify_by_extension() {
    assert_eq!(symbols::classify(&rp("a/b.entity.json")), FileClass::Json);
    assert_eq!(symbols::classify(&rp("a/en_US.lang")), FileClass::Lang);
    assert_eq!(
        symbols::classify(&rp("a/f.mcfunction")),
        FileClass::McFunction
    );
    assert_eq!(symbols::classify(&rp("a/x.gen.ts")), FileClass::Script);
    assert_eq!(symbols::classify(&rp("a/x.mjs")), FileClass::Script);
    assert_eq!(symbols::classify(&rp("a/x.d.ts")), FileClass::Other);
    assert_eq!(symbols::classify(&rp("a/x.png")), FileClass::Other);
}

#[test]
fn scan_entity_json_is_a_definition_and_comments_are_skipped() {
    let path = "src/vehicles/entities/car.entity.json";
    let text = read(path);
    let occ = symbols::scan(&rp(path), Some(&rp("BP/entities/car.json")), &text);
    let def: Vec<_> = occ.iter().filter(|o| o.role == Role::Definition).collect();
    assert_eq!(def.len(), 1, "{occ:?}");
    assert_eq!(
        (def[0].name.as_str(), def[0].kind),
        ("demo:car", Some(SymbolKind::Entity))
    );
    // component groups and events are untyped, rename-only references
    let names: Vec<_> = occ
        .iter()
        .filter(|o| o.role == Role::Reference)
        .map(|o| (o.name.as_str(), o.kind, o.typed))
        .collect();
    assert_eq!(
        names,
        [
            ("demo:car_parked", None, false),
            ("demo:park", None, false),
            ("demo:car_parked", None, false)
        ]
    );
    // nothing from `minecraft:` keys, nothing from the comments
    assert!(
        slices(&text, &occ)
            .iter()
            .all(|(s, ..)| s.starts_with("demo:"))
    );
}

#[test]
fn scan_client_entity_and_spawn_rule_are_typed_references() {
    for (path, dest) in [
        (
            "src/vehicles/entities/car.client_entity.json",
            "RP/entity/car.json",
        ),
        (
            "src/vehicles/entities/car.spawn_rule.json",
            "BP/spawn_rules/car.json",
        ),
    ] {
        let text = read(path);
        let occ = symbols::scan(&rp(path), Some(&rp(dest)), &text);
        assert!(occ.iter().all(|o| o.role == Role::Reference), "{occ:?}");
        let typed: Vec<_> = occ.iter().filter(|o| o.typed).collect();
        assert_eq!(typed.len(), 1, "{path}: {occ:?}");
        assert_eq!(typed[0].kind, Some(SymbolKind::Entity));
        assert_eq!(typed[0].name, "demo:car");
    }
    // the Molang string: `q.is_name_any('demo:car')`
    let path = "src/vehicles/entities/car.client_entity.json";
    let text = read(path);
    let occ = symbols::scan(&rp(path), Some(&rp("RP/entity/car.json")), &text);
    let molang = occ.iter().find(|o| !o.typed).unwrap();
    let r = molang.range.unwrap();
    assert_eq!(&text[r.start - 1..r.end + 1], "'demo:car'");
}

#[test]
fn scan_lang_keys_define_keys_and_reference_ids_with_utf8_offsets() {
    let path = "src/vehicles/texts/en_US.lang";
    let text = read(path);
    assert!(!text.is_ascii());
    let occ = symbols::scan(&rp(path), Some(&rp("RP/texts/en_US.lang")), &text);
    assert_eq!(
        slices(&text, &occ),
        [
            (
                "entity.demo:car.name",
                Some(SymbolKind::LangKey),
                Role::Definition
            ),
            ("demo:car", Some(SymbolKind::Entity), Role::Reference),
            (
                "item.spawn_egg.entity.demo:car.name",
                Some(SymbolKind::LangKey),
                Role::Definition
            ),
            ("demo:car", Some(SymbolKind::Entity), Role::Reference),
        ]
    );
    // lang-key references resolve by kind but never fire SymbolUnresolved
    assert!(occ.iter().all(|o| !o.typed));
    let text = "tile.demo:lamp.name=Lamp\nitem.demo:lamp=Lamp\nitem.demo:x.name=X\n";
    let occ = symbols::scan(&rp("t/en_US.lang"), Some(&rp("RP/texts/en_US.lang")), text);
    let kinds: Vec<_> = occ
        .iter()
        .filter(|o| o.role == Role::Reference)
        .map(|o| (o.name.as_str(), o.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("demo:lamp", Some(SymbolKind::Block)),
            ("demo:lamp", Some(SymbolKind::Item)),
            ("demo:x", Some(SymbolKind::Item))
        ]
    );
}

#[test]
fn scan_mcfunction_commands() {
    let path = "src/world/functions/spawn.mcfunction";
    let text = read(path);
    let occ = symbols::scan(
        &rp(path),
        Some(&rp("BP/functions/world/spawn.mcfunction")),
        &text,
    );
    let got: Vec<_> = occ
        .iter()
        .map(|o| (o.name.as_str(), o.kind, o.role, o.typed, o.range.is_some()))
        .collect();
    assert_eq!(
        got,
        [
            // the file itself defines the function (no text range: renaming it is a file move)
            (
                "world/spawn",
                Some(SymbolKind::Function),
                Role::Definition,
                false,
                false
            ),
            (
                "demo:car",
                Some(SymbolKind::Entity),
                Role::Reference,
                true,
                true
            ),
            (
                "demo:car",
                Some(SymbolKind::Entity),
                Role::Reference,
                true,
                true
            ),
            (
                "demo:horn",
                Some(SymbolKind::Item),
                Role::Reference,
                true,
                true
            ),
        ]
    );
    let text = "# summon demo:nope\nfunction world/spawn\ntag @e[type=!demo:car] add x\n";
    let occ = symbols::scan(&rp("f.mcfunction"), None, text);
    assert_eq!(
        slices(text, &occ),
        [
            ("world/spawn", Some(SymbolKind::Function), Role::Reference),
            ("demo:car", Some(SymbolKind::Entity), Role::Reference),
        ]
    );
}

#[test]
fn scan_script_literals_and_template_heads() {
    let path = "src/vehicles/scripts/lib/ids.ts";
    let text = read(path);
    let occ = symbols::scan(&rp(path), None, &text);
    let got: Vec<_> = occ
        .iter()
        .map(|o| (o.name.as_str(), o.local, o.typed))
        .collect();
    // `${ns}:car` is recorded by its local name; the table resolves it with the namespace
    assert_eq!(
        got,
        [
            ("demo:car", false, false),
            ("car", true, false),
            ("demo:car_key", false, false)
        ]
    );
    assert_eq!(slices(&text, &occ)[1].0, "car");

    let text = "// \"demo:in_comment\"\nconst a = `${ns}:${m.id}`; /* 'demo:c' */\nconst b = `${namespace}:lamp_${i}`;\nconst c = 'x\\'demo:q\\'';\n";
    let occ = symbols::scan(&rp("s.ts"), None, text);
    let got: Vec<_> = occ.iter().map(|o| (o.name.as_str(), o.local)).collect();
    // computed names (`${ns}:${x}`, `${ns}:lamp_${i}`) are not literals: nothing recorded
    assert_eq!(got, [("demo:q", false)]);
}

#[test]
fn scan_never_panics_on_broken_text() {
    for t in [
        "{\"minecraft:entity\": {\"description\": {\"identifier\": \"demo:x",
        "\"unterminated `${",
        "/* never closed",
        "é\u{0}\"demo:a\"]]]}}}",
        "",
    ] {
        for p in ["a.json", "a.ts", "a.lang", "a.mcfunction"] {
            let _ = symbols::scan(&rp(p), Some(&rp("BP/functions/a.mcfunction")), t);
        }
    }
}

// ------------------------------------------------------------------ table

#[test]
fn fixture_table_definitions_and_references() {
    let t = table();
    let defs = t.definitions(SymbolKind::Entity, "demo:car");
    assert_eq!(defs.len(), 1);
    assert_eq!(
        defs[0].path.as_str(),
        "src/vehicles/entities/car.entity.json"
    );
    let refs: Vec<_> = t
        .references(SymbolKind::Entity, "demo:car")
        .iter()
        .map(|l| l.path.as_str())
        .collect();
    assert_eq!(
        refs,
        [
            "src/vehicles/entities/car.client_entity.json",
            "src/vehicles/entities/car.client_entity.json",
            "src/vehicles/entities/car.spawn_rule.json",
            "src/vehicles/scripts/lib/ids.ts",
            "src/vehicles/scripts/lib/ids.ts",
            "src/vehicles/spawner.gen.ts",
            "src/vehicles/texts/en_US.lang",
            "src/vehicles/texts/en_US.lang",
            // the template: its loop body maps to the one source line
            "src/world/functions/convoy.mcfunction",
            "src/world/functions/convoy.mcfunction",
            "src/world/functions/convoy.mcfunction",
            "src/world/functions/spawn.mcfunction",
            "src/world/functions/spawn.mcfunction",
        ]
    );
    // block + item under one name
    assert_eq!(t.definitions(SymbolKind::Block, "demo:lamp").len(), 1);
    assert_eq!(t.definitions(SymbolKind::Item, "demo:lamp").len(), 1);
    // static outputs are indexed, never editable
    let horn = t.references(SymbolKind::Item, "demo:horn");
    let vendor = horn
        .iter()
        .find(|l| l.path.as_str().starts_with("vendor/"))
        .unwrap();
    assert!(!vendor.editable);
    assert_eq!(vendor.output, Some("vendor"));
    // the template head resolved with the project namespace
    let p = rp("src/vehicles/scripts/lib/ids.ts");
    let text = read("src/vehicles/scripts/lib/ids.ts");
    let at = text.find("}:car`").unwrap() + 3;
    let hit = t.at(&p, at).unwrap();
    assert_eq!((hit.occ.name.as_str(), hit.occ.local), ("demo:car", true));
    assert!(t.at(&p, 0).is_none());
}

#[test]
fn find_ranks_exact_prefix_substring_and_is_schema_1() {
    let t = table();
    let names: Vec<_> = t
        .find("demo:car", &[], 10)
        .into_iter()
        .map(|e| (e.kind, e.name))
        .collect();
    // functions and lang keys containing it come after the exact match
    assert_eq!(names[0], (SymbolKind::Entity, "demo:car".to_owned()));
    assert!(names.contains(&(SymbolKind::LangKey, "entity.demo:car.name".to_owned())));
    let lamp = t.find("lamp", &[SymbolKind::Block], 10);
    assert_eq!(lamp.len(), 1);
    assert_eq!(lamp[0].definitions[0].line, Some(4));
    let car = &t.find("demo:car", &[SymbolKind::Entity], 1)[0];
    assert_eq!(car.references, 13);
    assert_eq!(
        (car.definitions[0].line, car.definitions[0].column),
        (Some(6), Some(22))
    );
    let all = symbols::SymbolsJson {
        schema: SYMBOLS_JSON_SCHEMA,
        symbols: t.find("", &[], usize::MAX),
    };
    let a = serde_json::to_string(&all).unwrap();
    let b = serde_json::to_string(&symbols::SymbolsJson {
        schema: 1,
        symbols: table().find("", &[], usize::MAX),
    })
    .unwrap();
    assert_eq!(a, b);
    assert!(a.starts_with("{\"schema\":1,"));
}

// ------------------------------------------------------------------ rename

fn apply(root: &Path, fixes: &[Fix]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in fixes {
        if out.last().is_none_or(|(p, _)| p != f.path.as_str()) {
            let text = std::fs::read_to_string(root.join(f.path.as_str())).unwrap();
            out.push((f.path.as_str().to_owned(), text));
        }
    }
    for (p, text) in &mut out {
        for f in fixes.iter().filter(|f| f.path.as_str() == p).rev() {
            text.replace_range(f.start..f.end, &f.replacement);
        }
    }
    out
}

#[test]
fn rename_car_matches_the_expected_tree() {
    let t = table();
    let fixes = symbols::rename(&t, None, "demo:car", "demo:truck").unwrap();
    // sorted, non-overlapping
    for w in fixes.windows(2) {
        assert!(
            (&w[0].path, w[0].end) <= (&w[1].path, w[1].start),
            "{:?}",
            &w[..2]
        );
    }
    let expected = fixture().with_file_name("rename.expected");
    for (p, text) in apply(&fixture(), &fixes) {
        let want = std::fs::read_to_string(expected.join(&p)).unwrap();
        assert_eq!(text, want, "{p}");
    }
    // `demo:car_key` and `demo:car_parked` are other names
    assert!(fixes.iter().all(|f| f.end - f.start <= "demo:car".len()));
    assert_eq!(fixes.len(), 14);
}

#[test]
fn rename_multi_kind_renames_block_and_item_together() {
    let t = table();
    let fixes = symbols::rename(&t, None, "demo:lamp", "demo:light").unwrap();
    let got: Vec<_> = apply(&fixture(), &fixes)
        .into_iter()
        .map(|(p, t)| (p, t.matches("demo:light").count()))
        .collect();
    assert_eq!(
        got,
        [
            ("src/deco/blocks/lamp.block.json".to_owned(), 1),
            ("src/deco/items/lamp.item.json".to_owned(), 2),
            ("src/deco/texts/en_US.lang".to_owned(), 2),
        ]
    );
    // `--kind block` cannot tell whose the untyped `"block": "demo:lamp"` is
    let e = symbols::rename(&t, Some(SymbolKind::Block), "demo:lamp", "demo:light").unwrap_err();
    assert_eq!(e.code, Code::RenameBlocked);
    assert!(e.message.contains("ambiguous"), "{}", e.message);
    // `--kind` with a single defining kind is not ambiguous
    let f = symbols::rename(&t, Some(SymbolKind::Entity), "demo:car", "demo:truck").unwrap();
    assert_eq!(f.len(), 14);
}

#[test]
fn rename_blocked_cases() {
    let t = table();
    let blocked = |kind, from: &str, to: &str| {
        let e = symbols::rename(&t, kind, from, to).unwrap_err();
        assert_eq!(e.code, Code::RenameBlocked, "{from} -> {to}");
        assert_eq!(e.severity, Severity::Error);
        e.message
    };
    // a reference inside a static output
    assert!(
        blocked(None, "demo:horn", "demo:trumpet")
            .contains("vendor/BP/functions/vendor/horn.mcfunction")
    );
    // the new id is already defined (under any kind)
    assert!(blocked(None, "demo:car", "demo:lamp").contains("already defined"));
    for bad in [
        "Demo:truck",
        "truck",
        "demo:",
        ":truck",
        "demo:tr-uck",
        "demo:a:b",
    ] {
        assert!(blocked(None, "demo:car", bad).contains("invalid"), "{bad}");
    }
    assert!(blocked(None, "minecraft:pig", "demo:pig").contains("minecraft:"));
    assert!(blocked(None, "demo:nothing", "demo:else").contains("not defined"));
    // the template head `${ns}:car` cannot move to another namespace
    assert!(blocked(None, "demo:car", "other:truck").contains("src/vehicles/scripts/lib/ids.ts"));
}

fn gen_item(origin: &str, id: &str) -> ContentItem {
    ContentItem {
        dest: Dest {
            output: "main".into(),
            pack: Pack::Behavior,
            path: rp("items/x.json"),
        },
        body: Body::Json(json!({ "format_version": "1.21.0", "minecraft:item": {
            "description": { "identifier": id }, "components": {} } })),
        merge: Merge::Error,
        origin: rp(origin),
        module: rp("src/vehicles"),
    }
}

#[test]
fn generator_definitions_rename_only_through_one_literal() {
    let (t, _) = table_with(
        &fixture(),
        &[gen_item("src/vehicles/spawner.gen.ts", "demo:car_key")],
    );
    let d = t.definitions(SymbolKind::Item, "demo:car_key");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].path.as_str(), "src/vehicles/spawner.gen.ts");
    assert!(d[0].occ.range.is_some() && d[0].editable);
    let fixes = symbols::rename(&t, None, "demo:car_key", "demo:fob").unwrap();
    // one edit in the generator (the literal is both definition and reference), one in ids.ts
    let paths: Vec<_> = fixes.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "src/vehicles/scripts/lib/ids.ts",
            "src/vehicles/spawner.gen.ts"
        ]
    );

    // an id computed by the generator: no literal, so rename names the generator
    let (t, _) = table_with(
        &fixture(),
        &[gen_item("src/vehicles/spawner.gen.ts", "demo:wheel")],
    );
    let d = t.definitions(SymbolKind::Item, "demo:wheel");
    assert_eq!(d.len(), 1);
    assert!(d[0].occ.range.is_none() && !d[0].editable);
    let e = symbols::rename(&t, None, "demo:wheel", "demo:tire").unwrap_err();
    assert!(
        e.message.contains("src/vehicles/spawner.gen.ts"),
        "{}",
        e.message
    );
}

// ------------------------------------------------------------------ diagnostics

fn tmp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let cfg = json!({
        "type": "minecraftBedrock", "name": "t", "targetVersion": "1.26.50", "namespace": "demo",
        "packs": { "behaviorPack": "./packs/BP", "resourcePack": "./packs/RP" },
        "beacon": { "sources": ["./src"], "outputs": [
            { "id": "a", "include": ["src/a/**"] },
            { "id": "b", "include": ["src/b/**"] }
        ] }
    });
    std::fs::write(d.path().join("config.json"), cfg.to_string()).unwrap();
    for (p, t) in files {
        let p = d.path().join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, t).unwrap();
    }
    d
}

const ENTITY: &str = r#"{"minecraft:entity":{"description":{"identifier":"demo:x"}}}"#;

#[test]
fn diagnostics_unresolved_duplicate_cross_output() {
    let p = tmp_project(&[
        ("src/a/entities/x.entity.json", ENTITY),
        ("src/b/entities/x.entity.json", ENTITY),
        (
            "src/b/functions/f.mcfunction",
            "summon demo:ghost\nsummon minecraft:pig\nsummon demo:x\nsay demo:untyped\n",
        ),
        ("src/a/texts/en_US.lang", "entity.demo:nobody.name=Nobody\n"),
    ]);
    let (cfg, r, idx) = load(p.path());
    let (_, d) = SymbolTable::build(&cfg, &r, &[], &idx, false);
    let codes: Vec<_> = d.iter().map(|d| (d.code, d.severity)).collect();
    assert_eq!(
        codes,
        [
            (Code::SymbolDuplicate, Severity::Error),
            (Code::SymbolUnresolved, Severity::Warning)
        ],
        "{d:?}"
    );
    assert!(d[1].message.contains("demo:ghost"));
    let s = d[1].span.as_ref().unwrap();
    assert_eq!(
        (s.path.as_str(), s.line, s.column),
        ("src/b/functions/f.mcfunction", Some(1), Some(8))
    );

    // restricted with generators present: one note instead of the warnings
    let p = tmp_project(&[
        ("src/a/g.gen.ts", "export default generate(() => []);\n"),
        ("src/a/functions/f.mcfunction", "summon demo:ghost\n"),
    ]);
    let (cfg, r, idx) = load(p.path());
    let (_, d) = SymbolTable::build(&cfg, &r, &[], &idx, true);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].code, d[0].severity),
        (Code::SymbolUnresolved, Severity::Note)
    );

    // cross output: `a` uses an id only `b` defines, without `dependsOn`
    let p = tmp_project(&[
        ("src/b/entities/x.entity.json", ENTITY),
        ("src/a/functions/f.mcfunction", "summon demo:x\n"),
    ]);
    let (cfg, r, idx) = load(p.path());
    let (_, d) = SymbolTable::build(&cfg, &r, &[], &idx, false);
    let codes: Vec<_> = d.iter().map(|d| d.code).collect();
    assert_eq!(codes, [Code::SymbolCrossOutput], "{d:?}");
}

#[test]
fn update_rescans_only_changed_files_and_overlay_rescans_one() {
    let p = tmp_project(&[
        ("src/a/entities/x.entity.json", ENTITY),
        ("src/a/functions/f.mcfunction", "summon demo:x\n"),
    ]);
    let (cfg, r, idx) = load(p.path());
    let (mut t, d) = SymbolTable::build(&cfg, &r, &[], &idx, false);
    assert!(d.is_empty(), "{d:?}");
    std::fs::write(
        p.path().join("src/a/functions/f.mcfunction"),
        "summon demo:y\n",
    )
    .unwrap();
    let (cfg, r, idx) = load(p.path());
    t.update(&cfg, &r, &[], &idx);
    assert_eq!(t.references(SymbolKind::Entity, "demo:x").len(), 0);
    assert_eq!(t.references(SymbolKind::Entity, "demo:y").len(), 1);
    let d = t.diagnostics(&cfg, false);
    assert_eq!(d.len(), 1);
    // an unsaved buffer
    t.overlay(
        &rp("src/a/functions/f.mcfunction"),
        "summon demo:x\nsummon demo:x\n",
    );
    assert_eq!(t.references(SymbolKind::Entity, "demo:x").len(), 2);
    // removed files drop out
    std::fs::remove_file(p.path().join("src/a/functions/f.mcfunction")).unwrap();
    let (cfg, r, idx) = load(p.path());
    t.update(&cfg, &r, &[], &idx);
    assert_eq!(t.references(SymbolKind::Entity, "demo:x").len(), 0);
    let _ = idx.exists(&rp("config.json"));
}

// ------------------------------------------------------------------ session

struct OneItem;

impl beacon_engine::host::GeneratorHost for OneItem {
    fn run(
        &self,
        g: &beacon_engine::model::Generator,
        _: &beacon_engine::model::GenCtx,
        _: &dyn Vfs,
    ) -> beacon_engine::host::GenResult {
        beacon_engine::host::GenResult {
            items: vec![gen_item(g.path.as_str(), "demo:car_key")],
            inputs: Default::default(),
            diagnostics: vec![],
        }
    }
    fn version(&self) -> String {
        "e1 one item".into()
    }
}

#[test]
fn session_check_fills_the_table_with_generator_definitions_when_trusted() {
    use beacon_engine::build::{BuildOptions, Session};
    for trusted in [false, true] {
        let opts = BuildOptions {
            trusted,
            ..Default::default()
        };
        let mut s = Session::new(&fixture(), opts, Box::new(OneItem), None);
        assert!(s.symbols().is_none());
        let r = s.check();
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == Code::SymbolUnresolved),
            "{:?}",
            r.diagnostics
        );
        let t = s.symbols().unwrap();
        assert_eq!(t.definitions(SymbolKind::Entity, "demo:car").len(), 1);
        let keys = t.definitions(SymbolKind::Item, "demo:car_key").len();
        assert_eq!(keys, usize::from(trusted), "trusted: {trusted}");
        assert!(s.symbols_mut().is_some());
    }
}

// ------------------------------------------------------------------ overlays and apply

#[test]
fn overlay_persists_across_update_until_closed() {
    let p = tmp_project(&[
        ("src/a/entities/x.entity.json", ENTITY),
        ("src/a/functions/f.mcfunction", "summon demo:x\n"),
    ]);
    let f = rp("src/a/functions/f.mcfunction");
    let (cfg, r, idx) = load(p.path());
    let (mut t, _) = SymbolTable::build(&cfg, &r, &[], &idx, false);
    t.overlay(&f, "summon demo:x\nsummon demo:x\n");
    assert!(t.has_overlay(&f));
    // a watcher-style update rescans changed files, never an open buffer
    std::fs::write(p.path().join("src/a/functions/f.mcfunction"), "# empty\n").unwrap();
    let (cfg, r, idx) = load(p.path());
    t.update(&cfg, &r, &[], &idx);
    assert_eq!(t.references(SymbolKind::Entity, "demo:x").len(), 2);
    t.close_overlay(&f);
    assert!(!t.has_overlay(&f));
    t.update(&cfg, &r, &[], &idx);
    assert_eq!(t.references(SymbolKind::Entity, "demo:x").len(), 0);
}

#[test]
fn apply_writes_all_or_nothing_and_refuses_a_stale_table() {
    let p = tmp_project(&[
        ("src/a/entities/x.entity.json", ENTITY),
        ("src/a/functions/f.mcfunction", "summon demo:x\n"),
    ]);
    let (t, _) = table_with(p.path(), &[]);
    let fixes = symbols::rename(&t, None, "demo:x", "demo:y").unwrap();
    // the file moved under the table: refused, nothing written (not even the first file)
    let f = p.path().join("src/a/functions/f.mcfunction");
    std::fs::write(&f, "summon  demo:x\n").unwrap();
    let e = symbols::apply(p.path(), &t, &fixes).unwrap_err();
    assert_eq!(e.code, Code::RenameBlocked);
    assert!(e.message.contains("changed since"), "{}", e.message);
    assert!(
        std::fs::read_to_string(p.path().join("src/a/entities/x.entity.json"))
            .unwrap()
            .contains("demo:x")
    );
    std::fs::write(&f, "summon demo:x\n").unwrap();
    let plan = symbols::plan(p.path(), &t, &fixes).unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "summon demo:x\n"); // dry run
    let done = symbols::apply(p.path(), &t, &fixes).unwrap();
    assert_eq!(done.len(), 2);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "summon demo:y\n");
    // no temp files left
    let left: Vec<_> = std::fs::read_dir(f.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
}
