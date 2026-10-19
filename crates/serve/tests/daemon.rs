//! P4-1B: the `beacon serve` daemon, the stdio shim and the transport, end to end.
//!
//! `harness = false`: this binary is also the daemon (`daemon <project>`) and the shim
//! (`shim <project>`, MCP channel) it spawns, so the tests drive real processes with real
//! stdio, detached starts and the real pipe/socket. Tests run one after the other.

use beacon_engine::build::{BuildOptions, Session};
use beacon_engine::host::{GenResult, GeneratorHost};
use beacon_engine::model::{GenCtx, Generator};
use beacon_engine::path::Vfs;
use beacon_serve::discovery::{self, Discovery};
use beacon_serve::{
    Channel, ControlRequest, ENGINE_VERSION, Hello, HelloError, HelloReply, PROTOCOL_VERSION,
    daemon, shim, transport,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

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
        Box::new(|root, o: BuildOptions| Session::new(root, o, Box::new(NoHost), None)),
    )
}

fn shim_main(project: &Path) -> i32 {
    let exe = std::env::current_exe().unwrap();
    shim::run(&shim::ShimOptions {
        project: project.into(),
        channel: Channel::Mcp,
        trust: false,
        allow: vec![],
        run: discovery::run_dir().expect("run dir"),
        daemon: vec![exe.into(), "daemon".into(), project.into()],
    })
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|a| a.to_str()) {
        Some("daemon") => std::process::exit(daemon_main(Path::new(&args[1]))),
        Some("shim") => std::process::exit(shim_main(Path::new(&args[1]))),
        _ => {}
    }
    let filter = args
        .iter()
        .filter_map(|a| a.to_str())
        .find(|a| !a.starts_with('-'));
    let tests: &[(&str, fn())] = &[
        ("second_daemon_is_refused", second_daemon_is_refused),
        ("stale_discovery_is_recovered", stale_discovery_is_recovered),
        ("version_mismatch_daemon_side", version_mismatch_daemon_side),
        (
            "version_mismatch_shim_restarts_when_alone",
            version_mismatch_shim_restarts_when_alone,
        ),
        (
            "version_mismatch_shim_fails_with_clients",
            version_mismatch_shim_fails_with_clients,
        ),
        (
            "idle_zero_exits_after_last_client",
            idle_zero_exits_after_last_client,
        ),
        (
            "two_shims_meet_the_same_daemon",
            two_shims_meet_the_same_daemon,
        ),
        (
            "stored_trust_is_demoted_when_plugins_change",
            stored_trust_is_demoted_when_plugins_change,
        ),
        #[cfg(windows)]
        ("pipe_dacl_is_owner_only", pipe_dacl_is_owner_only),
        #[cfg(unix)]
        ("run_dir_is_private", run_dir_is_private),
    ];
    let mut failed = vec![];
    for (name, t) in tests {
        if filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let t0 = Instant::now();
        let ok = std::panic::catch_unwind(t).is_ok();
        println!(
            "test {name} ... {} ({} ms)",
            if ok { "ok" } else { "FAILED" },
            t0.elapsed().as_millis()
        );
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

// ---- helpers ----

struct Fx {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("config.json"), "{}").unwrap();
        Fx {
            home: tempfile::tempdir().unwrap(),
            project,
        }
    }
    fn run(&self) -> PathBuf {
        self.home.path().join("run")
    }
    fn p(&self) -> &Path {
        self.project.path()
    }
    fn cmd(&self, idle: u64) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.env("BEACON_HOME", self.home.path())
            .env("BEACON_SERVE_IDLE", idle.to_string());
        c
    }
    fn daemon(&self, idle: u64) -> Child {
        self.cmd(idle)
            .arg("daemon")
            .arg(self.p())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }
    fn shim(&self, spelling: &Path, idle: u64) -> Shim {
        let mut child = self
            .cmd(idle)
            .arg("shim")
            .arg(spelling)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        Shim {
            stdin: child.stdin.take(),
            child,
            rx,
        }
    }
    fn discovery(&self) -> Option<Discovery> {
        let key = discovery::project_key(self.p());
        let b = std::fs::read(discovery::discovery_path(&self.run(), &key)).ok()?;
        serde_json::from_slice(&b).ok()
    }
    fn control(&self, req: ControlRequest) -> std::io::Result<Value> {
        shim::control(&self.run(), self.p(), &req)
    }
    /// A raw client: connects to the daemon in the discovery file and says `hello`.
    fn hello(&self, hello: &Hello) -> (transport::Stream, HelloReply) {
        let d = self.discovery().expect("discovery");
        let mut s = transport::connect(&d.pipe).unwrap();
        let r = shim::handshake(&mut s, hello).unwrap();
        (s, r)
    }
    fn wait_daemon_gone(&self) {
        wait_until("daemon gone", || {
            self.discovery().is_none() && lock_free(&self.run(), self.p())
        });
        std::thread::sleep(Duration::from_millis(200)); // process exit after the unlock
    }
}

struct Shim {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
}

impl Shim {
    /// One MCP request through the shim; the daemon's (stub) answer.
    fn request(&mut self, id: u32) -> Value {
        let s = self.stdin.as_mut().unwrap();
        writeln!(s, r#"{{"jsonrpc":"2.0","id":{id},"method":"ping"}}"#).unwrap();
        s.flush().unwrap();
        let l = self
            .rx
            .recv_timeout(Duration::from_secs(20))
            .expect("answer through the shim");
        serde_json::from_str(&l).unwrap()
    }
    fn close(mut self) -> i32 {
        drop(self.stdin.take());
        wait_exit(&mut self.child)
    }
}

fn lock_free(run: &Path, project: &Path) -> bool {
    let key = discovery::project_key(project);
    let Ok(f) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(discovery::lock_path(run, &key))
    else {
        return false;
    };
    f.try_lock().is_ok()
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_exit(c: &mut Child) -> i32 {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(s) = c.try_wait().unwrap() {
            return s.code().unwrap_or(-1);
        }
        if Instant::now() > end {
            let _ = c.kill();
            panic!("process did not exit");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn stderr(c: &mut Child) -> String {
    let mut s = String::new();
    std::io::Read::read_to_string(c.stderr.as_mut().unwrap(), &mut s).unwrap();
    s
}

fn good_hello(channel: Channel) -> Hello {
    Hello {
        beacon: PROTOCOL_VERSION,
        engine: ENGINE_VERSION.into(),
        channel,
        trust: false,
        allow: vec![],
    }
}

fn stop(fx: &Fx) {
    let r = fx.control(ControlRequest::Stop { force: true }).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    fx.wait_daemon_gone();
}

/// Plays an older daemon: holds the lock, serves a pipe, answers every LSP/MCP hello with a
/// version mismatch; a control `stop` is accepted only when `alone`.
fn fake_old_daemon(fx: &Fx, alone: bool) -> std::thread::JoinHandle<()> {
    let run = fx.run();
    std::fs::create_dir_all(&run).unwrap();
    let root = discovery::canonical_root(fx.p());
    let key = discovery::project_key(&root);
    let lock = std::fs::File::create(discovery::lock_path(&run, &key)).unwrap();
    lock.try_lock().unwrap();
    let (mut l, pipe) = transport::Listener::bind(&run, &key).unwrap();
    let disc = discovery::discovery_path(&run, &key);
    let d = Discovery {
        pipe: pipe.clone(),
        pid: std::process::id(),
        engine: "0.0.0-old".into(),
        protocol: PROTOCOL_VERSION,
        project: root.to_string_lossy().into_owned(),
        port: None,
        token: None,
    };
    std::fs::write(&disc, serde_json::to_vec(&d).unwrap()).unwrap();
    std::thread::spawn(move || {
        let _lock = lock;
        loop {
            let mut s = l.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let h: Hello = serde_json::from_str(&line).unwrap();
            let mut reply = HelloReply {
                ok: true,
                engine: "0.0.0-old".into(),
                protocol: PROTOCOL_VERSION,
                error: None,
            };
            if h.channel != Channel::Control {
                reply.ok = false;
                reply.error = Some(HelloError {
                    code: "BCN0160".into(),
                    message: "old".into(),
                });
                writeln!(s, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
                continue;
            }
            writeln!(s, "{}", serde_json::to_string(&reply).unwrap()).unwrap();
            line.clear();
            r.read_line(&mut line).unwrap();
            assert_eq!(
                serde_json::from_str::<ControlRequest>(&line).unwrap(),
                ControlRequest::Stop { force: false }
            );
            if alone {
                std::fs::remove_file(&disc).unwrap();
                transport::cleanup(&pipe);
                writeln!(s, "{}", json!({"ok": true})).unwrap();
                return; // drops the listener and the lock
            }
            writeln!(s, "{}", json!({"ok": false, "clients": 1})).unwrap();
        }
    })
}

// ---- tests ----

fn second_daemon_is_refused() {
    let fx = Fx::new();
    let mut a = fx.daemon(60);
    wait_until("discovery", || fx.discovery().is_some());
    let mut b = fx.daemon(60);
    assert_ne!(wait_exit(&mut b), 0);
    assert!(stderr(&mut b).contains("BCN0161"));
    assert_eq!(fx.discovery().unwrap().pid, a.id());
    stop(&fx);
    assert_eq!(wait_exit(&mut a), 0);
}

fn stale_discovery_is_recovered() {
    let fx = Fx::new();
    let run = fx.run();
    std::fs::create_dir_all(&run).unwrap();
    let root = discovery::canonical_root(fx.p());
    let key = discovery::project_key(&root);
    let dead = Discovery {
        pipe: if cfg!(windows) {
            r"\\.\pipe\beacon-dead-0000".into()
        } else {
            run.join("dead.sock").to_string_lossy().into_owned()
        },
        pid: 999_999,
        engine: ENGINE_VERSION.into(),
        protocol: PROTOCOL_VERSION,
        project: root.to_string_lossy().into_owned(),
        port: None,
        token: None,
    };
    std::fs::write(
        discovery::discovery_path(&run, &key),
        serde_json::to_vec(&dead).unwrap(),
    )
    .unwrap();
    let mut s = fx.shim(fx.p(), 0);
    let r = s.request(7);
    assert_eq!(r["id"], 7, "{r}");
    let d = fx.discovery().unwrap();
    assert_ne!(d.pid, 999_999);
    assert_ne!(d.pipe, dead.pipe);
    assert_eq!(s.close(), 0);
    fx.wait_daemon_gone();
}

fn version_mismatch_daemon_side() {
    let fx = Fx::new();
    let mut d = fx.daemon(60);
    wait_until("discovery", || fx.discovery().is_some());
    let mut old = good_hello(Channel::Mcp);
    old.engine = "0.0.0-other".into();
    let (_s, r) = fx.hello(&old);
    assert!(!r.ok);
    assert_eq!(r.error.unwrap().code, "BCN0160");
    // control speaks across versions
    let mut c = good_hello(Channel::Control);
    c.engine = "0.0.0-other".into();
    assert!(fx.hello(&c).1.ok);
    // a client attached: stop is refused
    let (_lsp, r) = fx.hello(&good_hello(Channel::Lsp));
    assert!(r.ok);
    wait_until("attached", || {
        fx.control(ControlRequest::Status).unwrap()["clients"] == 1
    });
    let r = fx.control(ControlRequest::Stop { force: false }).unwrap();
    assert_eq!(
        (r["ok"].as_bool(), r["clients"].as_u64()),
        (Some(false), Some(1))
    );
    drop(_lsp);
    wait_until("detached", || {
        fx.control(ControlRequest::Status).unwrap()["clients"] == 0
    });
    // alone: stop is accepted
    let r = fx.control(ControlRequest::Stop { force: false }).unwrap();
    assert_eq!(r["ok"], true);
    assert_eq!(wait_exit(&mut d), 0);
    fx.wait_daemon_gone();
}

fn version_mismatch_shim_restarts_when_alone() {
    let fx = Fx::new();
    let fake = fake_old_daemon(&fx, true);
    let mut s = fx.shim(fx.p(), 0);
    assert_eq!(s.request(1)["id"], 1);
    fake.join().unwrap();
    assert_eq!(fx.discovery().unwrap().engine, ENGINE_VERSION);
    assert_eq!(s.close(), 0);
    fx.wait_daemon_gone();
}

fn version_mismatch_shim_fails_with_clients() {
    let fx = Fx::new();
    let _fake = fake_old_daemon(&fx, false);
    let mut s = fx.shim(fx.p(), 0);
    drop(s.stdin.take());
    assert_ne!(wait_exit(&mut s.child), 0);
    assert!(stderr(&mut s.child).contains("BCN0160"));
    assert_eq!(fx.discovery().unwrap().engine, "0.0.0-old");
    // the fake keeps serving (detached thread); its lock dies with the test process
}

fn idle_zero_exits_after_last_client() {
    let fx = Fx::new();
    let mut d = fx.daemon(0);
    wait_until("discovery", || fx.discovery().is_some());
    let mut s = fx.shim(fx.p(), 0);
    assert_eq!(s.request(1)["id"], 1);
    assert!(d.try_wait().unwrap().is_none(), "alive while a client is");
    assert_eq!(s.close(), 0);
    assert_eq!(wait_exit(&mut d), 0);
    assert!(fx.discovery().is_none());
}

fn two_shims_meet_the_same_daemon() {
    let fx = Fx::new();
    // other spelling: `..` (and upper case on Windows)
    let p = fx.p().to_string_lossy().into_owned();
    let p = if cfg!(windows) { p.to_uppercase() } else { p };
    let other = Path::new(&p).join("sub").join("..");
    std::fs::create_dir_all(fx.p().join("sub")).unwrap();
    let mut a = fx.shim(fx.p(), 0);
    let mut b = fx.shim(&other, 0);
    assert_eq!(a.request(1)["id"], 1);
    assert_eq!(b.request(2)["id"], 2);
    let st = fx.control(ControlRequest::Status).unwrap();
    assert_eq!(st["clients"], 2, "{st}");
    assert_eq!(st["pid"], fx.discovery().unwrap().pid);
    let n = std::fs::read_dir(fx.run())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension() == Some("json".as_ref()))
        .count();
    assert_eq!(n, 1, "one discovery file");
    assert_eq!(a.close(), 0);
    assert_eq!(b.close(), 0);
    fx.wait_daemon_gone();
}

/// A connection trusted by the stored decision loses trust when `beacon.plugins` changes (the
/// watcher sees `config.json`); one trusted by the shim's `--trust` keeps it.
fn stored_trust_is_demoted_when_plugins_change() {
    let fx = Fx::new();
    let cfg = |plugins: &str| {
        std::fs::write(
            fx.p().join("config.json"),
            format!(r#"{{"beacon":{{"plugins":{plugins}}}}}"#),
        )
        .unwrap();
    };
    cfg("[]");
    // the daemon reads the store under the fixture's BEACON_HOME
    // SAFETY: tests run one after the other on the main thread; children get their own env.
    unsafe { std::env::set_var("BEACON_HOME", fx.home.path()) };
    let root = discovery::canonical_root(fx.p());
    beacon_engine::trust::trust(&root, &[]).unwrap();
    let mut d = fx.daemon(60);
    wait_until("discovery", || fx.discovery().is_some());
    let trusted = |n: u64| {
        wait_until("trusted clients", || {
            fx.control(ControlRequest::Status).unwrap()["trustedClients"] == n
        })
    };
    let (_stored, r) = fx.hello(&good_hello(Channel::Lsp));
    assert!(r.ok);
    let mut flag = good_hello(Channel::Mcp);
    flag.trust = true;
    let (_explicit, r) = fx.hello(&flag);
    assert!(r.ok);
    trusted(2);
    cfg(r#"[{"id":"changed"}]"#);
    trusted(1);
    drop(_explicit);
    trusted(0);
    drop(_stored);
    stop(&fx);
    assert_eq!(wait_exit(&mut d), 0);
}

#[cfg(windows)]
fn pipe_dacl_is_owner_only() {
    let fx = Fx::new();
    let mut d = fx.daemon(60);
    wait_until("discovery", || fx.discovery().is_some());
    let s = transport::connect(&fx.discovery().unwrap().pipe).unwrap();
    let aces = transport::dacl_aces(&s).unwrap();
    let me = transport::current_user_sid().unwrap();
    assert!(me.starts_with("S-1-"), "{me}");
    assert_eq!(
        aces,
        [(true, me)],
        "one allow ACE, for the current user only"
    );
    drop(s);
    stop(&fx);
    assert_eq!(wait_exit(&mut d), 0);
}

#[cfg(unix)]
fn run_dir_is_private() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let fx = Fx::new();
    let run = fx.run();
    transport::secure_dir(&run).unwrap();
    let m = std::fs::symlink_metadata(&run).unwrap();
    assert_eq!(m.permissions().mode() & 0o777, 0o700);
    assert_eq!(m.uid(), unsafe { libc_geteuid() });
    // a symlinked run dir is refused
    let elsewhere = fx.home.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let link = fx.home.path().join("link");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    assert!(transport::secure_dir(&link).is_err());
    // and so is a daemon whose run dir is one
    std::fs::remove_dir(&run).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &run).unwrap();
    let mut d = fx.daemon(0);
    assert_ne!(wait_exit(&mut d), 0);
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "geteuid"]
    fn libc_geteuid() -> u32;
}
