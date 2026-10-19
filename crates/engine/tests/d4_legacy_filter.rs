//! Phase 3 B3: the `legacy_filter` external-process plugin (needs Python on PATH).

mod b1_common;
use b1_common::*;
use beacon_engine::build::BuildOutcome;
use beacon_engine::diag::Code;
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

const PY: &str = if cfg!(windows) { "python" } else { "python3" };

/// Builds a project whose single filter is `script` (run as `python filter.py`).
fn build(
    script: &str,
    extra_cfg: serde_json::Value,
    trusted: bool,
) -> (tempfile::TempDir, BuildOutcome) {
    let mut def = json!({"run": [PY, "filter.py"], "cwd": "tools", "settings": {"k": "v"}});
    def.as_object_mut()
        .unwrap()
        .extend(extra_cfg.as_object().unwrap().clone());
    let config =
        json!({"type":"minecraftBedrock","name":"lf","targetVersion":"1.21.100","namespace":"demo",
        "beacon":{"plugins":[{"legacyFilter": def}]}})
        .to_string();
    let p = project(&[
        ("config.json", &config),
        ("tools/filter.py", script),
        ("src/m/texts/en_US.lang", "item.demo:a=sword\n"),
        ("src/m/functions/f.mcfunction", "say hi\n"),
    ]);
    let mut o = opts(p.path(), None);
    o.trusted = trusted;
    let r = session(o, p.path(), &Arc::new(AtomicUsize::new(0))).build();
    (p, r)
}

fn out(p: &Path, rel: &str) -> std::io::Result<String> {
    std::fs::read_to_string(p.join("out/main").join(rel))
}

#[test]
fn round_trip() {
    let (p, r) = build(
        "import pathlib\nf = pathlib.Path('RP/texts/en_US.lang')\nf.write_text(f.read_text().upper())\n\
         pathlib.Path('BP/functions/added.mcfunction').write_text('say added')\n",
        json!({}),
        true,
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(
        out(p.path(), "RP/texts/en_US.lang")
            .unwrap()
            .contains("ITEM.DEMO:A=SWORD")
    );
    assert_eq!(
        out(p.path(), "BP/functions/added.mcfunction").unwrap(),
        "say added"
    );
}

#[test]
fn env_is_scrubbed() {
    let (p, r) = build(
        "import os, json, sys\nopen('BP/env.json','w').write(json.dumps({'env': sorted(os.environ), 'arg': sys.argv[-1], 'cwd': os.getcwd()}))\n",
        json!({}),
        true,
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    let v: serde_json::Value =
        serde_json::from_str(&out(p.path(), "BP/env.json").unwrap()).unwrap();
    let allow = beacon_engine::legacy_filter::ENV_ALLOW;
    for k in v["env"].as_array().unwrap() {
        // Windows adds nothing else to a cleared env; accept only the allowlist (plus LC_CTYPE,
        // which Python itself sets on Unix when it coerces the C locale, PEP 538)
        if k == "LC_CTYPE" {
            continue;
        }
        assert!(
            allow
                .iter()
                .any(|a| a.eq_ignore_ascii_case(k.as_str().unwrap())),
            "leaked {k}"
        );
    }
    assert_eq!(v["arg"], r#"{"k":"v"}"#);
    assert!(v["cwd"].as_str().unwrap().contains("beacon-lf-"));
}

#[test]
fn untrusted_never_spawns() {
    let marker = tempfile::tempdir().unwrap().keep().join("ran");
    let script = format!("open({:?}, 'w').write('x')\n", marker.to_str().unwrap());
    let (_p, r) = build(&script, json!({}), false);
    assert!(!marker.exists(), "filter ran in an untrusted session");
    assert!(has(&r, Code::Untrusted));
}

fn escaped(script: &str) {
    let (p, r) = build(script, json!({}), true);
    assert!(has(&r, Code::LegacyFilterEscaped), "{:?}", r.diagnostics);
    // the output is discarded: the filter's addition is absent, nothing written
    assert!(out(p.path(), "BP/functions/added.mcfunction").is_err());
}

#[test]
fn escape_dotdot() {
    escaped(
        "open('BP/functions/added.mcfunction','w').write('x')\nopen('../evil.txt','w').write('x')\n",
    );
}

#[test]
fn escape_absolute() {
    escaped(
        "import os\nopen('BP/functions/added.mcfunction','w').write('x')\nopen(os.path.join(os.path.dirname(os.getcwd()), 'abs.txt'),'w').write('x')\n",
    );
}

#[test]
fn escape_symlink() {
    // creating a symlink needs privilege on Windows; a junction is covered by is_link
    let script = "import os\nopen('BP/functions/added.mcfunction','w').write('x')\n\
        try:\n    os.symlink(os.getcwd(), 'BP/link', target_is_directory=True)\nexcept OSError:\n    open('../evil.txt','w').write('x')\n";
    escaped(script);
}

#[test]
fn outside_bp_rp_is_ignored_with_a_note() {
    let (p, r) = build("open('stray.txt','w').write('x')\n", json!({}), true);
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert!(has(&r, Code::LegacyFilterEscaped));
    assert!(out(p.path(), "stray.txt").is_err());
}

#[test]
fn failure_exit_and_timeout() {
    let (_p, r) = build("import sys\nsys.exit(3)\n", json!({}), true);
    assert!(has(&r, Code::LegacyFilterFailed), "{:?}", r.diagnostics);
    let (_p, r) = build(
        "import time\ntime.sleep(60)\n",
        json!({"timeoutSecs": 1}),
        true,
    );
    assert!(has(&r, Code::LegacyFilterFailed), "{:?}", r.diagnostics);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("timed out"))
    );
}

#[test]
fn failure_writes_no_artifacts() {
    let (p, r) = build(
        "import sys
sys.exit(3)
",
        json!({}),
        true,
    );
    assert!(has(&r, Code::LegacyFilterFailed));
    assert!(!p.path().join("out/main").exists());
}

#[test]
fn manifest_is_restored() {
    let uuid = |p: &Path, f: &str| {
        serde_json::from_str::<serde_json::Value>(&out(p, f).unwrap()).unwrap()["header"]["uuid"]
            .clone()
    };
    let (p, r) = build(
        "import json, os
f='BP/manifest.json'
m=json.load(open(f))
m['header']['uuid']='00000000-0000-0000-0000-000000000000'
m['header']['version']=[9,9,9]
json.dump(m,open(f,'w'))
os.remove('RP/manifest.json')
",
        json!({}),
        true,
    );
    assert!(errors(&r).is_empty(), "{:?}", r.diagnostics);
    assert_ne!(
        uuid(p.path(), "BP/manifest.json"),
        "00000000-0000-0000-0000-000000000000"
    );
    let m: serde_json::Value =
        serde_json::from_str(&out(p.path(), "BP/manifest.json").unwrap()).unwrap();
    assert_ne!(m["header"]["version"], json!([9, 9, 9]));
    assert!(out(p.path(), "RP/manifest.json").is_ok());
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("generated manifest was restored"))
    );
}

#[test]
fn size_caps_fail() {
    let big = "open('BP/big.bin','wb').write(b'x'*5000)
";
    let (_p, r) = build(big, json!({"maxFileBytes": 1000}), true);
    assert!(has(&r, Code::LegacyFilterFailed), "{:?}", r.diagnostics);
    let (_p, r) = build(big, json!({"maxTotalBytes": 1000}), true);
    assert!(has(&r, Code::LegacyFilterFailed), "{:?}", r.diagnostics);
}
