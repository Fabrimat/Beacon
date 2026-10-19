//! `beacon test` (8.5) on `fixtures/gametest` (one passing, one failing, one hanging GameTest).
//! The EULA gate runs always; the real BDS run only with `BEACON_BDS_EULA=accept` (it uses the
//! BDS cached under `BEACON_HOME` or downloads it, and `BEACON_ESBUILD` if set).

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn fixture() -> tempfile::TempDir {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/gametest");
    let t = tempfile::tempdir().unwrap();
    copy(&src, t.path());
    t
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (p, q) = (e.path(), to.join(e.file_name()));
        if p.is_dir() {
            copy(&p, &q);
        } else {
            std::fs::copy(&p, &q).unwrap();
        }
    }
}

fn beacon(project: &Path, args: &[&str], f: impl FnOnce(&mut Command)) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"));
    c.arg("test").args(args).arg("--project").arg(project);
    f(&mut c);
    c.stdin(Stdio::null()).output().unwrap()
}

fn sarif_codes(o: &Output) -> Vec<String> {
    let v: Value = serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&o.stdout)));
    v["runs"][0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ruleId"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn eula_refused_before_any_download() {
    let (p, home) = (fixture(), tempfile::tempdir().unwrap());
    for args in [&["--sarif", "--yes"][..], &["--json", "--yes"]] {
        let o = beacon(p.path(), args, |c| {
            c.env("BEACON_HOME", home.path())
                .env_remove("BEACON_BDS_EULA")
                .env("BEACON_DOWNLOAD", "yes");
        });
        let err = String::from_utf8_lossy(&o.stderr);
        assert_eq!(o.status.code(), Some(2), "{err}");
        assert!(
            err.contains("BCN0193") && err.contains("--accept-eula"),
            "{err}"
        );
        if args[0] == "--sarif" {
            assert_eq!(sarif_codes(&o), ["BCN0193"]);
        }
    }
    // no tools dir: nothing downloaded, no EULA log
    assert!(!home.path().join("tools").exists());
    assert!(!p.path().join(".beacon").exists());
    let o = beacon(p.path(), &["--json", "--sarif"], |_| {});
    assert_eq!(o.status.code(), Some(2));
}

/// The real BDS: pass, failed (with the test's message), timedOut; SARIF BCN0190 + BCN0191; a
/// filter matching only the passing test exits 0.
#[test]
fn real_bds_run() {
    if std::env::var("BEACON_BDS_EULA").as_deref() != Ok("accept") {
        eprintln!("SKIP: set BEACON_BDS_EULA=accept to run GameTests on the real BDS");
        return;
    }
    let p = fixture();
    let run = |args: &[&str]| {
        let mut a = vec!["--trust", "--yes", "--timeout", "180"];
        a.extend(args);
        beacon(p.path(), &a, |_| {})
    };

    let o = run(&["--json"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{err}");
    let v: Value = serde_json::from_slice(&o.stdout).expect("TestReport");
    assert_eq!(v["schema"], 1);
    assert_eq!(v["bdsVersion"], beacon_bds::VERSION);
    let st: Vec<_> = v["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
                r["message"].as_str(),
            )
        })
        .collect();
    assert_eq!(st.len(), 3, "{v}");
    assert!(st.contains(&("beacon:passes", "passed", None)), "{v}");
    assert!(
        st.contains(&("beacon:fails", "failed", Some("expected a pig"))),
        "{v}"
    );
    assert!(
        st.iter()
            .any(|(n, s, _)| *n == "beacon:hangs" && *s == "timedOut"),
        "{v}"
    );

    let o = run(&["--sarif"]);
    assert_eq!(o.status.code(), Some(1));
    let codes = sarif_codes(&o);
    assert!(
        codes.iter().any(|c| c == "BCN0190") && codes.iter().any(|c| c == "BCN0191"),
        "{codes:?}"
    );

    let o = run(&["--json", "--filter", "beacon:pa*"]);
    let v: Value = serde_json::from_slice(&o.stdout).expect("TestReport");
    assert_eq!(o.status.code(), Some(0), "{v}");
    assert_eq!(v["complete"], true);
    assert_eq!(v["results"].as_array().unwrap().len(), 1, "{v}");
}
