//! `beacon serve --stdio --lsp|--mcp [--trust] [--allow ...]`: what editors and MCP clients
//! launch. It finds the project's daemon through the discovery file (starting one detached
//! when missing or stale: [`ShimOptions::daemon`]; Windows `DETACHED_PROCESS`, Unix own
//! process group, stdio to null), connects, sends its [`crate::Hello`], then copies bytes
//! stdin -> pipe and pipe -> stdout until either side closes. It holds no engine state (a
//! few MB RSS). `beacon mcp` becomes this with `--mcp`.
//!
//! Version mismatch: the shim asks the daemon to stop over the control channel (which speaks
//! across versions). An idle daemon (no LSP/MCP client) stops and the shim starts its own;
//! otherwise the shim fails with `ServeVersionMismatch`.

use crate::discovery::{self, Discovery};
use crate::transport::{self, Stream};
use crate::{
    Channel, ControlRequest, ENGINE_VERSION, Grant, Hello, HelloReply, PROTOCOL_VERSION, read_line,
    write_line,
};
use beacon_engine::diag::{Code, Diagnostic, exit};
use beacon_engine::sarif;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Waiting for a freshly started daemon to accept the handshake.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);
/// A daemon that accepted the connection answers the handshake within this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct ShimOptions {
    pub project: PathBuf,
    pub channel: Channel,
    /// The operator's `--trust` (sent in the hello; the pipe only admits this user, who can
    /// already run `beacon build --trust`).
    pub trust: bool,
    /// The operator's `--allow` ([`Grant::parse_list`]).
    pub allow: Vec<Grant>,
    /// Per-user runtime directory ([`discovery::run_dir`]).
    pub run: PathBuf,
    /// Program + arguments that start the daemon for this project, e.g.
    /// `[<current exe>, "serve", "--project", <root>]`. Inherits the environment
    /// (`BEACON_HOME`, `BEACON_SERVE_IDLE`).
    pub daemon: Vec<OsString>,
}

/// Why the shim could not attach: `ServeUnavailable` or `ServeVersionMismatch`.
#[derive(Debug)]
pub struct ShimError {
    pub code: Code,
    pub message: String,
}

impl ShimError {
    fn new(code: Code, message: impl Into<String>) -> ShimError {
        ShimError {
            code,
            message: message.into(),
        }
    }
}

/// Runs `f` on a helper thread, giving up after `d` (the thread is abandoned).
fn timeout<T: Send + 'static>(d: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(d).ok()
}

/// Sends `hello`, reads the daemon's one-line reply. Bytes after the reply stay unread.
pub fn handshake(s: &mut Stream, hello: &Hello) -> io::Result<HelloReply> {
    write_line(s, hello)?;
    let mut r = s.try_clone()?;
    let line = timeout(HANDSHAKE_TIMEOUT, move || read_line(&mut r))
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "no handshake reply"))??
        .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

fn hello(channel: Channel, trust: bool, allow: Vec<Grant>) -> Hello {
    Hello {
        beacon: PROTOCOL_VERSION,
        engine: ENGINE_VERSION.into(),
        channel,
        trust,
        allow,
    }
}

/// The live daemon's endpoint for `root`, if the discovery file names one we can open.
fn open(run: &Path, root: &Path) -> Option<(Discovery, Stream)> {
    let d = Discovery::read(&discovery::discovery_path(
        run,
        &discovery::project_key(root),
    ))?;
    if !d.serves(root) {
        return None;
    }
    let s = transport::connect(&d.pipe).ok()?;
    Some((d, s))
}

/// One control request to the running daemon (never starts one). `NotFound` when none runs.
pub fn control(run: &Path, project: &Path, req: &ControlRequest) -> io::Result<serde_json::Value> {
    let root = discovery::canonical_root(project);
    let (_, mut s) = open(run, &root).ok_or(io::ErrorKind::NotFound)?;
    let r = handshake(&mut s, &hello(Channel::Control, false, vec![]))?;
    if !r.ok {
        return Err(io::Error::other(
            r.error.map_or_else(String::new, |e| e.message),
        ));
    }
    write_line(&mut s, req)?;
    let line = timeout(HANDSHAKE_TIMEOUT, move || read_line(&mut s))
        .ok_or(io::ErrorKind::TimedOut)??
        .ok_or(io::ErrorKind::UnexpectedEof)?;
    serde_json::from_str(&line).map_err(io::Error::other)
}

/// No daemon holds the project lock (probed by taking it for an instant).
fn lock_free(run: &Path, root: &Path) -> bool {
    let path = discovery::lock_path(run, &discovery::project_key(root));
    match std::fs::OpenOptions::new().write(true).open(path) {
        Ok(f) => f.try_lock().is_ok(),
        Err(_) => true, // never created: nobody ever served this project here
    }
}

fn spawn(cmd: &[OsString]) -> io::Result<Child> {
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| io::Error::other("no daemon command"))?;
    let mut c = Command::new(prog);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x8;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        // Our own stdio handles are inheritable (the editor's pipes): a daemon inheriting
        // them would keep the editor's pipes open after we exit.
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
        };
        for h in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: plain calls on our own std handles; failure only leaves inheritance on.
            unsafe { SetHandleInformation(GetStdHandle(h), HANDLE_FLAG_INHERIT, 0) };
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c.spawn()
}

/// Finds or starts the project's daemon and completes the handshake; the returned stream
/// speaks `opts.channel`.
pub fn connect(opts: &ShimOptions) -> Result<Stream, ShimError> {
    let root = discovery::canonical_root(&opts.project);
    let want = hello(opts.channel, opts.trust, opts.allow.clone());
    let deadline = Instant::now() + START_TIMEOUT;
    let mut child: Option<Child> = None;
    // the daemon of another version we stopped (its discovery file may linger a moment)
    let mut stopped: Option<u32> = None;
    let mut failed = 0;
    loop {
        if let Some((d, mut s)) = open(&opts.run, &root)
            && let Ok(r) = handshake(&mut s, &want)
        {
            if r.ok {
                return Ok(s);
            }
            let e = r.error.unwrap_or_else(|| crate::HelloError {
                code: Code::ServeUnavailable.id().into(),
                message: "refused".into(),
            });
            if e.code == Code::ServeVersionMismatch.id() && stopped != Some(d.pid) {
                // restart an idle daemon of another version, once
                if stopped.is_none()
                    && control(&opts.run, &root, &ControlRequest::Stop { force: false })
                        .is_ok_and(|v| v["ok"] == true)
                {
                    stopped = Some(d.pid);
                } else {
                    return Err(ShimError::new(
                        Code::ServeVersionMismatch,
                        format!(
                            "{} (other clients are attached; close them or run `beacon serve --stop`)",
                            e.message
                        ),
                    ));
                }
            }
            // else: shutting down; a successor is started below
        }
        // nothing usable: start a daemon (again, if ours lost the lock race and exited)
        let status = child.as_mut().map(|c| c.try_wait());
        let running = matches!(status, Some(Ok(None)));
        if let Some(Ok(Some(st))) = status
            && !st.success()
        {
            // one loss is the lock race with another shim's daemon; three is a broken start
            failed += 1;
            if failed >= 3 {
                return Err(ShimError::new(
                    Code::ServeUnavailable,
                    format!("the daemon exited at start ({st}); run it by hand to see why"),
                ));
            }
        }
        // a held lock is a daemon starting up or shutting down: wait for it, don't race it
        if !running && lock_free(&opts.run, &root) {
            child = Some(spawn(&opts.daemon).map_err(|e| {
                ShimError::new(
                    Code::ServeUnavailable,
                    format!("cannot start the daemon: {e}"),
                )
            })?);
        }
        if Instant::now() > deadline {
            return Err(ShimError::new(
                Code::ServeUnavailable,
                format!(
                    "no daemon answered for {} within {} s",
                    root.display(),
                    START_TIMEOUT.as_secs()
                ),
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// Copies until end of stream, flushing every chunk (LSP bodies are not line-terminated).
fn pump(mut from: impl Read, mut to: impl Write) -> io::Result<()> {
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        to.write_all(&buf[..n])?;
        to.flush()?;
    }
}

/// Relays stdio to the daemon. Exit codes: 0 when the client closed stdin; `diag::exit`
/// codes with a `ServeUnavailable` / `ServeVersionMismatch` message on stderr otherwise.
pub fn run(opts: &ShimOptions) -> i32 {
    let s = match connect(opts) {
        Ok(s) => s,
        Err(e) => {
            eprint!("{}", sarif::to_text(&[Diagnostic::new(e.code, e.message)]));
            return exit::DIAGNOSTICS;
        }
    };
    let up = match s.try_clone() {
        Ok(u) => u,
        Err(e) => {
            let m = format!("pipe: {e}");
            eprint!(
                "{}",
                sarif::to_text(&[Diagnostic::new(Code::ServeUnavailable, m)])
            );
            return exit::DIAGNOSTICS;
        }
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let t = tx.clone();
    std::thread::spawn(move || {
        let _ = pump(io::stdin().lock(), up);
        let _ = t.send(true); // the client is done
    });
    std::thread::spawn(move || {
        let _ = pump(s, io::stdout().lock());
        let _ = tx.send(false);
    });
    if rx.recv().unwrap_or(true) {
        exit::OK
    } else {
        let m = "the daemon closed the connection";
        eprint!(
            "{}",
            sarif::to_text(&[Diagnostic::new(Code::ServeUnavailable, m)])
        );
        exit::DIAGNOSTICS
    }
}
