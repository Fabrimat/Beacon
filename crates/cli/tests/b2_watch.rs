//! Workstream B2a: `beacon watch` end to end (coalescing, ignored paths) and the in-process
//! style latency measurement (`cargo test -p beacon-cli --release --test b2_watch -- --ignored --nocapture`).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

struct Watch {
    child: Child,
    rx: Receiver<serde_json::Value>,
}

impl Watch {
    fn start(project: &Path, extra: &[&str], esbuild: Option<&Path>) -> Watch {
        let mut c = Command::new(env!("CARGO_BIN_EXE_beacon"));
        c.args(["--json", "--project", project.to_str().unwrap(), "watch"])
            .args(extra)
            .env("BEACON_HOME", project.join("home"))
            .env_remove("BEACON_COM_MOJANG")
            .env(
                "BEACON_ESBUILD",
                esbuild.map_or("no-esbuild-here".into(), |p| p.as_os_str().to_owned()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str(&l).unwrap());
            }
        });
        Watch { child, rx }
    }

    fn next(&self, within: Duration) -> Option<serde_json::Value> {
        self.rx.recv_timeout(within).ok()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    write(
        d.path(),
        "config.json",
        r#"{"type":"minecraftBedrock","name":"t","targetVersion":"1.21.100","namespace":"demo",
            "packs":{"behaviorPack":"./packs/BP","resourcePack":"./packs/RP"}}"#,
    );
    write(d.path(), "src/m/functions/a.mcfunction", "say a\n");
    d
}

#[test]
fn burst_is_one_build_and_outputs_are_ignored() {
    let p = project();
    let w = Watch::start(p.path(), &[], None);
    let ready = w.next(Duration::from_secs(30)).expect("ready");
    assert_eq!(ready["event"], "ready");
    let out = p.path().join("packs/BP/functions/m/a.mcfunction");
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "say a\n");

    // nothing under .beacon, packs, .git or dist triggers a build
    write(p.path(), ".beacon/x/y.txt", "1");
    write(p.path(), "packs/BP/functions/m/a.mcfunction", "tampered\n");
    write(p.path(), "packs/BP/new.txt", "1");
    write(p.path(), "dist/z.txt", "1");
    write(p.path(), ".git/HEAD", "1");
    assert!(
        w.next(Duration::from_millis(700)).is_none(),
        "ignored paths must not build"
    );

    // a burst of edits is exactly one build
    for i in 0..6 {
        write(
            p.path(),
            &format!("src/m/functions/f{i}.mcfunction"),
            "say hi\n",
        );
    }
    write(p.path(), "src/m/functions/a.mcfunction", "say changed\n");
    // a slow disk may split the burst in two builds; the final state must be right
    let f5 = p.path().join("packs/BP/functions/m/f5.mcfunction");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut builds = 0;
    while !(f5.is_file()
        && std::fs::read_to_string(&out).unwrap()
            == "say changed
")
    {
        let built = w
            .next(deadline.saturating_duration_since(Instant::now()))
            .expect("final state not reached");
        assert_eq!(
            (built["event"].as_str(), built["errors"].as_u64()),
            (Some("built"), Some(0))
        );
        builds += 1;
    }
    assert!(builds <= 2, "{builds} builds for one burst");
    while let Some(e) = w.next(Duration::from_millis(700)) {
        assert_eq!(e["errors"], 0); // at most a trailing split-burst build
    }

    // deleting a source removes its output, foreign files survive
    write(p.path(), "packs/BP/foreign.txt", "mine");
    std::fs::remove_file(p.path().join("src/m/functions/f0.mcfunction")).unwrap();
    assert_eq!(w.next(Duration::from_secs(30)).unwrap()["event"], "built");
    assert!(!p.path().join("packs/BP/functions/m/f0.mcfunction").exists());
    assert!(p.path().join("packs/BP/foreign.txt").is_file());
}

/// macOS FSEvents (and inotify via a resolved path) report real paths: a project opened
/// through a symlink must still rebuild. Edits through the real path and the link both count.
#[cfg(unix)]
#[test]
fn watch_through_a_symlinked_root() {
    let p = project();
    let links = tempfile::tempdir().unwrap();
    let link = links.path().join("proj");
    std::os::unix::fs::symlink(p.path(), &link).unwrap();
    let w = Watch::start(&link, &[], None);
    assert_eq!(
        w.next(Duration::from_secs(30)).expect("ready")["event"],
        "ready"
    );
    let out = p.path().join("packs/BP/functions/m/a.mcfunction");
    for (root, body) in [
        (
            p.path(),
            "say real
",
        ),
        (
            link.as_path(),
            "say link
",
        ),
    ] {
        write(root, "src/m/functions/a.mcfunction", body);
        let deadline = Instant::now() + Duration::from_secs(30);
        while std::fs::read_to_string(&out).unwrap() != body {
            let e = w
                .next(deadline.saturating_duration_since(Instant::now()))
                .expect("no rebuild through the symlinked root");
            assert_eq!(e["errors"], 0);
        }
    }
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() as f64 * q).ceil() as usize).clamp(1, v.len()) - 1]
}

/// Edit -> `built` latency on the large corpus (`cargo corpus --size large --out
/// target/corpus-large --force`; esbuild from `BEACON_ESBUILD` or target/esbuild-test).
#[test]
#[ignore = "needs target/corpus-large; run with --release --ignored --nocapture"]
fn latency_large_corpus() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let corpus = std::env::var_os("BEACON_CORPUS").map_or(root.join("corpus-large"), Into::into);
    let esbuild = std::env::var_os("BEACON_ESBUILD")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("esbuild-test/node_modules/@esbuild/win32-x64/esbuild.exe"));
    let out = tempfile::tempdir().unwrap();
    let json = corpus.join("src/lib_000/blocks/storm_idol_0_5.block.json");
    let ts = corpus.join("src/lib_000/scripts/handlers.ts");
    let (json0, ts0) = (
        std::fs::read_to_string(&json).unwrap(),
        std::fs::read_to_string(&ts).unwrap(),
    );
    let w = Watch::start(
        &corpus,
        &["--trust", "--out", out.path().to_str().unwrap()],
        Some(&esbuild),
    );
    let ready = w.next(Duration::from_secs(120)).expect("ready");
    println!("initial build: {:.0} ms", ready["ms"].as_f64().unwrap());

    let run = |name: &str, edit: &dyn Fn(usize) -> String, path: &Path| {
        let (mut total, mut inner) = (vec![], vec![]);
        for i in 0..22 {
            std::thread::sleep(Duration::from_millis(300));
            let t = Instant::now();
            std::fs::write(path, edit(i)).unwrap();
            let e = w.next(Duration::from_secs(30)).expect("built");
            let dt = t.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(
                (e["event"].as_str(), e["errors"].as_u64()),
                (Some("built"), Some(0))
            );
            if i >= 2 {
                // the first two are warm-up
                total.push(dt);
                inner.push(e["ms"].as_f64().unwrap());
            }
        }
        println!(
            "{name}: edit->built p50 {:.0} ms p95 {:.0} ms | build only p50 {:.0} ms p95 {:.0} ms",
            pct(&mut total, 0.5),
            pct(&mut total, 0.95),
            pct(&mut inner, 0.5),
            pct(&mut inner, 0.95)
        );
    };
    run(
        "json",
        &|i| {
            json0.replace(
                "\"seconds_to_destroy\": 7.0",
                &format!("\"seconds_to_destroy\": 8.{i}"),
            )
        },
        &json,
    );
    run(
        "ts",
        &|i| format!("{ts0}\nexport const edit{i} = {i};\n"),
        &ts,
    );
    drop(w);
    std::fs::write(json, json0).unwrap();
    std::fs::write(ts, ts0).unwrap();
}
