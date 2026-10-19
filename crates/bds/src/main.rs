//! `beacon-bds` (8.5): `cargo bds smoke --world <w.mcworld>` runs a built world on a hash-pinned
//! Bedrock Dedicated Server and asserts its console output. Exit codes: 0 pass, 1 assertion
//! failure, 2 usage / EULA / download errors.
//!
//! Trust: the BDS zip is downloaded only from minecraft.net, only after explicit EULA acceptance
//! (logged), only with the pinned size + blake3 (`beacon-fetch`), is never redistributed, and is
//! extracted per run into a temp dir (member-confined) that is deleted afterwards.

use beacon_bds::{Confirm, EULA_URL, FetchError, VERSION, eula_method, extract_zip, log_eula, pin};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Console lines that are expected on a healthy run; each entry says why.
const BENIGN: &[(&str, &str)] = &[(
    "Debugger connection failed",
    "debug-listen: the probe disconnects after the first message and auto-attach logs it",
)];

/// Time to let behavior-pack scripts log after `Server started.` before sending `stop`.
const SETTLE: Duration = Duration::from_secs(5);
/// The game's script debugger port (what `beacon debug-config` prints).
const DEBUG_PORT: u16 = 19144;

#[derive(Parser)]
#[command(
    name = "beacon-bds",
    version,
    about = "Beacon in-game smoke test on BDS"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a .mcworld on BDS and check its console output.
    Smoke {
        #[arg(long, value_name = "path.mcworld")]
        world: PathBuf,
        /// `MARKER:N`: the console must contain at least N lines with MARKER (repeatable).
        #[arg(long, value_name = "MARKER:N")]
        expect: Vec<String>,
        /// Whole-run limit in seconds (download excluded).
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        /// Accept the Minecraft EULA (or `BEACON_BDS_EULA=accept`); logged to `<tools>/bds-eula.log`.
        #[arg(long)]
        accept_eula: bool,
        /// Allow the download without asking (or `BEACON_DOWNLOAD=yes`).
        #[arg(long)]
        yes: bool,
        /// Full console log destination.
        #[arg(long, default_value = "bds-smoke.log")]
        log: PathBuf,
    },
    /// Run a .mcworld on BDS with the script debugger listening on 19144 (6.6) and check that a
    /// TCP client reaches it and the game speaks first. Same trust rules as `smoke`.
    DebugListen {
        #[arg(long, value_name = "path.mcworld")]
        world: PathBuf,
        /// Whole-run limit in seconds (download excluded).
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        #[arg(long)]
        accept_eula: bool,
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "bds-debug-listen.log")]
        log: PathBuf,
    },
}

fn main() -> ExitCode {
    let (world, expect, timeout, accept_eula, yes, log, debug) = match Cli::parse().cmd {
        Cmd::Smoke {
            world,
            expect,
            timeout,
            accept_eula,
            yes,
            log,
        } => (world, expect, timeout, accept_eula, yes, log, false),
        Cmd::DebugListen {
            world,
            timeout,
            accept_eula,
            yes,
            log,
        } => (world, vec![], timeout, accept_eula, yes, log, true),
    };
    let code = match smoke(&world, &expect, timeout, accept_eula, yes, &log, debug) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    };
    ExitCode::from(code)
}

/// `server.properties` for a smoke run (BDS ignores unknown keys, so extras are harmless).
fn server_properties(port: u16, debug: bool) -> String {
    let dbg = if debug {
        // wait long at world load: the probe must reach the listener before the game moves on
        format!(
            "allow-inbound-script-debugging=true\nforce-inbound-debug-port={DEBUG_PORT}\nscript-debugger-auto-attach=listen\nscript-debugger-auto-attach-timeout=120\n"
        )
    } else {
        String::new()
    };
    format!(
        "{dbg}level-name=smoke\nallow-list=true\nmax-players=1\nonline-mode=true\n\
         server-port={port}\nserver-portv6={}\nenable-lan-visibility=false\n\
         transport=nethernet\ncontent-log-file-enabled=true\n",
        port.wrapping_add(1)
    )
}

fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map_or(41_234, |a| a.port())
}

/// Kills the server on drop (timeout, early return, panic).
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Failures found in the console `lines` (empty = pass).
fn scan(lines: &[String], expect: &[(String, usize)]) -> Vec<String> {
    let mut bad = Vec::new();
    for l in lines {
        // BDS lines are `[<date> <time> LEVEL] text`
        let hit = (l.starts_with('[') && l.contains(" ERROR]"))
            || (l.contains("[Scripting]") && l.to_lowercase().contains("error"));
        if hit && !BENIGN.iter().any(|(s, _)| l.contains(s)) {
            bad.push(format!("error line: {l}"));
        }
    }
    for (m, n) in expect {
        let got = lines.iter().filter(|l| l.contains(m.as_str())).count();
        if got < *n {
            bad.push(format!("marker {m:?}: expected at least {n}, saw {got}"));
        }
    }
    bad
}

/// One connection attempt to the debugger listener: `None` = nobody listening yet, `Some(Ok)` =
/// the game's first bytes (the debugger protocol is length-prefixed JSON; any first message
/// proves the listener is up).
fn try_probe() -> Option<Result<Vec<u8>, String>> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], DEBUG_PORT));
    let mut c = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)).ok()?;
    let _ = c.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 4096];
    Some(match c.read(&mut buf) {
        Ok(0) => Err("closed without a message".into()),
        // observed: 8 hex digits (length), newline, then {"type":"event","event":{"plugins":[..]}}
        Ok(n) if buf[..n].windows(14).any(|w| w == b"\"type\":\"event\"") => Ok(buf[..n].to_vec()),
        Ok(n) => Err(format!("unexpected first message: {}", preview(&buf[..n]))),
        Err(e) => Err(format!("no first message in 10s ({e})")),
    })
}

fn preview(b: &[u8]) -> String {
    String::from_utf8_lossy(&b[..b.len().min(200)])
        .escape_debug()
        .to_string()
}

fn parse_expect(s: &str) -> Result<(String, usize), String> {
    let (m, n) = s
        .rsplit_once(':')
        .ok_or_else(|| format!("--expect {s:?}: want MARKER:N"))?;
    Ok((
        m.to_owned(),
        n.parse()
            .map_err(|_| format!("--expect {s:?}: N must be a number"))?,
    ))
}

fn smoke(
    world: &Path,
    expect: &[String],
    timeout: u64,
    accept: bool,
    yes: bool,
    log: &Path,
    debug: bool,
) -> Result<bool, String> {
    let expect: Vec<_> = expect
        .iter()
        .map(|s| parse_expect(s))
        .collect::<Result<_, _>>()?;
    if !world.is_file() {
        return Err(format!("--world {} is not a file", world.display()));
    }
    let method =
        eula_method(accept, std::env::var("BEACON_BDS_EULA").ok().as_deref()).ok_or_else(|| {
            format!(
                "BDS use requires accepting the Minecraft EULA ({EULA_URL}): \
                 pass --accept-eula or set BEACON_BDS_EULA=accept"
            )
        })?;
    let tools = beacon_fetch::default_dir().ok_or("no tools dir (set BEACON_HOME)")?;
    log_eula(&tools, method).map_err(|e| format!("cannot write the EULA log: {e}"))?;
    println!(
        "Minecraft EULA ({EULA_URL}) accepted via {method}; logged to {}",
        tools.join("bds-eula.log").display()
    );

    let pin = pin().ok_or("no pinned BDS for this platform")?;
    // Same values as the `beacon` CLI accepts.
    let ok = yes
        || std::env::var("BEACON_DOWNLOAD")
            .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "yes" | "y" | "true" | "1"));
    let confirm = if ok { Confirm::Yes } else { Confirm::Tty };
    let zip = beacon_bds::fetch(&tools, confirm).map_err(|e| match e {
        FetchError::NeedsConfirm => format!(
            "BDS {VERSION} must be downloaded from {} ({} bytes, blake3-pinned); pass --yes or set BEACON_DOWNLOAD=yes",
            pin.url, pin.size
        ),
        e => e.to_string(),
    })?;

    let dir = tempfile::Builder::new()
        .prefix("beacon-bds-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let srv = dir.path();
    extract_zip(&zip, srv).map_err(|e| format!("BDS zip: {e}"))?;
    let w = srv.join("worlds").join("smoke");
    std::fs::create_dir_all(&w).map_err(|e| e.to_string())?;
    // the packs ride inside the world (behavior_packs/, world_behavior_packs.json)
    extract_zip(world, &w).map_err(|e| format!("{}: {e}", world.display()))?;
    std::fs::write(
        srv.join("server.properties"),
        server_properties(free_port(), debug),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(srv.join("allowlist.json"), "[]").map_err(|e| e.to_string())?;

    let exe = srv.join(if cfg!(windows) {
        "bedrock_server.exe"
    } else {
        "bedrock_server"
    });
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let mut cmd = Command::new(&exe);
    cmd.current_dir(srv)
        .env("LD_LIBRARY_PATH", ".")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Server(cmd.spawn().map_err(|e| format!("cannot start BDS: {e}"))?);
    let (tx, rx) = mpsc::channel();
    let pipes: [Box<dyn Read + Send>; 2] = [
        Box::new(child.0.stdout.take().unwrap()),
        Box::new(child.0.stderr.take().unwrap()),
    ];
    for r in pipes {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for l in io::BufReader::new(r).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    let mut stdin = child.0.stdin.take();
    // debug mode: connect to the listener as soon as it is up; its first bytes are the proof
    let mut probe: Option<Result<Vec<u8>, String>> = None;
    let mut last_try = Instant::now() - Duration::from_secs(1);

    let (start, limit) = (Instant::now(), Duration::from_secs(timeout));
    let mut lines = Vec::new();
    let (mut started, mut stopped, mut timed_out) = (None::<Instant>, false, false);
    loop {
        if start.elapsed() > limit {
            timed_out = true;
            break;
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(l) => {
                println!("{l}");
                if l.contains("Server started.") {
                    started = Some(Instant::now());
                }
                lines.push(l);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // server exited
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if debug && probe.is_none() && last_try.elapsed() >= Duration::from_millis(500) {
            last_try = Instant::now();
            probe = try_probe();
            if probe.is_some() {
                started.get_or_insert_with(Instant::now);
            }
        }
        if !stopped && (started.is_some_and(|t| t.elapsed() >= SETTLE) || probe.is_some()) {
            stopped = true;
            if let Some(mut s) = stdin.take() {
                let _ = writeln!(s, "stop");
            }
        }
    }
    drop(child); // kill + reap if still running
    let mut bad = scan(&lines, &expect);
    if timed_out {
        bad.push(format!(
            "timeout after {timeout}s (started: {})",
            started.is_some()
        ));
    } else if debug {
        match &probe {
            Some(Ok(b)) => println!("debugger first message: {} bytes: {}", b.len(), preview(b)),
            Some(Err(e)) => bad.push(format!("debugger on {DEBUG_PORT} connected but: {e}")),
            None => bad.push(format!("nothing listened on 127.0.0.1:{DEBUG_PORT}")),
        }
    } else if started.is_none() {
        bad.push("server exited before `Server started.`".into());
    }
    std::fs::write(log, lines.join("\n") + "\n")
        .map_err(|e| format!("cannot write {}: {e}", log.display()))?;
    println!(
        "\nBDS {VERSION} smoke: {} console lines, log at {}",
        lines.len(),
        log.display()
    );
    for b in &bad {
        println!("FAIL {b}");
    }
    if bad.is_empty() {
        println!("PASS");
    }
    Ok(bad.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eula_gate_refuses_without_consent() {
        assert_eq!(eula_method(false, None), None);
        assert_eq!(eula_method(false, Some("yes")), None);
        assert_eq!(eula_method(true, None), Some("--accept-eula"));
        assert_eq!(
            eula_method(false, Some("accept")),
            Some("BEACON_BDS_EULA=accept")
        );
        let (d, w) = (
            tempfile::tempdir().unwrap(),
            tempfile::NamedTempFile::new().unwrap(),
        );
        // no consent: exits before touching the tools dir or the network
        let r = smoke(
            w.path(),
            &[],
            1,
            false,
            false,
            &d.path().join("l.log"),
            false,
        );
        if std::env::var("BEACON_BDS_EULA").as_deref() != Ok("accept") {
            assert!(r.unwrap_err().contains("EULA"));
        }
    }

    #[test]
    fn properties_as_specified() {
        let s = server_properties(40000, false);
        assert!(!s.contains("debug"));
        let d = server_properties(40000, true);
        for k in [
            "allow-inbound-script-debugging=true",
            "force-inbound-debug-port=19144",
            "script-debugger-auto-attach=listen",
            "script-debugger-auto-attach-timeout=120",
        ] {
            assert!(d.lines().any(|l| l == k), "{k}");
        }
        for k in [
            "level-name=smoke",
            "allow-list=true",
            "max-players=1",
            "online-mode=true",
            "server-port=40000",
            "content-log-file-enabled=true",
        ] {
            assert!(s.lines().any(|l| l == k), "{k}");
        }
    }

    #[test]
    fn scanner() {
        let l = |s: &str| s.to_owned();
        let ok = [
            l("[Scripting] BEACON_SMOKE_OK game"),
            l("[Scripting] BEACON_SMOKE_OK lib"),
            l("Server started."),
        ];
        let ex = parse_expect("BEACON_SMOKE_OK:2").unwrap();
        assert!(scan(&ok, std::slice::from_ref(&ex)).is_empty());
        assert_eq!(scan(&ok[..1], std::slice::from_ref(&ex)).len(), 1); // missing marker
        let mut e = ok.to_vec();
        e.push(l(
            "[2026-10-16 14:47:20:645 ERROR] The following issues were found",
        ));
        e.push(l("[Scripting] Uncaught ReferenceError: x"));
        assert_eq!(scan(&e, &[]).len(), 2);
        assert!(parse_expect("nocolon").is_err() && parse_expect("A:x").is_err());
    }

    #[test]
    fn server_is_killed_on_drop() {
        let mut c = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("60");
            c
        };
        let mut child = c.stdout(Stdio::piped()).spawn().unwrap();
        let mut out = child.stdout.take().unwrap();
        let g = Server(child);
        let t = Instant::now();
        drop(g);
        // the pipe only reaches EOF once the process is gone
        io::copy(&mut out, &mut io::sink()).unwrap();
        assert!(t.elapsed() < Duration::from_secs(20));
    }
}
