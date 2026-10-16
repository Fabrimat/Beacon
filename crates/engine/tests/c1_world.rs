//! Worlds: `level.dat` edition transforms, `mcworld` and `mctemplate` artifacts.

mod b1_common;
mod c1_common;
use b1_common::{errors, has, opts, session, write};
use beacon_engine::build::BuildOutcome;
use beacon_engine::diag::Code;
use beacon_engine::model::Edition;
use beacon_engine::nbt::{self, Tag, parse_level_dat};
use beacon_engine::world::{LevelSpec, transform_level_dat};
use c1_common::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

fn spec<'a>(
    edition: Edition,
    version: &'a str,
    experiments: &'a [String],
    overrides: &'a BTreeMap<String, Value>,
) -> LevelSpec<'a> {
    LevelSpec {
        edition,
        target_version: version,
        name: "My World",
        experiments,
        overrides,
    }
}

fn set(t: &mut Tag, p: &str, v: Tag) {
    nbt::set(t, p, v).unwrap();
}

fn transform(data: &[u8], s: &LevelSpec) -> (i32, Tag, Vec<u8>) {
    let out = transform_level_dat(data, s).unwrap();
    let (v, t) = parse_level_dat(&out).unwrap();
    (v, t, out)
}

#[test]
fn bedrock_transform_changes_only_the_table() {
    let orig = fixture_bytes();
    let (_, original) = parse_level_dat(&orig).unwrap();
    let none = BTreeMap::new();
    let (v, t, out) = transform(&orig, &spec(Edition::Bedrock, "1.21.100", &[], &none));
    assert_eq!(v, 10);
    let mut want = original.clone();
    set(&mut want, "educationFeaturesEnabled", Tag::Byte(0));
    set(&mut want, "eduOffer", Tag::Int(0));
    set(&mut want, "experiments.cameras", Tag::Byte(0)); // missing: created as a Byte
    set(&mut want, "experiments.experiments_ever_used", Tag::Byte(0));
    set(
        &mut want,
        "experiments.saved_with_toggled_experiments",
        Tag::Byte(0),
    );
    set(
        &mut want,
        "MinimumCompatibleClientVersion",
        Tag::List(nbt::INT, [1, 21, 100, 0, 0].map(Tag::Int).to_vec()),
    );
    set(&mut want, "LevelName", Tag::string("My World"));
    assert_eq!(t, want);
    assert_ne!(t, original);
    assert_eq!(
        i32::from_le_bytes(out[4..8].try_into().unwrap()) as usize,
        out.len() - 8
    );
    // idempotent
    let (_, _, twice) = transform(&out, &spec(Edition::Bedrock, "1.21.100", &[], &none));
    assert_eq!(twice, out);
}

#[test]
fn education_transform_and_overrides() {
    let orig = fixture_bytes();
    let none = BTreeMap::new();
    let exp = vec!["cameras".to_string()];
    let (_, t, out) = transform(&orig, &spec(Edition::Education, "1.21.50", &exp, &none));
    for (p, v) in [
        ("educationFeaturesEnabled", 1),
        ("experiments.cameras", 1),
        ("experiments.experiments_ever_used", 1),
        ("experiments.saved_with_toggled_experiments", 1),
    ] {
        assert_eq!(nbt::get(&t, p), Some(&Tag::Byte(v)), "{p}");
    }
    assert_eq!(nbt::get(&t, "eduOffer"), Some(&Tag::Int(0)), "untouched");
    let (_, _, again) = transform(&out, &spec(Edition::Education, "1.21.50", &exp, &none));
    assert_eq!(again, out);

    // no experiments: ever_used is left alone
    let (_, t, _) = transform(&orig, &spec(Edition::Education, "1.21.50", &[], &none));
    assert_eq!(nbt::get(&t, "experiments.cameras"), None);
    assert_eq!(
        nbt::get(&t, "experiments.experiments_ever_used"),
        Some(&Tag::Byte(0))
    );

    // overrides win over the table; existing types are kept; missing tags are created
    let ov: BTreeMap<String, Value> = [
        ("educationFeaturesEnabled", json!(0)),
        ("experiments.cameras", json!(false)),
        ("abilities.mayfly", json!(true)),
        ("SpawnX", json!(7)),
        ("abilities.flySpeed", json!(0.5)),
        ("new.flag", json!(true)),
        ("new.count", json!(3)),
        ("new.big", json!(5_000_000_000i64)),
        ("new.text", json!("hi")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    let (_, t, out) = transform(&orig, &spec(Edition::Education, "1.21.50", &exp, &ov));
    let g = |p: &str| nbt::get(&t, p).cloned();
    assert_eq!(g("educationFeaturesEnabled"), Some(Tag::Byte(0)));
    assert_eq!(g("experiments.cameras"), Some(Tag::Byte(0)));
    assert_eq!(g("abilities.mayfly"), Some(Tag::Byte(1)));
    assert_eq!(g("SpawnX"), Some(Tag::Int(7)));
    assert_eq!(g("abilities.flySpeed"), Some(Tag::Float(0.5)));
    assert_eq!(g("new.flag"), Some(Tag::Byte(1)));
    assert_eq!(g("new.count"), Some(Tag::Int(3)));
    assert_eq!(g("new.big"), Some(Tag::Long(5_000_000_000)));
    assert_eq!(g("new.text"), Some(Tag::string("hi")));
    let (_, _, again) = transform(&out, &spec(Edition::Education, "1.21.50", &exp, &ov));
    assert_eq!(again, out);

    // wrong types are errors, never silently retyped
    for (k, v) in [
        ("LevelName", json!(5)),
        ("abilities.mayfly", json!(300)),
        ("abilities", json!(1)),
    ] {
        let ov = BTreeMap::from([(k.to_string(), v)]);
        assert!(
            transform_level_dat(&orig, &spec(Edition::Bedrock, "1.21.50", &[], &ov)).is_err(),
            "{k}"
        );
    }
    assert!(transform_level_dat(b"nope", &spec(Edition::Bedrock, "1.21.50", &[], &none)).is_err());
}

// ---- artifacts -----------------------------------------------------------------------------

const CONFIG: &str = r#"{
  "type": "minecraftBedrock", "name": "c1 world", "targetVersion": "1.21.100", "namespace": "demo",
  "worlds": ["./worlds/*"],
  "beacon": {
    "packMeta": { "name": "C1 Template", "description": "A template" },
    "targets": {
      "world": { "edition": "bedrock", "artifact": "mcworld", "world": "worlds/main" },
      "edu": { "edition": "education", "artifact": "mcworld", "world": "worlds/main",
               "targetVersion": "1.21.50", "experiments": ["cameras"],
               "levelDat": { "abilities.mayfly": 1 }, "worldName": "Edu World" },
      "nopacks": { "edition": "bedrock", "artifact": "mcworld", "world": "worlds/main", "embedPacks": false },
      "template": { "edition": "bedrock", "artifact": "mctemplate", "world": "worlds/main",
                    "template": { "lockTemplateOptions": true, "baseGameVersion": "1.21.0" } },
      "etemplate": { "edition": "education", "artifact": "mctemplate", "world": "worlds/main" },
      "missing": { "edition": "bedrock", "artifact": "mcworld", "world": "worlds/nope" },
      "broken": { "edition": "bedrock", "artifact": "mcworld", "world": "worlds/broken" }
    }
  }
}"#;

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(r, "config.json", CONFIG);
    write(
        r,
        "src/m/items/sword.item.json",
        r#"{"format_version":"1.21.100","minecraft:item":{"description":{"identifier":"demo:sword"},"components":{}}}"#,
    );
    write(r, "src/m/items/sword.png", "PNG");
    write(r, "src/m/texts/en_US.lang", "item.demo:sword=Sword\n");
    let w = r.join("worlds/main");
    std::fs::create_dir_all(w.join("db")).unwrap();
    std::fs::write(w.join("level.dat"), fixture_bytes()).unwrap();
    std::fs::write(w.join("level.dat_old"), b"old").unwrap();
    std::fs::write(w.join("levelname.txt"), "Old Name").unwrap();
    std::fs::write(w.join("world_icon.jpeg"), b"\xff\xd8jpeg").unwrap();
    std::fs::write(w.join("db/000005.ldb"), vec![7u8; 4096]).unwrap();
    std::fs::write(w.join("db/CURRENT"), b"MANIFEST-000004\n").unwrap();
    std::fs::write(w.join("db/LOCK"), b"").unwrap();
    std::fs::create_dir_all(r.join("worlds/broken")).unwrap();
    std::fs::write(
        r.join("worlds/broken/level.dat"),
        b"\x0a\0\0\0\x05\0\0\0junk!",
    )
    .unwrap();
    d
}

fn build(p: &Path, target: &str, out: &str) -> BuildOutcome {
    let mut o = opts(p, Some(target));
    o.out = Some(p.join(out));
    session(o, p, &Arc::new(AtomicUsize::new(0))).build()
}

type Zip = BTreeMap<String, (Vec<u8>, zip::CompressionMethod)>;

fn read_zip(path: &str) -> Zip {
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..z.len())
        .map(|i| {
            let mut f = z.by_index(i).unwrap();
            let mut b = Vec::new();
            f.read_to_end(&mut b).unwrap();
            (f.name().unwrap().into_owned(), (b, f.compression()))
        })
        .collect()
}

fn artifact(r: &BuildOutcome) -> Zip {
    assert!(errors(r).is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.report.artifacts.len(), 1, "{:?}", r.diagnostics);
    read_zip(&r.report.artifacts[0])
}

fn snapshot(dir: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    let mut m = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let t = e.metadata().unwrap().modified().unwrap();
                m.insert(p.display().to_string(), (std::fs::read(&p).unwrap(), t));
            }
        }
    }
    m
}

fn json_of(z: &Zip, name: &str) -> Value {
    serde_json::from_slice(&z[name].0).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn lock(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p.join("beacon.lock.json")).unwrap()).unwrap()
}

#[test]
fn mcworld_is_deterministic_and_clean() {
    let p = project();
    let before = snapshot(&p.path().join("worlds"));
    let (a, b) = (build(p.path(), "world", "a"), build(p.path(), "world", "b"));
    let (za, zb) = (artifact(&a), artifact(&b));
    assert_eq!(
        std::fs::read(&a.report.artifacts[0]).unwrap(),
        std::fs::read(&b.report.artifacts[0]).unwrap()
    );
    assert!(a.report.artifacts[0].ends_with("c1 world-world.mcworld"));
    assert!(za.keys().eq(zb.keys()));
    for k in [
        "level.dat",
        "levelname.txt",
        "world_icon.jpeg",
        "db/000005.ldb",
        "db/CURRENT",
        "world_behavior_packs.json",
        "world_resource_packs.json",
        "behavior_packs/c1 world BP/manifest.json",
        "resource_packs/c1 world RP/manifest.json",
    ] {
        assert!(za.contains_key(k), "missing {k}: {:?}", za.keys());
    }
    assert!(!za.contains_key("level.dat_old") && !za.contains_key("db/LOCK"));
    assert_eq!(za["db/000005.ldb"].1, zip::CompressionMethod::Stored);
    assert_eq!(za["level.dat"].0[..4], 10i32.to_le_bytes());
    assert_eq!(za["levelname.txt"].0, b"c1 world");
    let (_, t) = parse_level_dat(&za["level.dat"].0).unwrap();
    assert_eq!(nbt::get(&t, "LevelName"), Some(&Tag::string("c1 world")));
    assert_eq!(
        nbt::get(&t, "educationFeaturesEnabled"),
        Some(&Tag::Byte(0))
    );

    // pack lists come from the lock (this edition's UUIDs)
    let l = lock(p.path());
    let ed = &l["outputs"]["main"]["editions"]["bedrock"];
    assert_eq!(
        json_of(&za, "world_behavior_packs.json"),
        json!([{"pack_id": ed["BP"]["header"], "version": [1, 0, 0]}])
    );
    assert_eq!(
        json_of(&za, "world_resource_packs.json"),
        json!([{"pack_id": ed["RP"]["header"], "version": [1, 0, 0]}])
    );
    assert_eq!(
        json_of(&za, "behavior_packs/c1 world BP/manifest.json")["header"]["uuid"],
        ed["BP"]["header"]
    );
    // the source world was not touched
    assert_eq!(snapshot(&p.path().join("worlds")), before);
}

#[test]
fn bedrock_and_education_worlds_differ_only_where_expected() {
    let p = project();
    let (b, e) = (build(p.path(), "world", "o"), build(p.path(), "edu", "o"));
    let (zb, ze) = (artifact(&b), artifact(&e));
    assert!(e.report.artifacts[0].ends_with("c1 world-edu.mcworld"));
    assert!(zb.keys().eq(ze.keys()));
    let differing: Vec<&String> = zb.keys().filter(|k| zb[*k].0 != ze[*k].0).collect();
    assert_eq!(
        differing,
        [
            "behavior_packs/c1 world BP/manifest.json",
            "level.dat",
            "levelname.txt", // worldName
            "resource_packs/c1 world RP/manifest.json",
            "world_behavior_packs.json",
            "world_resource_packs.json",
        ]
    );
    assert_eq!(ze["levelname.txt"].0, b"Edu World");
    let (_, t) = parse_level_dat(&ze["level.dat"].0).unwrap();
    assert_eq!(
        nbt::get(&t, "educationFeaturesEnabled"),
        Some(&Tag::Byte(1))
    );
    assert_eq!(nbt::get(&t, "experiments.cameras"), Some(&Tag::Byte(1)));
    assert_eq!(nbt::get(&t, "abilities.mayfly"), Some(&Tag::Byte(1)));
    assert_eq!(nbt::get(&t, "LevelName"), Some(&Tag::string("Edu World")));
    let ed = &lock(p.path())["outputs"]["main"]["editions"]["education"];
    assert_eq!(
        json_of(&ze, "world_behavior_packs.json")[0]["pack_id"],
        ed["BP"]["header"]
    );
    // packs were generated for the target's min_engine_version
    let m = json_of(&ze, "behavior_packs/c1 world BP/manifest.json");
    assert_eq!(m["header"]["min_engine_version"], json!([1, 21, 50]));
}

#[test]
fn embed_packs_false_warns_and_keeps_the_lists() {
    let p = project();
    let r = build(p.path(), "nopacks", "o");
    assert!(has(&r, Code::EmbedPacksMissing));
    let z = artifact(&r);
    assert!(
        z.keys()
            .all(|k| !k.starts_with("behavior_packs/") && !k.starts_with("resource_packs/"))
    );
    assert_eq!(
        json_of(&z, "world_behavior_packs.json")
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn mctemplate_manifest_and_texts() {
    let p = project();
    let r = build(p.path(), "template", "o");
    let z = artifact(&r);
    assert!(r.report.artifacts[0].ends_with("c1 world-template.mctemplate"));
    let ids = &lock(p.path())["worlds"]["worlds/main"]["editions"]["bedrock"];
    assert_eq!(
        json_of(&z, "manifest.json"),
        json!({
            "format_version": 2,
            "header": {
                "name": "pack.name",
                "description": "pack.description",
                "uuid": ids["header"],
                "version": [1, 0, 0],
                "base_game_version": [1, 21, 0],
                "lock_template_options": true,
                "allow_random_seed": false,
            },
            "modules": [{"type": "world_template", "uuid": ids["module"], "version": [1, 0, 0]}],
        })
    );
    assert_eq!(
        z["texts/en_US.lang"].0,
        b"pack.name=C1 Template\npack.description=A template\n"
    );
    assert_eq!(json_of(&z, "texts/languages.json"), json!(["en_US"]));
    assert_eq!(z["world_icon.jpeg"].0, b"\xff\xd8jpeg");
    assert!(z.contains_key("db/000005.ldb") && z.contains_key("level.dat"));
    assert!(z.contains_key("behavior_packs/c1 world BP/manifest.json"));

    // targetVersion is the default base game version; the edition has its own UUID
    let e = artifact(&build(p.path(), "etemplate", "o"));
    let m = json_of(&e, "manifest.json");
    assert_eq!(m["header"]["base_game_version"], json!([1, 21, 100]));
    let ids = &lock(p.path())["worlds"]["worlds/main"]["editions"]["education"];
    assert_eq!(m["header"]["uuid"], ids["header"]);
}

#[test]
fn missing_world_and_bad_level_dat_produce_no_artifact() {
    let p = project();
    let r = build(p.path(), "missing", "o");
    assert!(has(&r, Code::WorldNotFound));
    assert!(r.report.artifacts.is_empty());
    let r = build(p.path(), "broken", "o");
    assert!(has(&r, Code::LevelDatInvalid), "{:?}", r.diagnostics);
    assert!(r.report.artifacts.is_empty());
    assert!(
        !p.path().join("o").exists()
            || std::fs::read_dir(p.path().join("o"))
                .unwrap()
                .next()
                .is_none()
    );
    // a world without level.dat
    std::fs::remove_file(p.path().join("worlds/main/level.dat")).unwrap();
    assert!(has(&build(p.path(), "world", "o"), Code::WorldNotFound));
}
