//! Contract (phase 4): script debugging through Mojang's minecraft-debugger (6.6). Beacon owns
//! the source maps and the debug configuration; the official VS Code extension owns the
//! debugger (DAP, wire protocol, breakpoints). Nothing here speaks the wire protocol.
//!
//! Verified against upstream Mojang/minecraft-debugger `c54af36` (2026-10-13) and BDS
//! 1.26.52.3 `server.properties`:
//! - launch fields: `type: "minecraft-js"`, `request: "attach"`, `mode: "connect" | "listen"`,
//!   `localRoot`, `sourceMapRoot`, `generatedSourceRoot`, `host`, `port` (default 19144),
//!   `targetModuleUuid`, `passcode`;
//! - the extension finds `*.map` under `sourceMapRoot`, resolves each map's `sources` and its
//!   `file` relative to the map's own folder, and names the script to the game by that
//!   generated file's path relative to `generatedSourceRoot`. The game names a script relative
//!   to the pack's `scripts/` folder (`main.js`, not `scripts/main.js`; proven end to end by
//!   the vscode e2e), so `generatedSourceRoot` is the mirror's `scripts` folder;
//! - BDS: `allow-inbound-script-debugging=true`, `force-inbound-debug-port`,
//!   `script-debugger-auto-attach=listen`, `script-debugger-auto-attach-timeout` (seconds to
//!   wait at world load), `script-debugger-listen-address` (default loopback),
//!   `script-debugger-passcode`. The client uses `/script debugger connect` (VS Code in
//!   `listen` mode).
//!
//! The bundle's own map is unusable for this (measured: esbuild writes `sources` relative to
//! its outdir, or as `file:///D:/...` across drives). The bundler now runs esbuild inside
//! `<project>/.beacon/tmp` and reports `sources` relative to the project root; dev-like builds
//! also write a **debug mirror** inside the project, `.beacon/debug/<target>/<output>/<entry>.map`
//! (`<entry>` is the BP-relative script path, `scripts/main.js`), next to a copy of the bundle,
//! with `sources` relative to the map's folder (+ `sourcesContent`) and `file` the bundle's name
//! in that folder, so the roots resolve with the editor's own path spelling, on any drive.
//! `targetModuleUuid` is the dev script module UUID (per worktree, 4.2), so committed
//! `launch.json` entries carry only `beaconOutput` / `beaconTarget`; the VS Code extension
//! fills in the rest at launch (a debug configuration provider for `minecraft-js`), and
//! `beacon debug-config --json` prints the resolved configuration for other editors.

use crate::config::{Artifact, ProjectConfig};
use crate::diag::{Code, Diagnostic};
use crate::lock::{Lockfile, dev_uuid};
use crate::path::RelPath;
use serde_json::Value;
use std::path::Path;

/// Default port of the game's script debugger.
pub const DEFAULT_PORT: u16 = 19144;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The editor connects to a listening game (BDS `script debugger listen`, auto-attach).
    Connect,
    /// The editor listens; the client runs `/script debugger connect`.
    Listen,
}

/// A resolved `minecraft-js` attach configuration (field names as the extension reads them).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchConfig {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub request: &'static str,
    pub name: String,
    pub mode: Mode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub port: u16,
    pub local_root: String,
    pub source_map_root: String,
    pub generated_source_root: String,
    pub target_module_uuid: String,
}

/// Debug mirror folder of one output of one dev-like target: `.beacon/debug/<target>/<output>`.
pub fn mirror_dir(target: &str, output: &str) -> Result<RelPath, Diagnostic> {
    RelPath::parse(&format!(".beacon/debug/{target}/{output}")).map_err(|e| {
        Diagnostic::new(
            Code::LaunchConfigStale,
            format!("target {target:?} / output {output:?} is not a usable folder name: {e:?}"),
        )
    })
}

/// Rewrites a bundle map for the mirror: every source relative to `mirror` (the project-relative
/// folder the map is written in), `file` set to the file name of `entry_file` (the bundle sits
/// beside its map, and the debugger resolves `file` against the map's folder), `sourcesContent`
/// kept. Accepts the
/// bundler's project-relative form and absolute / `file://` forms under the project; any other
/// source is left as is. A map that is not a JSON object is an error.
pub fn rewrite_map(
    map: &str,
    project_root: &Path,
    mirror: &RelPath,
    entry_file: &str,
) -> Result<String, Diagnostic> {
    let bad = |m: String| Diagnostic::new(Code::EmitFailed, format!("debug mirror: {m}"));
    let mut v: Value =
        serde_json::from_str(map).map_err(|e| bad(format!("bad source map: {e}")))?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| bad("source map is not an object".into()))?;
    let up = "../".repeat(mirror.as_str().split('/').count());
    let root = project_root.to_string_lossy().replace(BS, "/");
    let root = root.trim_start_matches("//?/").trim_end_matches('/');
    for s in obj
        .get_mut("sources")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let Some(src) = s.as_str() else { continue };
        let src = src.strip_prefix("file:///").unwrap_or(src).replace(BS, "/");
        let under = src
            .get(..root.len())
            .filter(|p| p.eq_ignore_ascii_case(root) && src[root.len()..].starts_with('/'))
            .map(|_| &src[root.len() + 1..]);
        let rel = match under {
            Some(r) => Some(r),
            // not absolute (`/x`, `C:/x`) and not a URL: project relative
            None if !src.starts_with('/') && !src.contains(':') => Some(src.as_str()),
            None => None,
        };
        if let Some(r) = rel {
            *s = Value::String(format!("{up}{}", r.trim_start_matches("./")));
        }
    }
    let name = entry_file.rsplit('/').next().unwrap_or(entry_file);
    obj.insert("file".into(), Value::String(name.into()));
    serde_json::to_string(&v).map_err(|e| bad(e.to_string()))
}

impl LaunchConfig {
    /// Replaces the `${workspaceFolder}` prefix of the roots with the project's absolute path
    /// (`/`-separated), for editors without that variable.
    pub fn absolutize(&mut self, root: &Path) {
        let abs = dunce::canonicalize(root).unwrap_or_else(|_| root.into());
        let abs = abs.to_string_lossy().replace(BS, "/");
        for r in [
            &mut self.local_root,
            &mut self.source_map_root,
            &mut self.generated_source_root,
        ] {
            *r = r.replace("${workspaceFolder}", &abs);
        }
    }
}

/// The configuration for (`target`, `output`): roots as `${workspaceFolder}/...`, the dev
/// script module UUID of this worktree. `LaunchConfigStale` for an unknown target/output
/// or a target that is not a dev build.
// ponytail: "output without scripts" is not checked here (the routing is the build's); a
// scriptless output simply has an empty mirror. Check it if the editor needs the warning.
pub fn launch_config(
    root: &Path,
    cfg: &ProjectConfig,
    lock: &Lockfile,
    target: &str,
    output: &str,
    mode: Mode,
) -> Result<LaunchConfig, Diagnostic> {
    let stale = |m: String| Diagnostic::new(Code::LaunchConfigStale, m);
    match cfg.beacon.targets.get(target) {
        Some(t) if t.artifact == Artifact::Dev => {}
        Some(_) => return Err(stale(format!("target {target:?} is not a dev target"))),
        None => return Err(stale(format!("unknown target {target:?}"))),
    }
    let release = lock.script_module_uuid(output).ok_or_else(|| {
        stale(format!(
            "unknown output {output:?} (or no lockfile entry yet)"
        ))
    })?;
    let mirror = format!(
        "${{workspaceFolder}}/{}",
        mirror_dir(target, output)?.as_str()
    );
    // the game names scripts relative to the pack's `scripts/` folder
    let generated = format!("{mirror}/scripts");
    Ok(LaunchConfig {
        kind: "minecraft-js",
        request: "attach",
        name: format!("Beacon: {target}/{output}"),
        mode,
        host: (mode == Mode::Connect).then(|| "127.0.0.1".to_owned()),
        port: DEFAULT_PORT,
        local_root: mirror.clone(),
        source_map_root: mirror,
        generated_source_root: generated,
        target_module_uuid: dev_uuid(&release, root),
    })
}
const BS: char = '\u{5c}';
