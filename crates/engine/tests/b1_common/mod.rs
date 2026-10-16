#![allow(dead_code)]
//! Fake host/bundler and a tiny project on disk for the B1 orchestration tests.
//!
//! The fake generator's source is a script, one instruction per line, recorded like the real
//! host records observed inputs:
//! `read <path>` (file hash, or a negative when absent), `list <dir>` (dir listing hash),
//! `define <key>`, `uncacheable`. It emits one function file per run.

use beacon_engine::build::{BuildOptions, BuildOutcome, Session};
use beacon_engine::diag::{Code, Diagnostic, Severity};
use beacon_engine::host::{BundleRequest, BundleResult, Bundler, GenResult, GeneratorHost};
use beacon_engine::model::{Body, ContentItem, Dest, GenCtx, Generator, Merge, Pack};
use beacon_engine::path::{RelPath, Vfs};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

pub struct FakeHost(pub Arc<AtomicUsize>);

impl GeneratorHost for FakeHost {
    fn run(&self, g: &Generator, ctx: &GenCtx, vfs: &dyn Vfs) -> GenResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        let mut r = GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        };
        let src = String::from_utf8(vfs.read(&g.path).unwrap()).unwrap();
        r.inputs
            .files
            .insert(g.path.clone(), vfs.hash(&g.path).unwrap());
        for line in src.lines() {
            match line.split_once(' ') {
                Some(("read", p)) => match vfs.hash(&rp(p)) {
                    Ok(h) => {
                        r.inputs.files.insert(rp(p), h);
                    }
                    Err(_) => {
                        r.inputs.negatives.insert(rp(p));
                    }
                },
                Some(("list", d)) => {
                    let names = vfs.list(Some(&rp(d)));
                    let h = *blake3::hash(names.join("\n").as_bytes()).as_bytes();
                    r.inputs.dirs.insert(rp(d), h);
                }
                Some(("define", k)) => {
                    let v = ctx.define.get(k).cloned().unwrap_or_default();
                    r.inputs.defines.insert(k.into(), v);
                }
                None if line == "uncacheable" => r.inputs.uncacheable = true,
                _ => {}
            }
        }
        let name = g.path.file_name().trim_end_matches(".gen.ts");
        r.items.push(ContentItem {
            dest: Dest {
                output: g.output.clone(),
                pack: Pack::Behavior,
                path: rp(&format!("functions/{}/{name}.mcfunction", ctx.module)),
            },
            body: Body::Text(format!("say {} {:?}\n", ctx.target, ctx.define)),
            merge: Merge::Error,
            origin: g.path.clone(),
            module: g.module.clone(),
        });
        r
    }

    fn version(&self) -> String {
        "fake host 1".into()
    }
}

pub struct FakeBundler;

impl Bundler for FakeBundler {
    fn bundle(&self, req: &BundleRequest) -> BundleResult {
        BundleResult {
            code: Some(format!("// {} entries\n", req.entries.len())),
            map: req.sourcemap.then(|| "{}".into()),
            inputs: req.entries.iter().cloned().collect(),
            external_inputs: false,
            diagnostics: vec![],
        }
    }
    fn version(&self) -> Option<String> {
        Some("fake 1".into())
    }
}

pub const CONFIG: &str = r#"{
  "type": "minecraftBedrock", "name": "b1 test", "targetVersion": "1.21.100", "namespace": "demo",
  "beacon": {
    "targets": {
      "dev": { "edition": "bedrock", "artifact": "dev", "define": { "FLAG": 1 } },
      "addon": { "edition": "bedrock", "artifact": "mcaddon" },
      "pack": { "edition": "bedrock", "artifact": "mcpack" }
    },
    "scriptModules": { "@minecraft/server": "2.10.0" }
  }
}"#;

pub fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

/// config + one module with an item, a texture, a lang file and `extra` files.
pub fn project(extra: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(d.path(), "config.json", CONFIG);
    write(
        d.path(),
        "src/m/items/sword.item.json",
        r#"{"format_version":"1.21.100","minecraft:item":{"description":{"identifier":"demo:sword"},"components":{}}}"#,
    );
    write(d.path(), "src/m/items/sword.png", "PNG");
    write(
        d.path(),
        "src/m/texts/en_US.lang",
        "item.demo:sword=Sword\n",
    );
    for (p, b) in extra {
        write(d.path(), p, b);
    }
    d
}

pub fn opts(root: &Path, target: Option<&str>) -> BuildOptions {
    BuildOptions {
        target: target.map(String::from),
        out: Some(root.join("out")),
        trusted: true,
        ..Default::default()
    }
}

pub fn session(opts: BuildOptions, root: &Path, runs: &Arc<AtomicUsize>) -> Session {
    Session::new(
        root,
        opts,
        Box::new(FakeHost(runs.clone())),
        Some(Box::new(FakeBundler)),
    )
}

pub fn errors(o: &BuildOutcome) -> Vec<&Diagnostic> {
    o.diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect()
}

pub fn has(o: &BuildOutcome, c: Code) -> bool {
    o.diagnostics.iter().any(|d| d.code == c)
}
