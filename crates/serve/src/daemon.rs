//! The daemon process (`beacon serve`, started detached by the shim or by hand).
//!
//! Lifecycle:
//! 1. take the exclusive lock (`discovery::lock_path`, `File::try_lock`, held until exit),
//!    else exit `ServeUnavailable` (another daemon is live); create the pipe/socket; write the
//!    discovery file atomically (a leftover one is stale: its lock was free);
//! 2. one engine thread owns all state ([`EngineState`]): up to two `Session`s through the
//!    [`SessionFactory`] (the restricted one at start, for the watcher's ignore list; the
//!    trusted one on first use, dropped when the last trusted client leaves) and the
//!    watcher's state.
//!    Channels reach it only through [`EngineHandle::call`] (closure dispatch: the LSP and MCP
//!    channels add features in their own files, never here). The file watcher (the
//!    `beacon watch` coalescing, [`crate::watch`]) feeds the sessions and re-checks; it never
//!    emits (only a `build` / `deploy` / `pack` request does);
//! 3. one thread per connection: handshake, then `lsp::serve` / `mcp::serve` / control;
//! 4. idle: no LSP/MCP client for `idle` (60 s; `--idle` / `BEACON_SERVE_IDLE`) -> remove the
//!    discovery file, release the lock, return. Before the first client the window is at least
//!    [`crate::shim::START_TIMEOUT`], so the shim that started us can attach.
//!
//! Idle memory: a `Session` keeps its JS host (and its QuickJS runtime) for its lifetime, and
//! the trusted session lives while a trusted client is attached; the bundler and file contents
//! are dropped after each build (12: `serve` idle RSS < 100 MB, LSP + MCP + symbols + API
//! index).

use crate::discovery::{self, Discovery};
use crate::transport::{self, Listener, Stream};
use crate::{
    Channel, ControlRequest, ENGINE_VERSION, Grant, Hello, HelloError, HelloReply,
    PROTOCOL_VERSION, read_line, write_line,
};
use beacon_engine::build::{BuildOutcome, Session};
use beacon_engine::config::CONFIG_FILE;
use beacon_engine::diag::{Code, Diagnostic, exit};
use beacon_engine::{sarif, trust};
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Exit after this long with no client attached.
pub const IDLE_SHUTDOWN: Duration = Duration::from_secs(60);

/// Builds a session for the project; `true` = trusted. The CLI wires the QuickJS host and
/// the esbuild bundler in (this crate depends on neither).
pub type SessionFactory = Box<dyn Fn(&Path, bool) -> Session + Send + Sync>;

#[derive(Debug, Clone)]
pub struct DaemonOptions {
    pub project: PathBuf,
    pub idle: Duration,
    /// Per-user runtime directory ([`discovery::run_dir`]).
    pub run: PathBuf,
}

impl DaemonOptions {
    /// Defaults: [`discovery::run_dir`], idle from `BEACON_SERVE_IDLE` (seconds) else
    /// [`IDLE_SHUTDOWN`]. `None` without a per-user directory.
    pub fn new(project: &Path) -> Option<DaemonOptions> {
        let idle = std::env::var("BEACON_SERVE_IDLE")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .map_or(IDLE_SHUTDOWN, Duration::from_secs);
        Some(DaemonOptions {
            project: project.into(),
            idle,
            run: discovery::run_dir()?,
        })
    }
}

/// Everything the engine thread owns. Reached only through [`EngineHandle::call`].
pub struct EngineState {
    root: PathBuf,
    factory: SessionFactory,
    /// [restricted, trusted], created on first use.
    sessions: [Option<Session>; 2],
    /// Attached LSP/MCP clients whose connection is trusted.
    trusted_clients: usize,
    /// The watcher's latest check (of [`EngineState::watch_trusted`]'s session).
    pub last_check: Option<BuildOutcome>,
}

impl EngineState {
    /// Canonical project root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The restricted or the trusted session, created on first use. Callers pass their
    /// connection's [`EngineHandle::trusted`], never a client-supplied value.
    pub fn session(&mut self, trusted: bool) -> &mut Session {
        let (root, f) = (&self.root, &self.factory);
        self.sessions[trusted as usize].get_or_insert_with(|| f(root, trusted))
    }

    /// The watcher re-checks the trusted session iff a trusted client is attached.
    pub fn watch_trusted(&self) -> bool {
        self.trusted_clients > 0
    }

    fn attach(&mut self, trusted: bool, on: bool) {
        if !trusted {
            return;
        }
        if on {
            self.trusted_clients += 1;
        } else {
            self.trusted_clients -= 1;
            if self.trusted_clients == 0 {
                self.sessions[1] = None; // back to restricted; frees the JS runtime
            }
        }
    }

    fn on_changes(&mut self, b: &crate::watch::Batch) {
        for s in self.sessions.iter_mut().flatten() {
            b.apply(s);
        }
        let t = self.watch_trusted();
        self.last_check = Some(self.session(t).check());
    }
}

type Job = Box<dyn FnOnce(&mut EngineState) + Send>;

/// One connection's way into the engine thread, with that connection's trust and grants.
#[derive(Clone)]
pub struct EngineHandle {
    tx: Sender<Job>,
    trusted: bool,
    grants: Vec<Grant>,
}

impl EngineHandle {
    /// Runs `f` on the engine thread and waits for its result. A panic inside `f` is
    /// contained there (the engine thread lives on) and re-raised here.
    pub fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut EngineState) -> R + Send + 'static,
    ) -> R {
        let (rtx, rrx) = mpsc::sync_channel(1);
        let job: Job = Box::new(move |st| {
            let _ = rtx.send(f(st));
        });
        self.tx.send(job).expect("engine thread gone");
        rrx.recv().expect("engine call panicked")
    }

    /// Trusted connection: the stored per-user decision or the shim's own `--trust` (5.5).
    pub fn trusted(&self) -> bool {
        self.trusted
    }

    /// The operator's `--allow` grants for this connection (8.1).
    pub fn grants(&self) -> &[Grant] {
        &self.grants
    }

    pub fn allows(&self, g: Grant) -> bool {
        self.grants.contains(&g)
    }
}

fn engine(root: PathBuf, factory: SessionFactory) -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::spawn(move || {
        let mut st = EngineState {
            root,
            factory,
            sessions: [None, None],
            trusted_clients: 0,
            last_check: None,
        };
        for job in rx {
            // the caller sees the panic (its reply channel drops); state stays usable
            let _ = catch_unwind(AssertUnwindSafe(|| job(&mut st)));
        }
    });
    tx
}

/// Connection registry: attached LSP/MCP clients, stop requests, idle clock.
struct Reg {
    clients: usize,
    /// No new client may attach (a stop was accepted or idle shutdown began).
    closing: bool,
    /// Leave the main loop now.
    stop: bool,
    /// Last time `clients` dropped to 0 (`None`: never had one).
    idle_since: Option<Instant>,
}

struct Shared {
    reg: Mutex<Reg>,
    cv: Condvar,
    root: PathBuf,
    started: Instant,
}

impl Shared {
    fn reg(&self) -> std::sync::MutexGuard<'_, Reg> {
        self.reg.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn diag(code: Code, msg: &str) -> String {
    sarif::to_text(&[Diagnostic::new(code, msg)])
}

/// Runs until idle shutdown or `stop`. Returns a process exit code (`diag::exit`); the
/// caller exits the process (connection and watcher threads are not joined).
pub fn run(opts: &DaemonOptions, factory: SessionFactory) -> i32 {
    let root = discovery::canonical_root(&opts.project);
    let key = discovery::project_key(&root);
    let fail = |m: String| {
        eprint!("{}", diag(Code::ServeUnavailable, &m));
        exit::DIAGNOSTICS
    };
    if let Err(e) = transport::secure_dir(&opts.run) {
        return fail(format!("run directory {}: {e}", opts.run.display()));
    }
    let lock = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(discovery::lock_path(&opts.run, &key))
    {
        Ok(f) => f,
        Err(e) => return fail(format!("lock file: {e}")),
    };
    if lock.try_lock().is_err() {
        return fail(format!(
            "another `beacon serve` already serves {}",
            root.display()
        ));
    }
    let (mut listener, pipe) = match Listener::bind(&opts.run, &key) {
        Ok(l) => l,
        Err(e) => return fail(format!("cannot listen: {e}")),
    };
    let disc_path = discovery::discovery_path(&opts.run, &key);
    let disc = Discovery {
        pipe: pipe.clone(),
        pid: std::process::id(),
        engine: ENGINE_VERSION.into(),
        protocol: PROTOCOL_VERSION,
        project: root.to_string_lossy().into_owned(),
        port: None,
        token: None,
    };
    if let Err(e) = disc.write(&disc_path) {
        transport::cleanup(&pipe);
        return fail(format!("discovery file: {e}"));
    }

    let tx = engine(root.clone(), factory);
    watcher(&root, tx.clone());
    let shared = Arc::new(Shared {
        reg: Mutex::new(Reg {
            clients: 0,
            closing: false,
            stop: false,
            idle_since: None,
        }),
        cv: Condvar::new(),
        root: root.clone(),
        started: Instant::now(),
    });
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok(s) => {
                        let (shared, tx) = (shared.clone(), tx.clone());
                        std::thread::spawn(move || connection(s, &shared, tx));
                    }
                    // a client that vanished mid-connect; never spin hot
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
    }

    let first = opts.idle.max(crate::shim::START_TIMEOUT);
    let mut reg = shared.reg();
    loop {
        if reg.stop {
            break;
        }
        if reg.clients > 0 {
            reg = shared.cv.wait(reg).unwrap_or_else(|e| e.into_inner());
            continue;
        }
        let deadline = match reg.idle_since {
            Some(t) => t + opts.idle,
            None => shared.started + first,
        };
        let now = Instant::now();
        if now >= deadline {
            reg.closing = true; // a client mid-handshake is refused, not dropped silently
            break;
        }
        reg = shared
            .cv
            .wait_timeout(reg, deadline - now)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    drop(reg);
    // ponytail: the acceptor stays blocked in accept(); the caller exits the process. Order
    // matters: our files go while we still hold the lock, so a successor's survive.
    if Discovery::read(&disc_path).is_some_and(|d| d.pid == std::process::id()) {
        let _ = std::fs::remove_file(&disc_path);
    }
    transport::cleanup(&pipe);
    drop(lock);
    exit::OK
}

/// Feeds file changes to the sessions and re-checks, on the engine thread.
fn watcher(root: &Path, tx: Sender<Job>) {
    let w = match crate::watch::Watch::start(root) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("note: cannot watch {}: {e}", root.display());
            return;
        }
    };
    let h = EngineHandle {
        tx,
        trusted: false,
        grants: vec![],
    };
    std::thread::spawn(move || {
        let ignored = |h: &EngineHandle| {
            h.call(|st| {
                let t = st.watch_trusted();
                st.session(t).output_dirs()
            })
        };
        let mut ign = ignored(&h);
        while let Some(b) = w.next(&ign) {
            let cfg = b.touches_config(&h.call(|st| st.root().to_path_buf()));
            h.call(move |st| st.on_changes(&b));
            if cfg {
                ign = ignored(&h); // pack folders may have moved
            }
        }
    });
}

fn plugins(root: &Path) -> Vec<serde_json::Value> {
    let cfg: serde_json::Value = std::fs::read(root.join(CONFIG_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    cfg["beacon"]["plugins"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn reply(ok: bool, error: Option<HelloError>) -> HelloReply {
    HelloReply {
        ok,
        engine: ENGINE_VERSION.into(),
        protocol: PROTOCOL_VERSION,
        error,
    }
}

fn hello_error(code: Code, message: String) -> Option<HelloError> {
    Some(HelloError {
        code: code.id().into(),
        message,
    })
}

fn connection(mut s: Stream, shared: &Shared, tx: Sender<Job>) {
    let Ok(Some(line)) = read_line(&mut s) else {
        return;
    };
    let hello: Hello = match serde_json::from_str(&line) {
        Ok(h) => h,
        Err(e) => {
            let m = format!("bad hello: {e}");
            let _ = write_line(
                &mut s,
                &reply(false, hello_error(Code::ServeUnavailable, m)),
            );
            return;
        }
    };
    // Control speaks across versions: a newer shim must be able to stop an older daemon.
    if hello.channel == Channel::Control {
        if write_line(&mut s, &reply(true, None)).is_ok() {
            control(s, shared);
        }
        return;
    }
    if hello.beacon != PROTOCOL_VERSION || hello.engine != ENGINE_VERSION {
        let m = format!(
            "daemon runs engine {ENGINE_VERSION} protocol {PROTOCOL_VERSION}, client {} protocol {}",
            hello.engine, hello.beacon
        );
        let _ = write_line(
            &mut s,
            &reply(false, hello_error(Code::ServeVersionMismatch, m)),
        );
        return;
    }
    // Trust per connection: the shim's own `--trust` (the operator's command line; the pipe
    // only admits this user) or the stored decision. Never from channel messages.
    let trusted = hello.trust || trust::is_trusted(&shared.root, &plugins(&shared.root));
    {
        let mut r = shared.reg();
        if r.closing {
            drop(r);
            let m = "the daemon is shutting down".to_string();
            let _ = write_line(
                &mut s,
                &reply(false, hello_error(Code::ServeUnavailable, m)),
            );
            return;
        }
        r.clients += 1;
    }
    let handle = EngineHandle {
        tx,
        trusted,
        grants: hello.allow,
    };
    handle.call(move |st| st.attach(trusted, true));
    // detach even if the channel panics
    struct Detach<'a>(&'a Shared, EngineHandle);
    impl Drop for Detach<'_> {
        fn drop(&mut self) {
            let t = self.1.trusted;
            self.1.call(move |st| st.attach(t, false));
            let mut r = self.0.reg();
            r.clients -= 1;
            if r.clients == 0 {
                r.idle_since = Some(Instant::now());
            }
            self.0.cv.notify_all();
        }
    }
    let _d = Detach(shared, handle.clone());
    if write_line(&mut s, &reply(true, None)).is_err() {
        return;
    }
    match hello.channel {
        Channel::Lsp => crate::lsp::serve(s, handle),
        Channel::Mcp => crate::mcp::serve(s, handle),
        Channel::Control => unreachable!(),
    }
}

/// Process memory in bytes: private usage on Windows, RSS elsewhere (`None` if unknown).
fn rss() -> Option<u64> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: plain out-struct call on the current process handle.
        unsafe {
            let mut c: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
            c.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
            let ok = GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&mut c as *mut PROCESS_MEMORY_COUNTERS_EX).cast(),
                c.cb,
            );
            (ok != 0).then_some(c.PrivateUsage as u64)
        }
    }
    #[cfg(not(windows))]
    {
        let pid = std::process::id().to_string();
        std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok()?.trim().parse::<u64>().ok())
            .map(|k| k * 1024)
    }
}

fn control(mut s: Stream, shared: &Shared) {
    while let Ok(Some(line)) = read_line(&mut s) {
        let r = match serde_json::from_str::<ControlRequest>(&line) {
            Ok(ControlRequest::Status) => json!({
                "pid": std::process::id(),
                "engine": ENGINE_VERSION,
                "protocol": PROTOCOL_VERSION,
                "clients": shared.reg().clients,
                "uptimeMs": shared.started.elapsed().as_millis() as u64,
                "rssBytes": rss(),
            }),
            Ok(ControlRequest::Stop { force }) => {
                let clients = {
                    let mut r = shared.reg();
                    r.closing |= r.clients == 0 || force;
                    r.clients
                };
                if clients == 0 || force {
                    // answer first, then let the main loop exit the process
                    let _ = write_line(&mut s, &json!({ "ok": true }));
                    shared.reg().stop = true;
                    shared.cv.notify_all();
                    return;
                }
                json!({ "ok": false, "clients": clients })
            }
            Err(e) => json!({ "ok": false, "error": format!("bad control request: {e}") }),
        };
        if write_line(&mut s, &r).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_engine::build::BuildOptions;
    use beacon_engine::host::{GenResult, GeneratorHost};

    struct NoHost;
    impl GeneratorHost for NoHost {
        fn run(
            &self,
            _: &beacon_engine::model::Generator,
            _: &beacon_engine::model::GenCtx,
            _: &dyn beacon_engine::path::Vfs,
        ) -> GenResult {
            GenResult {
                items: vec![],
                inputs: Default::default(),
                diagnostics: vec![],
            }
        }
        fn version(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn watcher_trust_follows_trusted_clients() {
        let tx = engine(
            PathBuf::from("/nowhere"),
            Box::new(|root, trusted| {
                let o = BuildOptions {
                    trusted,
                    ..Default::default()
                };
                Session::new(root, o, Box::new(NoHost), None)
            }),
        );
        let h = |trusted| EngineHandle {
            tx: tx.clone(),
            trusted,
            grants: vec![Grant::Edit],
        };
        let (a, b, r) = (h(true), h(true), h(false));
        assert!(a.allows(Grant::Edit) && !a.allows(Grant::Deploy));
        r.call(|st| st.attach(false, true));
        assert!(!r.call(|st| st.watch_trusted()));
        a.call(|st| st.attach(true, true));
        b.call(|st| st.attach(true, true));
        a.call(|st| {
            st.session(true);
        });
        a.call(|st| st.attach(true, false));
        assert!(a.call(|st| st.watch_trusted() && st.sessions[1].is_some()));
        // the last trusted client leaves: restricted again, trusted session dropped
        b.call(|st| st.attach(true, false));
        assert!(b.call(|st| !st.watch_trusted() && st.sessions[1].is_none()));
        // a panic in a job reaches the caller; the engine thread lives on
        let p = std::panic::catch_unwind(AssertUnwindSafe(|| r.call(|_| panic!("boom"))));
        assert!(p.is_err());
        assert_eq!(
            r.call(|st| st.root().to_path_buf()),
            PathBuf::from("/nowhere")
        );
    }
}
