//! Play, check and test commands: daemon client (build, deploy) and `beacon test` sidecar.
//!
//! # Play
//! `play` calls the daemon's MCP `deploy` tool (`dryRun:false`) with grant `deploy` only, which
//! builds the first `dev` target into the edition's `development_*_packs`. An untrusted project
//! with code comes back as `BCN0050` and nothing is built (the app never sends `trust: true`);
//! no Minecraft install is `BCN0200`, checked before anything is built.
//!
//! # Launch methods (verified 2026-10-19 on this machine)
//! Windows 11, Bedrock GDK (`Microsoft.MinecraftUWP_1.26.5020.0`) and Education installed.
//! Verification: `Start-Process '<uri>'`, then the process and its image path were read back
//! (`Get-Process Minecraft.Windows`) and a fresh `Debug_Log*` appeared in the game's log folder.
//! - Bedrock GDK: `minecraft:` started `C:\Program Files\WindowsApps\Microsoft.MinecraftUWP_...\
//!   Minecraft.Windows.exe`. Registered as `URL:minecraft` under `HKCR\minecraft`.
//! - Education: `minecraftedu:` started `C:\Program Files\Microsoft Studios\Minecraft Education\
//!   Minecraft.Windows.exe` (`HKCR\minecraftedu\shell\open\command`).
//! - Bedrock UWP layout and Preview (GDK/UWP): **not verified** (Preview is not installed here
//!   and `minecraft-preview:` is not registered; the UWP layout is a stale folder here). Beacon
//!   does not launch them (`launched: false`); the packs are still deployed.
//!
//! The URIs carry no project data. A launch is a request; the game is not verified to run.
//! With `BEACON_COM_MOJANG` set (deploy to an arbitrary folder) nothing is launched.
//!
//! # Content log (verified on this machine)
//! The game writes `ContentLog<yyyy-mm-dd_hh-mm-ss>_1.txt` (one per game start, lines like
//! `10:52:44[Recipes][error]-text`) into the `logs` folder next to `games`:
//! - Bedrock GDK: `%APPDATA%\Minecraft Bedrock\logs\`
//! - Education: `%APPDATA%\Minecraft Education Edition\logs\`
//!
//! The file only exists when "Enable Content Log File" is on (Settings > Creator); with it off
//! the game writes only `Debug_Log*.txt` (seen on both products today) and the tail times out
//! quietly. UWP/Preview log folders are unverified: no tailing. `BEACON_CONTENT_LOG_DIR`
//! overrides the folder (tests).

use super::{
    AppCore, AppError, AppResult, ContentLogLine, PlayReport, client, sanitize,
    sanitize_diagnostics,
};
use crate::Grant;
use beacon_engine::config::Artifact;
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::gametest::{TestOptions, TestReport};
use beacon_engine::model::Edition;
use beacon_engine::route;
use beacon_engine::setup::{self, Layout, MinecraftInstall, Product};
use serde_json::json;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How long the tail waits for / follows a content log.
pub const TAIL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Most lines forwarded per tail (the rest is dropped; a chatty pack can write MB per second).
pub const TAIL_MAX_LINES: usize = 2000;
const MAX_CHUNK: u64 = 1 << 20;
const POLL: Duration = Duration::from_millis(300);

/// The install a deploy of `edition` goes to.
fn install_for(edition: Edition) -> Option<MinecraftInstall> {
    setup::preferred(&setup::detect(), edition).cloned()
}

/// The URI that starts this install, only where verified (see the module doc).
fn launch_uri(i: &MinecraftInstall) -> Option<&'static str> {
    match (i.product, i.layout) {
        (Product::Bedrock, Layout::Gdk) => Some("minecraft:"),
        (Product::Education, _) => Some("minecraftedu:"),
        _ => None,
    }
}

#[cfg(windows)]
fn launch(uri: &str) -> bool {
    use std::os::windows::process::CommandExt;
    // `start "" <uri>`: the shell's protocol dispatch; the uri is one of our constants
    std::process::Command::new("cmd")
        .args(["/C", "start", "", uri])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(windows))]
fn launch(_: &str) -> bool {
    false
}

/// The `logs` folder of an install (verified layouts only).
fn log_dir(i: &MinecraftInstall) -> Option<PathBuf> {
    match i.layout {
        // <base>/Users/Shared/games/com.mojang
        Layout::Gdk if i.product == Product::Bedrock => {
            i.com_mojang.ancestors().nth(4).map(|b| b.join("logs"))
        }
        // <base>/games/com.mojang
        Layout::Classic => i.com_mojang.ancestors().nth(2).map(|b| b.join("logs")),
        _ => None,
    }
}

/// The play target (given, else the first `dev` one) and its edition.
fn play_target(root: &Path, target: Option<String>) -> AppResult<(String, Edition)> {
    let cfg = route::load(root)?;
    let t = &cfg.beacon.targets;
    let name = target
        .or_else(|| {
            t.iter()
                .find(|(_, d)| d.artifact == Artifact::Dev)
                .map(|(n, _)| n.clone())
        })
        .ok_or_else(|| AppError::new(Code::ConfigField, "no dev target in beacon.targets"))?;
    match t.get(&name) {
        Some(d) if d.artifact == Artifact::Dev => Ok((name, d.edition)),
        _ => Err(AppError::new(
            Code::ConfigField,
            &format!("`{name}` is not a dev target"),
        )),
    }
}

fn level(line: &str) -> &'static str {
    if line.contains("][error]") {
        "error"
    } else if line.contains("][warning]") {
        "warning"
    } else if line.contains("][verbose]") {
        "verbose"
    } else {
        "info"
    }
}

/// Lines of a log chunk, sanitized; empty lines dropped.
fn lines(chunk: &[u8]) -> Vec<ContentLogLine> {
    String::from_utf8_lossy(chunk)
        .split('\n')
        .map(|l| ContentLogLine {
            level: level(l).into(),
            text: sanitize(l),
        })
        .filter(|l| !l.text.trim().is_empty())
        .collect()
}

/// Newest `ContentLog*.txt` in `dir` (by modification time); the flag says it was created at or
/// after `since` (a game start after Play) rather than already being written to.
fn newest_log(dir: &Path, since: SystemTime) -> Option<(PathBuf, bool)> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n.starts_with("ContentLog") && n.ends_with(".txt")
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            let fresh = m.created().is_ok_and(|c| c >= since);
            Some((m.modified().ok()?, e.path(), fresh))
        })
        .max()
        .map(|(_, p, fresh)| (p, fresh))
}

impl AppCore {
    pub fn check(&self, _root: PathBuf) -> AppResult<Vec<Diagnostic>> {
        todo!("wave 2")
    }

    /// Play: deploy the play target through the daemon (grant `deploy` only), then ask the game
    /// to start. Build problems (`BCN0050` untrusted, `BCN0200` no install, compile errors) come
    /// back in the report with `deployed_to: None`; only an unusable project or daemon is an
    /// `AppError`. Start [`AppCore::content_log_tail`] right after.
    pub fn play(&self, root: PathBuf, target: Option<String>) -> AppResult<PlayReport> {
        self.play_with(root, target, &setup::detect())
    }

    /// [`AppCore::play`] against an explicit install list (tests; `play` passes the detected one).
    pub fn play_with(
        &self,
        root: PathBuf,
        target: Option<String>,
        installs: &[MinecraftInstall],
    ) -> AppResult<PlayReport> {
        let root = crate::discovery::canonical_root(&root);
        let (target, edition) = play_target(&root, target)?;
        let failed = |d: Vec<Diagnostic>| PlayReport {
            diagnostics: sanitize_diagnostics(d),
            deployed_to: None,
            launched: false,
        };
        let over = std::env::var_os("BEACON_COM_MOJANG").map(PathBuf::from);
        let install = if over.is_some() {
            None
        } else {
            match setup::preferred(installs, edition) {
                Some(i) => Some(i.clone()),
                None => {
                    return Ok(failed(vec![Diagnostic::new(
                        Code::MinecraftNotFound,
                        "No Minecraft install found. Start Minecraft once, sign in and reach the main menu, then retry.",
                    )]));
                }
            }
        };
        let out = match client::call_tool(
            &self.beacon_exe,
            &root,
            Some(Grant::Deploy),
            "deploy",
            json!({ "target": target, "dryRun": false }),
        ) {
            Ok(o) => o,
            // the daemon itself is unavailable: not a build result
            Err(e) if e.code == Code::ServeUnavailable.id() => return Err(e),
            // a refused or failed build: the daemon's first diagnostic (BCN0050 when untrusted)
            // ponytail: only the first diagnostic of a failed build is kept (client::call_tool)
            Err(e) => {
                let code = Code::from_id(&e.code).unwrap_or(Code::EmitFailed);
                return Ok(failed(vec![Diagnostic::new(code, e.message)]));
            }
        };
        let diagnostics: Vec<Diagnostic> = out
            .json()
            .ok()
            .and_then(|j| serde_json::from_value(j["diagnostics"].clone()).ok())
            .unwrap_or_default();
        let launched = install.as_ref().and_then(launch_uri).is_some_and(launch);
        Ok(PlayReport {
            diagnostics: sanitize_diagnostics(diagnostics),
            deployed_to: over.or(install.map(|i| i.com_mojang)),
            launched,
        })
    }

    /// Follows the game's content log after a Play started at `since`: waits (up to
    /// [`TAIL_TIMEOUT`]) for a `ContentLog*.txt` newer than `since`, then calls `emit` with each
    /// new complete line, sanitized, at most [`TAIL_MAX_LINES`] of them. Returns when `stop` is
    /// set (screen closed), on the timeout, or at the line cap. Blocking: run it on a thread.
    pub fn content_log_tail(
        &self,
        root: PathBuf,
        target: Option<String>,
        since: SystemTime,
        stop: &AtomicBool,
        emit: impl FnMut(ContentLogLine),
    ) {
        let dir = std::env::var_os("BEACON_CONTENT_LOG_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                let root = crate::discovery::canonical_root(&root);
                let (_, edition) = play_target(&root, target).ok()?;
                log_dir(&install_for(edition)?)
            });
        if let Some(dir) = dir {
            tail_dir(&dir, since, TAIL_TIMEOUT, stop, emit);
        }
    }

    /// `beacon test --json` through the sidecar.
    pub fn test_run(&self, _root: PathBuf, _opts: TestOptions) -> AppResult<TestReport> {
        todo!("wave 2")
    }
}

fn tail_dir(
    dir: &Path,
    since: SystemTime,
    timeout: Duration,
    stop: &AtomicBool,
    mut emit: impl FnMut(ContentLogLine),
) {
    let t0 = Instant::now();
    let mut file: Option<(PathBuf, u64)> = None; // (path, offset of the next unread byte)
    let mut sent = 0;
    while !stop.load(Ordering::Relaxed) && t0.elapsed() < timeout && sent < TAIL_MAX_LINES {
        match (newest_log(dir, since), &file) {
            // first sight: a file the game already had open is followed from its live end,
            // one created after Play from its start
            (Some((p, fresh)), None) => {
                let at = if fresh {
                    0
                } else {
                    std::fs::File::open(&p)
                        .and_then(|f| f.metadata())
                        .map_or(0, |m| m.len())
                };
                file = Some((p, at));
            }
            // a game start after Play: switch to its log from the start
            (Some((p, true)), Some((cur, _))) if p != *cur => file = Some((p, 0)),
            _ => {}
        }
        if let Some((p, off)) = &mut file
            && let Ok(mut f) = std::fs::File::open(&p)
        {
            // at most 1 MiB per poll
            let mut buf = Vec::new();
            if f.seek(SeekFrom::Start(*off)).is_ok()
                && f.take(MAX_CHUNK).read_to_end(&mut buf).is_ok()
            {
                // whole lines only; a chunk with no newline is one over-long line: send it
                // truncated (sanitize caps it) and move on
                let end = match buf.iter().rposition(|&b| b == b'\n') {
                    Some(e) => Some(e),
                    None if buf.len() as u64 >= MAX_CHUNK => Some(buf.len()),
                    None => None,
                };
                if let Some(end) = end {
                    *off += (end as u64 + 1).min(buf.len() as u64);
                    for l in lines(&buf[..end]) {
                        if sent >= TAIL_MAX_LINES {
                            break;
                        }
                        sent += 1;
                        emit(l);
                    }
                }
            }
        }
        std::thread::sleep(POLL);
    }
}
