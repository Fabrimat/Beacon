//! `GeneratorHost` tests over an in-memory `Vfs`.

use beacon_engine::diag::Code;
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{Body, Edition, GenCtx, Generator, Merge, Pack};
use beacon_engine::path::{Hash, RelPath, Vfs};
use beacon_jshost::{Host, Limits};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, ErrorKind};
use std::time::Duration;

#[derive(Default)]
struct Fake {
    files: BTreeMap<String, Vec<u8>>,
    fail: BTreeMap<String, ErrorKind>,
}

impl Fake {
    fn with(mut self, p: &str, s: &str) -> Self {
        self.files.insert(p.into(), s.as_bytes().to_vec());
        self
    }
    fn check(&self, p: &RelPath) -> io::Result<()> {
        match self.fail.get(p.as_str()) {
            Some(k) => Err(io::Error::new(*k, "injected")),
            None => Ok(()),
        }
    }
}

impl Vfs for Fake {
    fn exists(&self, p: &RelPath) -> bool {
        self.files.contains_key(p.as_str())
    }
    fn read(&self, p: &RelPath) -> io::Result<Vec<u8>> {
        self.check(p)?;
        self.files
            .get(p.as_str())
            .cloned()
            .ok_or_else(|| ErrorKind::NotFound.into())
    }
    fn hash(&self, p: &RelPath) -> io::Result<Hash> {
        Ok(*blake3::hash(&self.read(p)?).as_bytes())
    }
    fn list(&self, dir: Option<&RelPath>) -> Vec<String> {
        let prefix = dir.map_or(String::new(), |d| format!("{d}/"));
        let names: BTreeSet<_> = self
            .files
            .keys()
            .filter_map(|k| k.strip_prefix(&prefix))
            .map(|r| r.split('/').next().unwrap().to_string())
            .collect();
        names.into_iter().collect()
    }
    fn stream(&self, p: &RelPath, out: &mut dyn io::Write) -> io::Result<u64> {
        let b = self.read(p)?;
        out.write_all(&b)?;
        Ok(b.len() as u64)
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

fn gen_for(path: &str) -> Generator {
    let module = path.rsplit_once('/').unwrap().0;
    Generator {
        path: rp(path),
        module: rp(module),
        output: "main".into(),
        namespace: "demo".into(),
    }
}

fn ctx_for(g: &Generator) -> GenCtx {
    GenCtx {
        module: g.module.file_name().into(),
        namespace: "demo".into(),
        edition: Edition::Bedrock,
        target: "1.21.0".into(),
        define: BTreeMap::from([("debug".into(), json!(true))]),
    }
}

fn run_with(host: &Host, vfs: &Fake, path: &str) -> GenResult {
    let g = gen_for(path);
    host.run(&g, &ctx_for(&g), vfs)
}

fn run(vfs: &Fake, path: &str) -> GenResult {
    run_with(&Host::new(Limits::default()), vfs, path)
}

const SOUNDS: &str = "export default generate(({ assets, ns }) =>\n  assets(\"./*.ogg\").map(f => sound(`${ns}:${f.name}`, { file: f.path })));";
const MOBS: &str = "import mobs from \"./mobs.data.json\";\n\nexport default generate(({ ns }) =>\n  mobs.map(m => entity(`${ns}:${m.id}`, {\n    server: { components: { \"minecraft:health\": { value: m.health } } },\n  })));";

#[test]
fn host_is_send_sync() {
    fn ok<T: Send + Sync>() {}
    ok::<Host>();
}

#[test]
fn corpus_sounds() {
    let vfs = Fake::default()
        .with("src/s/sounds.gen.ts", SOUNDS)
        .with("src/s/idol_1.ogg", "x")
        .with("src/s/sword_0.ogg", "x")
        .with("src/s/deep/no.ogg", "x");
    let r = run(&vfs, "src/s/sounds.gen.ts");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.items.len(), 2);
    for (i, n) in ["idol_1", "sword_0"].iter().enumerate() {
        let it = &r.items[i];
        assert_eq!(it.dest.pack, Pack::Resource);
        assert_eq!(it.dest.path.as_str(), "sounds/sound_definitions.json");
        assert_eq!(it.merge, Merge::JsonDeep);
        assert_eq!(it.origin, rp("src/s/sounds.gen.ts"));
        let Body::Json(v) = &it.body else { panic!() };
        assert_eq!(
            *v,
            json!({"format_version": "1.20.20", "sound_definitions": {
                format!("demo:{n}"): {"category": "neutral", "sounds": [format!("sounds/s/{n}")]}}})
        );
    }
    // assets() recorded the listed dir and the generator itself.
    assert!(r.inputs.dirs.contains_key(&rp("src/s")));
    assert!(r.inputs.files.contains_key(&rp("src/s/sounds.gen.ts")));
    assert!(!r.inputs.uncacheable);
}

#[test]
fn assets_relative_to_generator_folder() {
    let g = |glob: &str| {
        format!(
            "generate(({{ assets }}) => assets(\"{glob}\").map(f => sound(`demo:${{f.name}}`, {{ file: f.path }})));"
        )
    };
    let vfs = Fake::default()
        .with("src/s/top.ogg", "x")
        .with("src/s/sounds/a.ogg", "x")
        .with("src/s/sounds/sub/deep.ogg", "x")
        .with("src/s/sounds/sounds.gen.ts", &g("./*.ogg"))
        .with("src/s/sounds/up.gen.ts", &g("../*.ogg"))
        .with("src/s/sounds/rec.gen.ts", &g("./**/*.ogg"))
        .with("src/s/sounds/esc.gen.ts", &g("../../*.ogg"));
    let run_in_module = |p: &str| {
        let mut g = gen_for(p);
        g.module = rp("src/s");
        Host::new(Limits::default()).run(&g, &ctx_for(&g), &vfs)
    };
    let names = |p: &str| {
        let r = run_in_module(p);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        r.items.len()
    };
    assert_eq!(names("src/s/sounds/sounds.gen.ts"), 1);
    assert_eq!(names("src/s/sounds/up.gen.ts"), 1);
    assert_eq!(names("src/s/sounds/rec.gen.ts"), 2);
    assert!(
        !run_in_module("src/s/sounds/esc.gen.ts")
            .diagnostics
            .is_empty()
    );
}

#[test]
fn corpus_mobs() {
    let vfs = Fake::default().with("src/d/mobs.gen.ts", MOBS).with(
        "src/d/mobs.data.json",
        r#"[{"id":"a","health":5},{"id":"b","health":7}]"#,
    );
    let r = run(&vfs, "src/d/mobs.gen.ts");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let dests: Vec<_> = r.items.iter().map(|i| i.dest.path.to_string()).collect();
    assert_eq!(dests, ["entities/a.json", "entities/b.json"]);
    let Body::Json(v) = &r.items[1].body else {
        panic!()
    };
    assert_eq!(
        *v,
        json!({"format_version": "1.21.0", "minecraft:entity": {
            "description": {"identifier": "demo:b"},
            "components": {"minecraft:health": {"value": 7}}}})
    );
    assert!(r.inputs.files.contains_key(&rp("src/d/mobs.data.json")));
    assert_eq!(r.inputs.files.len(), 2);
}

#[test]
fn item_block_file_and_entity_client() {
    let g = r#"generate(() => [
      item("demo:gem", { description: { category: "items" }, components: { "minecraft:max_stack_size": 16 } }),
      block("demo:ore", { format_version: "1.20.80" }),
      file("RP", "texts/en_US.lang", { text: "a=b" }, "langLines"),
      entity("demo:car", {
        client: { geometry: "./models/car.geo.json", texture: "./entities/car.png" },
        spawnRule: { conditions: [] },
        lang: { en_US: "Car" },
      }),
    ]);"#;
    let vfs = Fake::default()
        .with("src/v/a.gen.ts", g)
        .with(
            "src/v/models/car.geo.json",
            r#"{"format_version":"1.12.0","minecraft:geometry":[{"description":{"identifier":"geometry.car"}}]}"#,
        )
        .with("src/v/entities/car.png", "png");
    let r = run(&vfs, "src/v/a.gen.ts");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let by: BTreeMap<_, _> = r
        .items
        .iter()
        .map(|i| (i.dest.path.to_string(), i))
        .collect();
    let Body::Json(v) = &by["items/gem.json"].body else {
        panic!()
    };
    assert_eq!(v["minecraft:item"]["description"]["category"], "items");
    assert_eq!(v["minecraft:item"]["description"]["identifier"], "demo:gem");
    let Body::Json(v) = &by["blocks/ore.json"].body else {
        panic!()
    };
    assert_eq!(v["format_version"], "1.20.80");
    assert_eq!(by["texts/en_US.lang"].merge, Merge::LangLines);
    let c = by.get("texts/en_US.lang").unwrap();
    assert!(matches!(&c.body, Body::Text(_)));
    let Body::Json(v) = &by["entity/car.json"].body else {
        panic!()
    };
    let d = &v["minecraft:client_entity"]["description"];
    assert_eq!(d["geometry"]["default"], "geometry.car");
    assert_eq!(d["textures"]["default"], "textures/entity/car");
    assert_eq!(
        by["models/entity/car.geo.json"].body,
        Body::Passthrough(rp("src/v/models/car.geo.json"))
    );
    assert_eq!(
        by["textures/entity/car.png"].body,
        Body::Passthrough(rp("src/v/entities/car.png"))
    );
    assert!(by.contains_key("spawn_rules/car.json"));
    assert!(r.inputs.files.contains_key(&rp("src/v/entities/car.png")));
    assert!(
        r.inputs
            .files
            .contains_key(&rp("src/v/models/car.geo.json"))
    );
    let lang = r.items.iter().filter(|i| i.merge == Merge::LangLines);
    assert!(
        lang.filter_map(|i| match &i.body {
            Body::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .any(|t| t == "entity.demo:car.name=Car")
    );
}

#[test]
fn invalid_id_is_generator_error() {
    let vfs = Fake::default().with("m/a.gen.ts", "generate(() => item('nocolon'));");
    let r = run(&vfs, "m/a.gen.ts");
    assert_eq!(r.diagnostics[0].code, Code::GeneratorError);
    assert_eq!(
        r.diagnostics[0].span.as_ref().unwrap().path,
        rp("m/a.gen.ts")
    );
}

#[test]
fn observed_inputs_imports_negatives_defines() {
    let vfs = Fake::default()
        .with(
            "m/a.gen.ts",
            "import { k } from './lib';\nimport d from '../shared/d.json';\n\
             generate(({ define }) => item(`demo:${k}${d.n}${define.debug}`));",
        )
        .with("m/lib.js", "export const k = 'x';")
        .with("shared/d.json", r#"{"n":1}"#);
    let r = run(&vfs, "m/a.gen.ts");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(r.items[0].dest.path.as_str(), "items/x1true.json");
    let files: Vec<_> = r.inputs.files.keys().map(|p| p.to_string()).collect();
    assert_eq!(files, ["m/a.gen.ts", "m/lib.js", "shared/d.json"]);
    // `./lib` probed `lib.ts` first.
    assert_eq!(
        r.inputs
            .negatives
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>(),
        ["m/lib.ts"]
    );
    assert_eq!(
        r.inputs.defines,
        BTreeMap::from([("debug".into(), json!(true))])
    );
    assert!(!r.inputs.uncacheable);
}

#[test]
fn bare_and_missing_imports_are_unresolved() {
    let vfs = Fake::default().with("m/a.gen.ts", "import 'lodash';\nimport './nope';\n");
    let r = run(&vfs, "m/a.gen.ts");
    assert_eq!(r.diagnostics.len(), 2);
    assert!(
        r.diagnostics
            .iter()
            .all(|d| d.code == Code::ImportUnresolved)
    );
    assert!(r.inputs.negatives.contains(&rp("m/nope.ts")));
    assert!(r.inputs.negatives.contains(&rp("m/nope/index.ts")));
}

#[test]
fn nondeterminism_is_uncacheable() {
    for src in ["new Date();", "Date.now();", "Math.random();"] {
        let vfs = Fake::default().with("m/a.gen.ts", src);
        assert!(run(&vfs, "m/a.gen.ts").inputs.uncacheable, "{src}");
    }
    let vfs = Fake::default().with("m/a.gen.ts", "new Date(0); generate([]);");
    assert!(!run(&vfs, "m/a.gen.ts").inputs.uncacheable);
}

#[test]
fn non_notfound_io_error_is_not_a_negative() {
    let mut vfs = Fake::default()
        .with("m/a.gen.ts", "import './lib';")
        .with("m/lib.ts", "export {};");
    vfs.fail
        .insert("m/lib.ts".into(), ErrorKind::PermissionDenied);
    let r = run(&vfs, "m/a.gen.ts");
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.diagnostics[0].code, Code::GeneratorError);
    assert!(r.inputs.uncacheable);
    assert!(r.inputs.negatives.is_empty());
}

#[test]
fn json_import_stays_data() {
    let evil = r#"{"s":"\"};alert(1);//"}"#;
    let vfs = Fake::default()
        .with(
            "m/a.gen.ts",
            "import d from './d.json';\ngenerate(file('BP', 'x.json', { json: d }));",
        )
        .with("m/d.json", evil);
    let r = run(&vfs, "m/a.gen.ts");
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    assert_eq!(
        r.items[0].body,
        Body::Json(serde_json::from_str::<Value>(evil).unwrap())
    );
}

#[test]
fn error_line_maps_to_ts() {
    let src = "type T = { a: number };\ninterface I { b: string }\nconst x: T = { a: 1 };\n\ngenerate(() => {\n  const y: number = x.a;\n  throw new Error('boom ' + y);\n});\n";
    let vfs = Fake::default().with("m/a.gen.ts", src);
    let r = run(&vfs, "m/a.gen.ts");
    let d = &r.diagnostics[0];
    assert_eq!(d.code, Code::GeneratorError);
    assert!(d.message.contains("boom 1"));
    let s = d.span.as_ref().unwrap();
    assert_eq!((s.path.as_str(), s.line), ("m/a.gen.ts", Some(7)));
}

#[test]
fn timeout_and_oom_are_diagnostics() {
    let host = Host::new(Limits {
        budget: Duration::from_millis(200),
        memory: 8 << 20,
        ..Limits::default()
    });
    let vfs = Fake::default()
        .with("m/t.gen.ts", "while (true) {}")
        .with(
            "m/o.gen.ts",
            "const a: any[] = []; while (true) a.push(new Array(1000).fill(1));",
        )
        .with("m/ok.gen.ts", "generate(() => item('demo:ok'));");
    assert_eq!(
        run_with(&host, &vfs, "m/t.gen.ts").diagnostics[0].code,
        Code::GeneratorTimeout
    );
    assert_eq!(
        run_with(&host, &vfs, "m/o.gen.ts").diagnostics[0].code,
        Code::GeneratorOutOfMemory
    );
    let ok = run_with(&host, &vfs, "m/ok.gen.ts");
    assert_eq!(ok.items.len(), 1, "{:?}", ok.diagnostics);
}

#[test]
fn fresh_context_no_prototype_leak() {
    let vfs = Fake::default()
        .with("m/a.gen.ts", "globalThis.leak = 1; (Object.prototype as any).polluted = 1;")
        .with(
            "m/b.gen.ts",
            "generate(() => file('BP', 'x.json', { json: { l: typeof (globalThis as any).leak, p: typeof ({} as any).polluted } }));",
        );
    let host = Host::new(Limits::default());
    run_with(&host, &vfs, "m/a.gen.ts");
    let r = run_with(&host, &vfs, "m/b.gen.ts");
    assert_eq!(
        r.items[0].body,
        Body::Json(json!({"l": "undefined", "p": "undefined"}))
    );
}

#[test]
fn stale_jobs_never_outlive_their_run() {
    let host = Host::new(Limits {
        budget: Duration::from_millis(200),
        ..Limits::default()
    });
    let vfs = Fake::default()
        .with("m/x/f.ogg", "x")
        .with(
            "m/a.gen.ts",
            "const f = (): Promise<void> => Promise.resolve().then(f); f();              Promise.resolve().then(() => assets('**/*'));",
        )
        .with(
            "m/b.gen.ts",
            "await null; await null; await null; generate(() => item('demo:b'));",
        );
    let a = run_with(&host, &vfs, "m/a.gen.ts");
    assert_eq!(a.diagnostics[0].code, Code::GeneratorTimeout);
    let b = run_with(&host, &vfs, "m/b.gen.ts");
    assert!(b.diagnostics.is_empty(), "{:?}", b.diagnostics);
    assert_eq!(b.items.len(), 1);
    assert!(b.inputs.dirs.is_empty());
}

#[test]
fn date_constructor_bypass_is_flagged() {
    let vfs = Fake::default().with("m/a.gen.ts", "new (Date.prototype.constructor as any)();");
    assert!(run(&vfs, "m/a.gen.ts").inputs.uncacheable);
}

#[test]
fn define_in_keys_and_stringify_are_observed() {
    for (src, key) in [
        (
            "generate(({ define }) => 'debug' in define ? [] : []);",
            "debug",
        ),
        (
            "generate(({ define }) => Object.keys(define).length ? [] : []);",
            "debug",
        ),
        (
            "generate(({ define }) => JSON.stringify(define) ? [] : []);",
            "debug",
        ),
    ] {
        let vfs = Fake::default().with("m/a.gen.ts", src);
        let r = run(&vfs, "m/a.gen.ts");
        assert!(r.diagnostics.is_empty(), "{src}: {:?}", r.diagnostics);
        assert!(r.inputs.defines.contains_key(key), "{src}");
    }
}
