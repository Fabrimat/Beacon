//! `beacon-bds` (8.5): `cargo bds smoke --world <w.mcworld>` runs a built world on a hash-pinned
//! Bedrock Dedicated Server and asserts its console output. Exit codes: 0 pass, 1 assertion
//! failure, 2 usage / EULA / download errors.
//!
//! Trust: the BDS zip is downloaded only from minecraft.net, only after explicit EULA acceptance
//! (logged), only with the pinned size + blake3 (`beacon-fetch`), is never redistributed, and is
//! extracted per run into a temp dir (member-confined) that is deleted afterwards.

use beacon_fetch::{Confirm, FetchError, Pinned};
use clap::{Parser, Subcommand};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = "1.26.52.3";
const EULA_URL: &str = "https://minecraft.net/eula";

// Pins. To refresh: GET https://net-secondary.web.minecraft-services.net/api/v1.0/download/links
// (the data behind minecraft.net/download/server/bedrock; keys serverBedrockWindows /
// serverBedrockLinux, NOT the Preview ones), download each zip YOURSELF and take its byte size and
// blake3 (`b3sum`), then update VERSION and the URLs. Never copy a hash from a third party and
// never use a mirror. The host accepts any honest user agent (fetch sends `beacon/<v>`); if it
// ever refuses non-browser agents, fix the UA in beacon-fetch, do not impersonate a browser.
const WIN: Pinned = Pinned {
    url: "https://www.minecraft.net/bedrockdedicatedserver/bin-win/bedrock-server-1.26.52.3.zip",
    blake3: "03d640d7268fca93be29a1f27c5094f76188e9df68a0bb70f62184c9841840d9",
    size: 85_766_941,
};
// ponytail: the Linux pin is verified but the Linux run path (chmod, LD_LIBRARY_PATH) is untested.
const LINUX: Pinned = Pinned {
    url: "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-1.26.52.3.zip",
    blake3: "8cb0d97ff8bef9065643ba69f0e5ee70c0758ce1897b232f32f3ebbc46dae6e8",
    size: 94_927_522,
};

/// Console lines that are expected on a healthy run; each entry says why.
const BENIGN: &[(&str, &str)] = &[];

/// Time to let behavior-pack scripts log after `Server started.` before sending `stop`.
const SETTLE: Duration = Duration::from_secs(5);
const MAX_UNZIPPED: u64 = 2 << 30; // BDS is ~0.4 GB unzipped; guards against a zip bomb

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
}

fn main() -> ExitCode {
    let Cmd::Smoke {
        world,
        expect,
        timeout,
        accept_eula,
        yes,
        log,
    } = Cli::parse().cmd;
    let code = match smoke(&world, &expect, timeout, accept_eula, yes, &log) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("error: {e}");
            2
        }
    };
    ExitCode::from(code)
}

/// `--accept-eula` or `BEACON_BDS_EULA=accept`; the method is what gets logged.
fn eula_method(flag: bool, env: Option<&str>) -> Option<&'static str> {
    if flag {
        Some("--accept-eula")
    } else if env == Some("accept") {
        Some("BEACON_BDS_EULA=accept")
    } else {
        None
    }
}

fn utc_now() -> String {
    let s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let (days, rem) = ((s / 86400) as i64, s % 86400);
    // civil-from-days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn log_eula(tools: &Path, method: &str) -> io::Result<()> {
    std::fs::create_dir_all(tools)?;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(tools.join("bds-eula.log"))?;
    writeln!(
        f,
        "{} accepted Minecraft EULA ({EULA_URL}) for BDS {VERSION} via {method}",
        utc_now()
    )
}

/// Extracts every member of `zip` under `dest`. Any `..`, absolute/drive path or link rejects the
/// whole archive before anything is written.
fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    let mut z = zip::ZipArchive::new(std::fs::File::open(zip).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut total = 0u64;
    let mut names = Vec::new();
    for i in 0..z.len() {
        let e = z.by_index_raw(i).map_err(|e| e.to_string())?;
        let n = e.name().map_err(|e| e.to_string())?.into_owned();
        let safe = !(n.starts_with(['/', '\\'])
            || n.as_bytes().get(1) == Some(&b':')
            || n.split(['/', '\\']).any(|c| c == ".."));
        // S_IFLNK: zip stores a link as a member whose body is the target
        let link = e.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000);
        match e.enclosed_name() {
            Some(rel) if safe && !link => names.push(rel),
            _ => return Err(format!("unsafe archive member {n:?}")),
        }
        total += e.size();
    }
    if total > MAX_UNZIPPED {
        return Err("archive is implausibly large".into());
    }
    for (i, rel) in names.iter().enumerate() {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        let out = dest.join(rel);
        if e.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        std::fs::create_dir_all(out.parent().unwrap()).map_err(|e| e.to_string())?;
        let mut f = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        let n = e.size();
        io::copy(&mut (&mut e).take(n), &mut f).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `server.properties` for a smoke run (BDS ignores unknown keys, so extras are harmless).
fn server_properties(port: u16) -> String {
    format!(
        "level-name=smoke\nallow-list=true\nmax-players=1\nonline-mode=true\n\
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

    let pin = if cfg!(windows) { WIN } else { LINUX };
    // Same values as the `beacon` CLI accepts.
    let ok = yes
        || std::env::var("BEACON_DOWNLOAD")
            .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "yes" | "y" | "true" | "1"));
    let confirm = if ok { Confirm::Yes } else { Confirm::Tty };
    let zip = beacon_fetch::fetch(&pin, &tools, confirm).map_err(|e| match e {
        FetchError::NeedsConfirm => format!(
            "BDS {VERSION} must be downloaded from {} ({} bytes, blake3-pinned); \
             pass --yes or set BEACON_DOWNLOAD=yes",
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
        server_properties(free_port()),
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
        if !stopped && started.is_some_and(|t| t.elapsed() >= SETTLE) {
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
        let r = smoke(w.path(), &[], 1, false, false, &d.path().join("l.log"));
        if std::env::var("BEACON_BDS_EULA").as_deref() != Ok("accept") {
            assert!(r.unwrap_err().contains("EULA"));
        }
    }

    #[test]
    fn eula_log_line() {
        let d = tempfile::tempdir().unwrap();
        log_eula(d.path(), "--accept-eula").unwrap();
        log_eula(d.path(), "BEACON_BDS_EULA=accept").unwrap();
        let s = std::fs::read_to_string(d.path().join("bds-eula.log")).unwrap();
        assert_eq!(s.lines().count(), 2);
        assert!(s.contains(VERSION) && s.contains("via --accept-eula") && s.starts_with("20"));
    }

    fn zip_of(members: &[(&str, &[u8])]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let mut w = zip::ZipWriter::new(&mut f);
        for (n, b) in members {
            // start_file keeps `..` as written
            let o = zip::write::SimpleFileOptions::default();
            if let Some(l) = n.strip_prefix("link:") {
                w.add_symlink(l, "target", o).unwrap();
                continue;
            }
            w.start_file(*n, o).unwrap();
            w.write_all(b).unwrap();
        }
        w.finish().unwrap();
        f
    }

    #[test]
    fn zip_extraction_is_confined() {
        let p = tempfile::tempdir().unwrap();
        let d = p.path().join("out");
        std::fs::create_dir(&d).unwrap();
        let ok = zip_of(&[("a/b.txt", b"hi")]);
        extract_zip(ok.path(), &d).unwrap();
        assert_eq!(std::fs::read(d.join("a/b.txt")).unwrap(), b"hi");
        for bad in [
            "../evil",
            "a/../../evil",
            "/abs",
            "\\abs",
            "C:evil",
            "link:a/l",
        ] {
            let z = zip_of(&[("fine.txt", b"x"), (bad, b"x")]);
            let d2 = p.path().join("out2");
            std::fs::create_dir_all(&d2).unwrap();
            assert!(extract_zip(z.path(), &d2).is_err(), "{bad}");
            assert_eq!(std::fs::read_dir(&d2).unwrap().count(), 0, "{bad}"); // nothing written
            assert!(!p.path().join("evil").exists());
        }
    }

    #[test]
    fn properties_as_specified() {
        let s = server_properties(40000);
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

    /// Network: the real pin downloads and verifies (85 MB). `-- --ignored real_pinned_download`
    #[test]
    #[ignore]
    fn real_pinned_download() {
        let d = tempfile::tempdir().unwrap();
        let p = if cfg!(windows) { WIN } else { LINUX };
        let f = beacon_fetch::fetch(&p, d.path(), Confirm::Yes).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().len(), p.size);
        assert!(beacon_fetch::fetch(&p, d.path(), Confirm::No).is_ok()); // cached by hash
        let x = tempfile::tempdir().unwrap();
        extract_zip(&f, x.path()).unwrap();
        let exe = if cfg!(windows) {
            "bedrock_server.exe"
        } else {
            "bedrock_server"
        };
        assert!(x.path().join(exe).is_file());
    }
}
