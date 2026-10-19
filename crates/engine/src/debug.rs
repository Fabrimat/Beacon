//! Contract (phase 4): script debugging through Mojang's minecraft-debugger (6.6). Beacon owns
//! the source maps and the debug configuration; the official VS Code extension owns the
//! debugger (DAP, wire protocol, breakpoints). Nothing here speaks the wire protocol.
//!
//! Verified against upstream Mojang/minecraft-debugger `c69bc37` (2026-10-13) and BDS
//! 1.26.52.3 `server.properties`:
//! - launch fields: `type: "minecraft-js"`, `request: "attach"`, `mode: "connect" | "listen"`,
//!   `localRoot`, `sourceMapRoot`, `generatedSourceRoot`, `host`, `port` (default 19144),
//!   `targetModuleUuid`, `passcode`;
//! - the extension finds `*.map` under `sourceMapRoot`, resolves each map's `sources`
//!   relative to the map file, and matches the game's script path against the map's `file`
//!   relative to `generatedSourceRoot`;
//! - BDS: `allow-inbound-script-debugging=true`, `force-inbound-debug-port`,
//!   `script-debugger-auto-attach=listen`, `script-debugger-auto-attach-timeout` (seconds to
//!   wait at world load), `script-debugger-listen-address` (default loopback),
//!   `script-debugger-passcode`. The client uses `/script debugger connect` (VS Code in
//!   `listen` mode).
//!
//! The bundle's own map is unusable for this (measured: esbuild writes `sources` relative to
//! its temp outdir, or as `file:///D:/...` across drives). Dev-like builds therefore also
//! write a **debug mirror** inside the project, `.beacon/debug/<target>/<output>/`, holding
//! `<entry path below scripts/>.map` with `sources` relative to that folder (+ `sourcesContent`),
//! so `${workspaceFolder}`-relative roots resolve with the editor's own path spelling.
//! `targetModuleUuid` is the dev script module UUID (per worktree, 4.2), so committed
//! `launch.json` entries carry only `beaconOutput` / `beaconTarget`; the VS Code extension
//! fills in the rest at launch (a debug configuration provider for `minecraft-js`), and
//! `beacon debug-config --json` prints the resolved configuration for other editors.

use crate::config::ProjectConfig;
use crate::diag::Diagnostic;
use crate::lock::Lockfile;
use crate::path::RelPath;
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
    let _ = (target, output);
    todo!("phase 4 wave 1: debug interop")
}

/// Rewrites a bundle map for the mirror: every source made relative to `mirror` (project
/// relative), `file` set to the entry's file name, `sourcesContent` kept. Accepts esbuild's
/// relative, absolute and `file://` forms; a source outside the project stays absolute.
pub fn rewrite_map(
    map: &str,
    project_root: &Path,
    mirror: &RelPath,
    entry_file: &str,
) -> Result<String, Diagnostic> {
    let _ = (map, project_root, mirror, entry_file);
    todo!("phase 4 wave 1: debug interop")
}

/// The configuration for (`target`, `output`): roots as `${workspaceFolder}/...`, the dev
/// script module UUID of this worktree. `LaunchConfigStale` for an unknown target/output
/// or an output without scripts.
pub fn launch_config(
    root: &Path,
    cfg: &ProjectConfig,
    lock: &Lockfile,
    target: &str,
    output: &str,
    mode: Mode,
) -> Result<LaunchConfig, Diagnostic> {
    let _ = (root, cfg, lock, target, output, mode);
    todo!("phase 4 wave 1: debug interop")
}
