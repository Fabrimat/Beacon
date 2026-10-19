//! Needs a real esbuild: installed once into `target/esbuild-test/` via npm; skipped without npm.

use beacon_bundler::Esbuild;
use beacon_engine::diag::Code;
use beacon_engine::host::{BundleRequest, Bundler};
use beacon_engine::path::RelPath;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

fn install() -> Option<&'static Path> {
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

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

#[test]
fn bundles_two_entries() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    println!("version: {:?}", esbuild.version());
    assert!(esbuild.version().unwrap().starts_with("esbuild 0.28"));

    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    std::fs::create_dir_all(r.join("src")).unwrap();
    std::fs::write(
        r.join("src/a.js"),
        "import {h} from './helper.js';\nimport {world} from '@minecraft/server';\nconsole.log('ENTRY_A', h(), world);\n",
    )
    .unwrap();
    std::fs::write(r.join("src/b.js"), "console.log('ENTRY_B');\n").unwrap();
    std::fs::write(
        r.join("src/helper.js"),
        "export const h = () => 'HELPER';\n",
    )
    .unwrap();

    let entries = [rp("src/a.js"), rp("src/b.js")];
    let req = BundleRequest {
        root: r,
        entries: &entries,
        sourcemap: true,
        minify: false,
        define: &Default::default(),
        externals: &["@minecraft/server".to_string()],
    };
    let t0 = Instant::now();
    let res = esbuild.bundle(&req);
    println!("bundle (cold) took {:?}", t0.elapsed());
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let code = res.code.expect("code");
    assert!(code.contains("ENTRY_A") && code.contains("ENTRY_B") && code.contains("HELPER"));
    assert!(code.contains("\"@minecraft/server\""), "{code}");
    let want: std::collections::BTreeSet<_> =
        ["src/a.js", "src/b.js", "src/helper.js"].map(rp).into();
    assert_eq!(res.inputs, want);
    assert!(!res.external_inputs);
    assert!(res.map.expect("map").contains("\"mappings\""));

    let mut ms: Vec<_> = (0..10)
        .map(|_| {
            let t0 = Instant::now();
            esbuild.bundle(&req);
            t0.elapsed()
        })
        .collect();
    ms.sort();
    println!(
        "bundle warm: min {:?} median {:?} max {:?}",
        ms[0], ms[5], ms[9]
    );
}

#[test]
fn syntax_error_has_span() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    let t = tempfile::tempdir().unwrap();
    std::fs::write(t.path().join("bad.js"), "let ok = 1;\nlet x x;\n").unwrap();
    let entries = [rp("bad.js")];
    let res = esbuild.bundle(&BundleRequest {
        root: t.path(),
        entries: &entries,
        sourcemap: false,
        minify: false,
        define: &Default::default(),
        externals: &[],
    });
    assert!(res.code.is_none());
    let d = &res.diagnostics[0];
    assert_eq!(d.code, Code::BundleError);
    let sp = d.span.as_ref().expect("span");
    assert_eq!((sp.path.as_str(), sp.line), ("bad.js", Some(2)));
}

#[test]
fn outside_root_imports_are_flagged() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    let t = tempfile::tempdir().unwrap();
    let root = t.path().join("proj");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(t.path().join("shared")).unwrap();
    std::fs::write(
        t.path().join("shared/x.js"),
        "export const x = 'SHARED';
",
    )
    .unwrap();
    std::fs::write(
        root.join("src/a.js"),
        "import { x } from '../../shared/x.js';
console.log(x);
",
    )
    .unwrap();
    let entries = [rp("src/a.js")];
    let res = esbuild.bundle(&BundleRequest {
        root: &root,
        entries: &entries,
        sourcemap: false,
        minify: false,
        define: &Default::default(),
        externals: &[],
    });
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    assert!(res.code.unwrap().contains("SHARED"));
    assert!(res.external_inputs);
    assert_eq!(res.inputs, [rp("src/a.js")].into());
}

#[test]
fn define_becomes_constant() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    let t = tempfile::tempdir().unwrap();
    std::fs::write(
        t.path().join("a.js"),
        "if (beacon.define.BETA_MODE) console.log('BETA_ON', beacon.define.label);\n",
    )
    .unwrap();
    let entries = [rp("a.js")];
    let define = serde_json::from_str(r#"{"BETA_MODE":true,"label":"a \"b\" c"}"#).unwrap();
    let res = esbuild.bundle(&BundleRequest {
        root: t.path(),
        entries: &entries,
        sourcemap: false,
        minify: false,
        define: &define,
        externals: &[],
    });
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let code = res.code.unwrap();
    assert!(code.contains("BETA_ON"), "{code}");
    assert!(!code.contains("beacon.define"), "{code}");
    assert!(
        code.contains(r#"a \"b\" c"#) || code.contains(r#"'a "b" c'"#),
        "{code}"
    );
}

#[test]
fn only_declared_modules_are_external() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    let t = tempfile::tempdir().unwrap();
    let nm = t.path().join("node_modules/@minecraft/vanilla-data");
    std::fs::create_dir_all(&nm).unwrap();
    std::fs::write(
        nm.join("package.json"),
        r#"{"name":"@minecraft/vanilla-data","main":"index.js"}"#,
    )
    .unwrap();
    std::fs::write(
        nm.join("index.js"),
        "export const VANILLA = 'INLINED_VD';
",
    )
    .unwrap();
    std::fs::write(
        t.path().join("a.js"),
        "import {VANILLA} from '@minecraft/vanilla-data';
import {world} from '@minecraft/server';
import {x} from '@minecraft/server/deep';
console.log(VANILLA, world, x);
",
    )
    .unwrap();
    let entries = [rp("a.js")];
    let res = esbuild.bundle(&BundleRequest {
        root: t.path(),
        entries: &entries,
        sourcemap: false,
        minify: false,
        define: &Default::default(),
        externals: &["@minecraft/server".to_string()],
    });
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let code = res.code.unwrap();
    assert!(code.contains("INLINED_VD"), "{code}");
    assert!(!code.contains("\"@minecraft/vanilla-data\""), "{code}");
    assert!(code.contains("\"@minecraft/server\""), "{code}");
    assert!(code.contains("\"@minecraft/server/deep\""), "{code}");
}

/// Debugger interop: map `sources` are project-relative and resolve to real files, also when the
/// project is on another drive than the system TEMP (the repo's `target/` vs `%TEMP%`).
#[test]
fn map_sources_are_project_relative() {
    let Some(inst) = install() else { return };
    let esbuild = Esbuild::find(inst, beacon_bundler::Confirm::No).expect("find");
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let t = tempfile::tempdir_in(base).unwrap();
    // no verbatim prefix: the engine hands esbuild a plain path
    let canon = t.path().canonicalize().unwrap();
    let r = PathBuf::from(canon.to_string_lossy().trim_start_matches(r"\\?\"));
    std::fs::create_dir_all(r.join("src/lib")).unwrap();
    std::fs::write(
        r.join("src/main.ts"),
        "import {h} from './lib/h';\nconsole.log(h());\n",
    )
    .unwrap();
    std::fs::write(
        r.join("src/lib/h.ts"),
        "export const h = (): string => 'H';\n",
    )
    .unwrap();
    let entries = [rp("src/main.ts")];
    let req = BundleRequest {
        root: &r,
        entries: &entries,
        sourcemap: true,
        minify: false,
        define: &Default::default(),
        externals: &[],
    };
    let res = esbuild.bundle(&req);
    assert!(res.diagnostics.is_empty(), "{:?}", res.diagnostics);
    let map: serde_json::Value = serde_json::from_str(&res.map.expect("map")).unwrap();
    let mut got: Vec<_> = map["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect();
    got.sort();
    assert_eq!(got, ["src/lib/h.ts", "src/main.ts"]);
    assert!(got.iter().all(|s| r.join(s).is_file()));
}

#[test]
fn alias_replaces_a_module() {
    let Some(inst) = install() else { return };
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    std::fs::write(
        r.join("main.js"),
        "import { register } from '@minecraft/server-gametest';\nregister('a', 'b', () => {});\n",
    )
    .unwrap();
    let shim = r.join("shim.js");
    std::fs::write(
        &shim,
        "import * as real from 'real-gt';\nexport * from 'real-gt';\nexport const register = (...a) => (console.warn('SHIMMED'), real.register(...a));\n",
    )
    .unwrap();
    let b = beacon_bundler::LazyEsbuild::new(inst, beacon_bundler::Confirm::No)
        .with_alias("@minecraft/server-gametest", &shim);
    let res = b.bundle(&BundleRequest {
        root: r,
        entries: &[rp("main.js")],
        sourcemap: false,
        minify: false,
        define: &Default::default(),
        // the alias wins over the external
        externals: &["@minecraft/server-gametest".into(), "real-gt".into()],
    });
    let code = res.code.unwrap_or_else(|| panic!("{:?}", res.diagnostics));
    assert!(
        code.contains("SHIMMED") && code.contains("\"real-gt\""),
        "{code}"
    );
    assert!(!code.contains("\"@minecraft/server-gametest\""), "{code}");
}
