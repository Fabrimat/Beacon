//! Content files, `texture` on item/block and data modules, through the real host and
//! (for data modules) the real bundler, driven by an engine `Session`.

use beacon_bundler::Esbuild;
use beacon_engine::build::{BuildOptions, BuildOutcome, Session};
use beacon_engine::diag::Severity;
use beacon_jshost::{Host, Limits};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const CONFIG: &str = r#"{"type":"minecraftBedrock","name":"c2","targetVersion":"1.21.100",
  "namespace":"demo","beacon":{"scriptModules":{"@minecraft/server":"2.10.0"}}}"#;

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    for (p, b) in [("config.json", CONFIG)].iter().chain(files) {
        let f = d.path().join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, b).unwrap();
    }
    d
}

fn build(root: &Path, bundler: Option<Esbuild>) -> BuildOutcome {
    let opts = BuildOptions {
        out: Some(root.join("out")),
        trusted: true,
        ..Default::default()
    };
    let b = bundler.map(|b| Box::new(b) as Box<dyn beacon_engine::host::Bundler>);
    let r = Session::new(root, opts, Box::new(Host::new(Limits::default())), b).build();
    let errs: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errs.is_empty(), "{errs:?}");
    r
}

fn out(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("out/main").join(rel)).unwrap()
}

fn files(root: &Path) -> Vec<String> {
    let mut v = vec![];
    let mut todo = vec![root.join("out/main")];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                todo.push(p);
            } else {
                v.push(
                    p.strip_prefix(root.join("out/main"))
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    v.sort();
    v
}

const CAR: &str = r#"entity("demo:car", {
  server: { components: { "minecraft:health": { value: 20 } } },
  lang: { en_US: "Car" },
})"#;

#[test]
fn content_file_equals_generator_form() {
    let a = project(&[("src/v/car.entity.ts", &format!("export default {CAR};"))]);
    let b = project(&[(
        "src/v/car.gen.ts",
        &format!("export default generate(() => {CAR});"),
    )]);
    let c = project(&[(
        "src/v/car.gen.ts",
        &format!("export default generate(() => [{CAR}]);"),
    )]);
    for p in [&a, &b, &c] {
        build(p.path(), None);
    }
    let names = files(a.path());
    assert!(
        names.contains(&"BP/entities/car.json".to_string()),
        "{names:?}"
    );
    for p in [&b, &c] {
        assert_eq!(files(p.path()), names);
        for n in names
            .iter()
            .filter(|n| !n.ends_with("manifest.json") && !n.ends_with(".beacon-emit.json"))
        {
            assert_eq!(out(p.path(), n), out(a.path(), n), "{n}");
        }
    }
    // no double emit: exactly one lang line
    assert_eq!(
        out(a.path(), "RP/texts/en_US.lang")
            .matches("entity.demo:car.name")
            .count(),
        1
    );
}

#[test]
fn content_file_array_and_item_texture() {
    let p = project(&[
        (
            "src/m/things.item.ts",
            r#"export default [item("demo:gem", { texture: "./items/gem.png" }), item("demo:rod")];"#,
        ),
        (
            "src/m/ore.block.ts",
            r#"export default block("demo:ore", { texture: "./blocks/ore.png" });"#,
        ),
        ("src/m/items/gem.png", "PNG"),
        ("src/m/blocks/ore.png", "PNG"),
    ]);
    let r = build(p.path(), None);
    let names = files(p.path());
    for n in [
        "BP/items/gem.json",
        "BP/items/rod.json",
        "BP/blocks/ore.json",
        "RP/textures/items/gem.png",
    ] {
        assert!(names.contains(&n.to_string()), "{n} in {names:?}");
    }
    let gem: Value = serde_json::from_str(&out(p.path(), "BP/items/gem.json")).unwrap();
    assert!(gem["minecraft:item"].get("texture").is_none(), "{gem}");
    let it: Value = serde_json::from_str(&out(p.path(), "RP/textures/item_texture.json")).unwrap();
    assert_eq!(
        it["texture_data"],
        json!({ "demo:gem": { "textures": "textures/items/gem" } })
    );
    let t: Value =
        serde_json::from_str(&out(p.path(), "RP/textures/terrain_texture.json")).unwrap();
    assert_eq!(
        t["texture_data"],
        json!({ "demo:ore": { "textures": "textures/blocks/ore" } })
    );
    drop(r);
}

fn esbuild() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/esbuild-test");
        std::fs::create_dir_all(&dir).ok()?;
        let mut c = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", "npm"]);
            c
        } else {
            Command::new("npm")
        };
        let ok = c
            .args(["i", "--no-audit", "--no-fund", "--prefix"])
            .arg(&dir)
            .arg("esbuild@0.28.2")
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("SKIP: npm unavailable or esbuild install failed");
        }
        ok.then(|| dir.canonicalize().ok()).flatten()
    })
    .as_deref()
}

#[test]
fn data_module_is_inlined() {
    let Some(inst) = esbuild() else { return };
    let p = project(&[
        (
            "src/m/scripts/main.ts",
            "import trophies from \"../trophies.data.json\";\nconsole.log(trophies.gold);\n",
        ),
        (
            "src/m/trophies.data.json",
            r#"{"gold": "DATA_MODULE_GOLD"}"#,
        ),
    ]);
    build(
        p.path(),
        Esbuild::find(inst, beacon_bundler::Confirm::No).ok(),
    );
    let js = out(p.path(), "BP/scripts/main.js");
    assert!(
        js.contains("DATA_MODULE_GOLD") && !js.contains("import "),
        "{js}"
    );
    assert!(!files(p.path()).iter().any(|n| n.contains("trophies")));
}
