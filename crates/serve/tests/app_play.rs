//! P5-2A: Play through the daemon, and the content-log tail.
//!
//! `harness = false`: this binary is also the daemon the client starts (`serve --project
//! <root>`). `BEACON_HOME`, `BEACON_SERVE_IDLE=0` and `BEACON_COM_MOJANG` are set once for the
//! run; tests run one after the other. Nothing here launches the game (the override skips it).

use beacon_bundler::{Confirm, LazyEsbuild};
use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::diag::Severity;
use beacon_engine::host::{Bundler, GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use beacon_serve::app::{AppCore, ContentLogLine, MAX_TEXT};
use beacon_serve::daemon;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

struct NoHost;

impl GeneratorHost for NoHost {
    fn run(&self, _: &Generator, _: &GenCtx, _: &dyn Vfs) -> GenResult {
        GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        }
    }
    fn version(&self) -> String {
        "none".into()
    }
}

fn daemon_main(project: &Path) -> i32 {
    let opts = daemon::DaemonOptions::new(project).expect("run dir");
    daemon::run(
        &opts,
        Box::new(|root, o: BuildOptions| {
            // like `beacon serve`: a trusted session bundles scripts with esbuild (BEACON_ESBUILD)
            let bundler = o
                .trusted
                .then(|| Box::new(LazyEsbuild::new(root, Confirm::No)) as Box<dyn Bundler>);
            Session::new(root, o, Box::new(NoHost), bundler)
        }),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("serve") {
        std::process::exit(daemon_main(Path::new(&args[2])));
    }
    if args.iter().any(|a| a == "--manual") {
        std::process::exit(manual());
    }
    let home = tempfile::tempdir().unwrap();
    let mojang = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded here; nothing has started yet.
    unsafe {
        std::env::set_var("BEACON_HOME", home.path());
        std::env::set_var("BEACON_SERVE_IDLE", "0");
        std::env::set_var("BEACON_COM_MOJANG", mojang.path());
    }
    let filter = args.iter().find(|a| !a.starts_with('-'));
    let tests: &[(&str, fn())] = &[
        (
            "play_deploys_a_fresh_project_through_the_daemon",
            play_deploys_a_fresh_project_through_the_daemon,
        ),
        (
            "untrusted_project_with_code_is_bcn0050_and_builds_nothing",
            untrusted_project_with_code_is_bcn0050_and_builds_nothing,
        ),
        (
            "no_install_is_bcn0200_the_headline",
            no_install_is_bcn0200_the_headline,
        ),
        (
            "content_log_lines_are_sanitized_and_capped",
            content_log_lines_are_sanitized_and_capped,
        ),
        (
            "content_log_follows_a_running_game_then_a_new_start_and_survives_a_huge_line",
            content_log_follows_a_running_game_then_a_new_start_and_survives_a_huge_line,
        ),
    ];
    let mut failed = vec![];
    for (name, t) in tests {
        if filter.is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        let ok = std::panic::catch_unwind(t).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if !failed.is_empty() {
        eprintln!("FAILED: {failed:?}");
        std::process::exit(1);
    }
    println!("test result: ok");
}

fn core() -> AppCore {
    AppCore::new(std::env::current_exe().unwrap())
}

fn mojang() -> std::path::PathBuf {
    std::env::var_os("BEACON_COM_MOJANG").unwrap().into()
}

fn create(dir: &Path) {
    core()
        .init_create(json!({ "folder": dir, "name": "Demo", "agentFiles": false }))
        .unwrap_or_else(|e| panic!("init_create: {e:?}"));
}

fn dev_packs() -> Vec<String> {
    ["development_behavior_packs", "development_resource_packs"]
        .iter()
        .flat_map(|d| {
            std::fs::read_dir(mojang().join(d))
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
        })
        .collect()
}

fn clear_packs() {
    for d in ["development_behavior_packs", "development_resource_packs"] {
        let _ = std::fs::remove_dir_all(mojang().join(d));
    }
}

fn play_deploys_a_fresh_project_through_the_daemon() {
    clear_packs();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    create(&dir);
    let r = core().play(dir, None).expect("play");
    let errors: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(r.deployed_to.as_deref(), Some(mojang().as_path()));
    assert!(!r.launched, "BEACON_COM_MOJANG never launches the game");
    assert!(!dev_packs().is_empty(), "dev pack folders in com.mojang");
}

fn untrusted_project_with_code_is_bcn0050_and_builds_nothing() {
    clear_packs();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    create(&dir);
    // a plugin changes the trust key: the stored decision no longer matches
    let p = dir.join("config.json");
    let mut c: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    c["beacon"]["plugins"] = json!([{ "legacyFilter": { "id": "a", "cmd": ["one"] } }]);
    std::fs::write(&p, serde_json::to_vec_pretty(&c).unwrap()).unwrap();
    let r = core().play(dir, None).expect("a report, not an error");
    assert_eq!(r.diagnostics[0].code.id(), "BCN0050", "{r:?}");
    assert!(r.deployed_to.is_none() && !r.launched);
    assert!(dev_packs().is_empty(), "no build output: {:?}", dev_packs());
}

fn no_install_is_bcn0200_the_headline() {
    clear_packs();
    // the override would win over an empty install list, so lift it for this case
    let saved = std::env::var_os("BEACON_COM_MOJANG").unwrap();
    // SAFETY: tests run one after the other; no other thread reads the environment.
    unsafe { std::env::remove_var("BEACON_COM_MOJANG") };
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("demo");
    create(&dir);
    let r = core().play_with(dir, None, &[]);
    // SAFETY: as above.
    unsafe { std::env::set_var("BEACON_COM_MOJANG", saved) };
    let r = r.expect("a report, not an error");
    assert_eq!(r.diagnostics.len(), 1, "{r:?}");
    assert_eq!(r.diagnostics[0].code.id(), "BCN0200");
    assert!(
        r.diagnostics[0]
            .message
            .contains("Start Minecraft once, sign in and reach the main menu, then retry.")
    );
    assert!(r.deployed_to.is_none() && !r.launched);
    assert!(dev_packs().is_empty());
}

fn content_log_lines_are_sanitized_and_capped() {
    let logs = tempfile::tempdir().unwrap();
    // SAFETY: tests run one after the other; no other thread reads the environment.
    unsafe { std::env::set_var("BEACON_CONTENT_LOG_DIR", logs.path()) };
    let since = SystemTime::now() - Duration::from_secs(1);
    // an old log is ignored; the new one has an ANSI line, a huge line, an incomplete last line
    let long = format!("12:00:01[Scripting][inform]-{}", "y".repeat(2 * MAX_TEXT));
    std::fs::write(
        logs.path().join("ContentLog2026-10-19_12-00-00_1.txt"),
        format!(
            "12:00:00[Scripting][error]-\u{1b}[31mboom\u{1b}[0m\u{7}\r\n\n{long}\n12:00:02[Scripting][inform]-partial"
        ),
    )
    .unwrap();
    let got: Arc<Mutex<Vec<ContentLogLine>>> = Default::default();
    let stop = Arc::new(AtomicBool::new(false));
    let h = {
        let (got, stop) = (got.clone(), stop.clone());
        std::thread::spawn(move || {
            core().content_log_tail("unused".into(), None, since, &stop, |l| {
                got.lock().unwrap().push(l)
            })
        })
    };
    let t0 = Instant::now();
    while got.lock().unwrap().len() < 2 && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    // SAFETY: as above.
    unsafe { std::env::remove_var("BEACON_CONTENT_LOG_DIR") };
    let got = got.lock().unwrap();
    assert_eq!(
        got.len(),
        2,
        "empty and incomplete lines are not sent: {got:?}"
    );
    assert_eq!(got[0].level, "error");
    assert_eq!(got[0].text, "12:00:00[Scripting][error]-boom");
    assert!(got[1].text.len() <= MAX_TEXT && got[1].level == "info");
    assert!(
        got.iter()
            .all(|l| !l.text.contains('\u{1b}') && !l.text.contains('\r'))
    );
}

fn content_log_follows_a_running_game_then_a_new_start_and_survives_a_huge_line() {
    use std::io::Write;
    let logs = tempfile::tempdir().unwrap();
    // SAFETY: tests run one after the other; no other thread reads the environment.
    unsafe { std::env::set_var("BEACON_CONTENT_LOG_DIR", logs.path()) };
    // the game is already running: its log exists, with older lines, before Play
    let old = logs.path().join("ContentLog2026-10-19_09-00-00_1.txt");
    std::fs::write(
        &old,
        "09:00:00[Scripting][inform]-before play
",
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let since = SystemTime::now();
    let got: Arc<Mutex<Vec<ContentLogLine>>> = Default::default();
    let stop = Arc::new(AtomicBool::new(false));
    let h = {
        let (got, stop) = (got.clone(), stop.clone());
        std::thread::spawn(move || {
            core().content_log_tail("unused".into(), None, since, &stop, |l| {
                got.lock().unwrap().push(l)
            })
        })
    };
    let wait = |n: usize| {
        let t0 = Instant::now();
        while got.lock().unwrap().len() < n && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    std::thread::sleep(Duration::from_millis(700)); // the tail has taken its baseline
    let mut f = std::fs::OpenOptions::new().append(true).open(&old).unwrap();
    writeln!(f, "09:00:05[Scripting][warning]-while running").unwrap();
    wait(1);
    // the game restarts: a newer log, read from its first line
    std::fs::write(
        logs.path().join("ContentLog2026-10-19_09-10-00_1.txt"),
        "09:10:00[Scripting][inform]-restarted
",
    )
    .unwrap();
    wait(2);
    // a line far over the chunk size with no newline does not stall the tail
    let new = logs.path().join("ContentLog2026-10-19_09-10-00_1.txt");
    let mut f = std::fs::OpenOptions::new().append(true).open(&new).unwrap();
    f.write_all("z".repeat(1 << 21).as_bytes()).unwrap();
    writeln!(f).unwrap();
    writeln!(f, "09:10:09[Scripting][inform]-after the huge line").unwrap();
    let t0 = Instant::now();
    while !got
        .lock()
        .unwrap()
        .last()
        .is_some_and(|l| l.text.contains("after the huge"))
    {
        assert!(t0.elapsed() < Duration::from_secs(10), "tail stalled");
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.store(true, Ordering::Relaxed);
    h.join().unwrap();
    // SAFETY: as above.
    unsafe { std::env::remove_var("BEACON_CONTENT_LOG_DIR") };
    let got = got.lock().unwrap();
    assert_eq!(
        got[0].text,
        "09:00:05[Scripting][warning]-while running",
        "{:?}",
        &got[..2]
    );
    assert_eq!(got[1].text, "09:10:00[Scripting][inform]-restarted");
    assert!(got.iter().all(|l| l.text.len() <= MAX_TEXT));
    assert!(got.iter().all(|l| !l.text.contains("before play")));
}

/// `app_play --manual [--target-edition ...]`: Play a scratch project against the real
/// com.mojang (and launch the game), print what happened, then delete exactly the entries the
/// run added to the dev folders. Not part of the suite.
fn manual() -> i32 {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: single-threaded here; nothing has started yet.
    unsafe { std::env::set_var("BEACON_HOME", home.path()) };
    let before: std::collections::BTreeSet<_> = real_entries();
    let t = tempfile::tempdir().unwrap();
    let dir = t.path().join("scratch");
    create(&dir);
    let t0 = SystemTime::now();
    let r = core().play(dir.clone(), None);
    println!("{r:#?}");
    let after = real_entries();
    let added: Vec<_> = after.difference(&before).cloned().collect();
    println!("added: {added:#?}");
    // a short look at the content log tail (the game may not write one: setting off)
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            std::thread::sleep(Duration::from_secs(20));
            stop.store(true, Ordering::Relaxed)
        });
        core().content_log_tail(dir.clone(), None, t0, &stop, |l| println!("log: {l:?}"));
    });
    if std::env::args().any(|a| a == "--keep") {
        return 0;
    }
    for p in added {
        let _ = std::fs::remove_dir_all(&p);
    }
    println!("removed what this run added");
    0
}

fn real_entries() -> std::collections::BTreeSet<std::path::PathBuf> {
    let mut v = std::collections::BTreeSet::new();
    for i in beacon_engine::setup::detect() {
        for d in [
            "development_behavior_packs",
            "development_resource_packs",
            "minecraftWorlds",
        ] {
            for e in std::fs::read_dir(i.com_mojang.join(d))
                .into_iter()
                .flatten()
                .flatten()
            {
                v.insert(e.path());
            }
        }
    }
    v
}
