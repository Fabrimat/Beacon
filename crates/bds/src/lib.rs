//! `beacon-bds` library (8.5): the pinned BDS download, the EULA gate, member-confined
//! extraction, `server.properties` and the server process ([`install`], [`run`]), shared by
//! `cargo bds smoke` / `debug-listen`, `beacon setup` and `beacon test`.
//!
//! Trust: the BDS zip is downloaded only from minecraft.net, only after explicit EULA acceptance
//! (logged), only with the pinned size + blake3 (`beacon-fetch`), is never redistributed, and is
//! extracted per run into a temp dir (member-confined) that is deleted afterwards.

pub use beacon_fetch::{Confirm, FetchError, Pinned};
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const VERSION: &str = "1.26.52.3";
pub const EULA_URL: &str = "https://minecraft.net/eula";

// Pins. To refresh: GET https://net-secondary.web.minecraft-services.net/api/v1.0/download/links
// (the data behind minecraft.net/download/server/bedrock; keys serverBedrockWindows /
// serverBedrockLinux, NOT the Preview ones), download each zip YOURSELF and take its byte size and
// blake3 (`b3sum`), then update VERSION and the URLs. Never copy a hash from a third party and
// never use a mirror. The host accepts any honest user agent (fetch sends `beacon/<v>`); if it
// ever refuses non-browser agents, fix the UA in beacon-fetch, do not impersonate a browser.
pub const WIN: Pinned = Pinned {
    url: "https://www.minecraft.net/bedrockdedicatedserver/bin-win/bedrock-server-1.26.52.3.zip",
    blake3: "03d640d7268fca93be29a1f27c5094f76188e9df68a0bb70f62184c9841840d9",
    size: 85_766_941,
};
// ponytail: the Linux pin is verified but the Linux run path (chmod, LD_LIBRARY_PATH) is untested.
pub const LINUX: Pinned = Pinned {
    url: "https://www.minecraft.net/bedrockdedicatedserver/bin-linux/bedrock-server-1.26.52.3.zip",
    blake3: "8cb0d97ff8bef9065643ba69f0e5ee70c0758ce1897b232f32f3ebbc46dae6e8",
    size: 94_927_522,
};

/// The pinned server zip of this platform (`None` where Beacon pins no BDS).
pub fn pin() -> Option<Pinned> {
    if cfg!(windows) {
        Some(WIN)
    } else if cfg!(target_os = "linux") {
        Some(LINUX)
    } else {
        None
    }
}

/// The pinned zip from the hash cache in `tools`, downloaded per `confirm` on a miss.
pub fn fetch(tools: &Path, confirm: Confirm) -> Result<PathBuf, FetchError> {
    let pin = pin().ok_or_else(|| FetchError::Http("no pinned BDS for this platform".into()))?;
    beacon_fetch::fetch(&pin, tools, confirm)
}

const MAX_UNZIPPED: u64 = 2 << 30; // BDS is ~0.4 GB unzipped; guards against a zip bomb

/// `--accept-eula` or `BEACON_BDS_EULA=accept`; the method is what gets logged.
pub fn eula_method(flag: bool, env: Option<&str>) -> Option<&'static str> {
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

pub fn log_eula(tools: &Path, method: &str) -> io::Result<()> {
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
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
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

/// The game's script debugger port (what `beacon debug-config` prints).
pub const DEBUG_PORT: u16 = 19144;

/// `server.properties` for a Beacon run of world `level` (BDS ignores unknown keys, so extras are
/// harmless). `debug`: the script debugger listens on [`DEBUG_PORT`].
pub fn server_properties(level: &str, port: u16, debug: bool) -> String {
    let dbg = if debug {
        // wait long at world load: the probe must reach the listener before the game moves on
        format!(
            "allow-inbound-script-debugging=true\nforce-inbound-debug-port={DEBUG_PORT}\nscript-debugger-auto-attach=listen\nscript-debugger-auto-attach-timeout=120\n"
        )
    } else {
        String::new()
    };
    format!(
        "{dbg}level-name={level}\nallow-list=true\nmax-players=1\nonline-mode=true\n\
         server-port={port}\nserver-portv6={}\nenable-lan-visibility=false\n\
         transport=nethernet\ncontent-log-file-enabled=true\n",
        port.wrapping_add(1)
    )
}

pub fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|s| s.local_addr())
        .map_or(41_234, |a| a.port())
}

/// Extracts the BDS `zip` into `srv` and configures it for world `level` (a free port, an empty
/// allow list). Returns the world folder `srv/worlds/<level>` (created, empty): the caller puts
/// the world and its packs there.
pub fn install(zip: &Path, srv: &Path, level: &str, debug: bool) -> Result<PathBuf, String> {
    extract_zip(zip, srv).map_err(|e| format!("BDS zip: {e}"))?;
    let w = srv.join("worlds").join(level);
    std::fs::create_dir_all(&w).map_err(|e| e.to_string())?;
    std::fs::write(
        srv.join("server.properties"),
        server_properties(level, free_port(), debug),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(srv.join("allowlist.json"), "[]").map_err(|e| e.to_string())?;
    Ok(w)
}

/// Kills the server on drop (timeout, early return, panic).
pub struct Server(pub Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What [`run`] saw.
pub struct Run {
    /// Every console line (stdout and stderr).
    pub lines: Vec<String>,
    /// `limit` ran out before the server exited (it was killed).
    pub timed_out: bool,
}

/// Starts the BDS installed in `srv` and calls `step` with each console line (`Some`), and at
/// least every 250 ms without one (`None`), until the server exits or `limit` runs out; `step`
/// talks to the console through the writer (`stop` ends the server). The server is killed when
/// this returns.
pub fn run(
    srv: &Path,
    limit: Duration,
    mut step: impl FnMut(Option<&str>, &mut dyn Write),
) -> Result<Run, String> {
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
        Box::new(child.0.stdout.take().expect("piped")),
        Box::new(child.0.stderr.take().expect("piped")),
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
    let mut stdin = child.0.stdin.take().expect("piped");
    let start = Instant::now();
    let mut lines = Vec::new();
    loop {
        if start.elapsed() > limit {
            return Ok(Run {
                lines,
                timed_out: true,
            });
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(l) => {
                step(Some(&l), &mut stdin);
                lines.push(l);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break, // server exited
            Err(mpsc::RecvTimeoutError::Timeout) => step(None, &mut stdin),
        }
    }
    Ok(Run {
        lines,
        timed_out: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eula_gate_methods() {
        assert_eq!(eula_method(false, None), None);
        assert_eq!(eula_method(false, Some("yes")), None);
        assert_eq!(eula_method(true, None), Some("--accept-eula"));
        assert_eq!(
            eula_method(false, Some("accept")),
            Some("BEACON_BDS_EULA=accept")
        );
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
