//! Phase-5 contract: the desktop app's core (6.1). The Tauri shell in `apps/desktop/` is a thin
//! layer: each `#[tauri::command]` forwards to one [`AppCore`] method of the same name, so the
//! logic is tested here, in the workspace, without a webview.
//!
//! ```text
//! webview (React)  --Tauri IPC invoke / events-->  apps/desktop (commands = COMMANDS)
//!                                                     |
//!                                                  AppCore (this module, in process)
//!   project ops (check, play, settings) -> the project's daemon over the pipe
//!                                          (per-call shim::connect, MCP channel, grants edit/deploy)
//!   trust prompt, init                  -> engine in process (trust, init)
//!   setup, test                         -> bundled `beacon` sidecar, `--json`
//!   updater                             -> crate::update policy only ("updates are off" in the alpha)
//! ```
//!
//! - **One writer.** The app is a daemon client like VS Code and agents (3): settings forms
//!   are edits against a base hash, refused with `DocumentStale` / `DocumentDirty`.
//! - **No TCP.** The webview never reaches the daemon; Tauri IPC replaces the TCP + bearer
//!   token endpoint planned in 3 (it is not built).
//! - **Trust (5.5)** comes only from the stored decision; the app never sends `trust: true` in
//!   its `Hello`. [`AppCore::trust_grant`] records the decision for exactly the plugin list
//!   the prompt showed (its `key`).
//! - Text from the project or the game is untrusted: it reaches the webview only through
//!   [`sanitize`] and is rendered as text, never as HTML.

use beacon_engine::config::Artifact;
use beacon_engine::diag::{Code, Diagnostic};
use beacon_engine::{init, plugin, route, trust};
use serde_json::Value;
use std::path::{Path, PathBuf};

// one file per wave brief, so parallel briefs never share a file
pub mod client;
mod play;
mod settings;
mod setup;
mod updater;

/// Tauri command names (the `generate_handler!` list and the capability allowlist).
pub const COMMANDS: &[&str] = &[
    "app_info",
    "setup_status",
    "setup_run",
    "project_open",
    "trust_prompt",
    "trust_grant",
    "init_questions",
    "init_create",
    "check",
    "play",
    "settings_list",
    "settings_read",
    "settings_write",
    "test_run",
    "update_check",
    "update_install",
    "update_rollback",
];

/// Events pushed to the webview (`app.emit`).
pub mod events {
    /// Payload: `Vec<Diagnostic>` (latest full set for the open project).
    pub const DIAGNOSTICS: &str = "beacon://diagnostics";
    /// Payload: [`super::ContentLogLine`].
    pub const CONTENT_LOG: &str = "beacon://content-log";
    /// Payload: [`super::Progress`] (play, setup, test, update).
    pub const PROGRESS: &str = "beacon://progress";
}

/// Error crossing IPC: a diagnostic code id (`BCN0050`) and its message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
}

impl From<Diagnostic> for AppError {
    fn from(d: Diagnostic) -> Self {
        AppError {
            code: d.code.id().into(),
            // messages quote project files and paths: untrusted
            message: sanitize(&d.message),
        }
    }
}

impl From<Vec<Diagnostic>> for AppError {
    fn from(d: Vec<Diagnostic>) -> Self {
        d.into_iter().next().map_or_else(
            || AppError::new(Code::ConfigInvalid, "unknown error"),
            AppError::from,
        )
    }
}

impl AppError {
    pub fn new(code: Code, message: &str) -> AppError {
        AppError {
            code: code.id().into(),
            message: sanitize(message),
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub engine: String,
    pub protocol: u32,
    /// `windows-x86_64`, `darwin-aarch64`, ... (the updater's platform key).
    pub platform: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrustState {
    Trusted,
    /// Has code (generators, scripts, plugins): show the trust prompt before Play.
    Untrusted,
    /// No code to run: builds untrusted (14.1), no prompt needed.
    NoCode,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub root: PathBuf,
    pub name: String,
    pub trust: TrustState,
    pub targets: Vec<String>,
    /// The target Play builds: the first `dev`-artifact target.
    pub play_target: Option<String>,
}

/// What the trust prompt shows (5.5, 5.10).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustPrompt {
    pub root: PathBuf,
    /// Plain-language list of what will run (sanitized): generators and scripts (bundled with
    /// esbuild), one entry per plugin ("runs arbitrary code" for external processes).
    pub runs: Vec<String>,
    /// The trust key as shown in the details view (every string sanitized).
    pub plugins: Vec<Value>,
    /// The trust key (`plugin::project_trust_key`) the prompt was built from, verbatim.
    /// Opaque to the UI, which passes it back to `trust_grant` unchanged and never renders it:
    /// `trust::trust` records exactly this list, and the store's hash check then fails closed
    /// if the project changed in between (no second read, no separate compare).
    pub key: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayReport {
    pub diagnostics: Vec<Diagnostic>,
    /// `com.mojang` the dev packs went to; `None` when the build failed.
    pub deployed_to: Option<PathBuf>,
    /// The game was asked to start (URI handler); it is not verified to be running.
    pub launched: bool,
}

/// A file the settings forms edit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SettingsFile {
    /// `config.json` (standard fields + the `beacon` key).
    Config,
    /// `<module>/module.json`; `module` is the project-relative module folder.
    Module { module: String },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDoc {
    pub file: SettingsFile,
    /// The parsed file (`{}` when it does not exist yet).
    pub value: Value,
    /// JSON Schema the form is rendered from (Beacon's own for `module.json` / `beacon`).
    pub schema: Value,
    /// blake3 hex of the bytes read; `settings_write` must quote it.
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentLogLine {
    pub level: String,
    /// Already passed through [`sanitize`].
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `play`, `setup`, `test`, `update`.
    pub task: String,
    pub step: String,
    /// 0..=100 when known.
    pub percent: Option<u8>,
}

/// Longest untrusted text forwarded to the webview, in bytes.
pub const MAX_TEXT: usize = 16 << 10;

/// Untrusted text (content log, diagnostics from project files, plugin output) for the
/// webview: ANSI escape sequences and control characters other than `\n` / `\t` removed,
/// capped at [`MAX_TEXT`] bytes on a char boundary.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_TEXT));
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // CSI: ESC [ params final-byte(@..~); other escapes: drop ESC + one char
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() && c != '\n' && c != '\t' {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_TEXT {
            break;
        }
        out.push(c);
    }
    out
}

/// [`sanitize`] for every string inside a JSON value, keys included (plugin kinds and pin ids
/// are project-derived too).
pub fn sanitize_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(sanitize(s)),
        Value::Array(a) => Value::Array(a.iter().map(sanitize_value).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| (sanitize(k), sanitize_value(v)))
                .collect(),
        ),
        v => v.clone(),
    }
}

/// Diagnostics for the webview: messages (project-derived) sanitized.
pub fn sanitize_diagnostics(d: Vec<Diagnostic>) -> Vec<Diagnostic> {
    d.into_iter()
        .map(|mut d| {
            d.message = sanitize(&d.message);
            for f in &mut d.fixes {
                f.description = sanitize(&f.description);
            }
            d
        })
        .collect()
}

/// Restricted-mode host for the trust probe: no JS ever runs (nothing is trusted yet).
struct NoHost;

impl beacon_engine::host::GeneratorHost for NoHost {
    fn run(
        &self,
        _: &beacon_engine::model::Generator,
        _: &beacon_engine::model::GenCtx,
        _: &dyn beacon_engine::path::Vfs,
    ) -> beacon_engine::host::GenResult {
        beacon_engine::host::GenResult {
            items: vec![],
            inputs: Default::default(),
            diagnostics: vec![],
        }
    }
    fn version(&self) -> String {
        "none".into()
    }
}

/// Does the project have code to run (generators, scripts or plugins)? Asked of the engine's
/// own restricted-mode check, which reports `Untrusted` exactly then (build.rs).
fn has_code(root: &Path) -> bool {
    use beacon_engine::build::{BuildOptions, Session};
    let mut s = Session::new(root, BuildOptions::default(), Box::new(NoHost), None);
    s.check()
        .diagnostics
        .iter()
        .any(|d| d.code == Code::Untrusted)
}

/// The app's in-process core. One per app window.
pub struct AppCore {
    /// The bundled `beacon` binary (sidecar): daemon command, `setup`, `test`.
    pub beacon_exe: PathBuf,
}

impl AppCore {
    pub fn new(beacon_exe: PathBuf) -> AppCore {
        AppCore { beacon_exe }
    }

    pub fn app_info(&self) -> AppInfo {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            o => o,
        };
        AppInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            engine: crate::ENGINE_VERSION.into(),
            protocol: crate::PROTOCOL_VERSION,
            platform: format!("{os}-{}", std::env::consts::ARCH),
        }
    }

    pub fn project_open(&self, root: PathBuf) -> AppResult<ProjectSummary> {
        let cfg = route::load(&root)?;
        let root = crate::discovery::canonical_root(&root);
        let trust = if trust::is_trusted(&root, &plugin::project_trust_key(&root)) {
            TrustState::Trusted
        } else if has_code(&root) {
            TrustState::Untrusted
        } else {
            TrustState::NoCode
        };
        let play_target = cfg
            .beacon
            .targets
            .iter()
            .find(|(_, t)| t.artifact == Artifact::Dev)
            .map(|(n, _)| sanitize(n));
        Ok(ProjectSummary {
            root,
            name: sanitize(&cfg.name),
            trust,
            targets: cfg.beacon.targets.keys().map(|t| sanitize(t)).collect(),
            play_target,
        })
    }

    pub fn trust_prompt(&self, root: PathBuf) -> AppResult<TrustPrompt> {
        route::load(&root)?;
        let root = crate::discovery::canonical_root(&root);
        let key = plugin::project_trust_key(&root);
        let mut runs = vec![];
        if has_code(&root) {
            runs.push(
                "Generators and scripts of this project (scripts are bundled with esbuild)".into(),
            );
        }
        for p in key.iter().filter(|p| p.get("pins").is_none()) {
            runs.push(match p.as_object().and_then(|o| o.keys().next()) {
                Some(k) => sanitize(&format!(
                    "Plugin `{k}`: runs arbitrary code on this computer"
                )),
                None => "A plugin: runs arbitrary code on this computer".into(),
            });
        }
        Ok(TrustPrompt {
            root,
            runs,
            plugins: key.iter().map(sanitize_value).collect(),
            key,
        })
    }

    /// Records trust for exactly the list the prompt showed (`shown` = `TrustPrompt::key`).
    /// If the project changed since, that list no longer matches its current key: nothing is
    /// written and the current decision stands; the returned state says which it is and
    /// the UI shows the prompt again unless it is `Trusted`.
    pub fn trust_grant(&self, root: PathBuf, shown: Vec<Value>) -> AppResult<TrustState> {
        let root = crate::discovery::canonical_root(&root);
        let current = plugin::project_trust_key(&root);
        // a stale click writes nothing: it cannot untrust an already-trusted project
        if trust::key_hash(&shown) == trust::key_hash(&current) {
            trust::trust(&root, &shown).map_err(|e| {
                AppError::new(Code::EmitFailed, &format!("cannot record trust: {e}"))
            })?;
        }
        Ok(if trust::is_trusted(&root, &current) {
            TrustState::Trusted
        } else {
            TrustState::Untrusted
        })
    }

    /// `init::questions()` (the same spec as `beacon init --describe --json`).
    pub fn init_questions(&self) -> Value {
        beacon_engine::init::questions()
    }

    /// `init::Answers` as JSON (`folder` required); returns the new project's summary
    /// (built-in template = trusted by `init::scaffold`). `gitInit` is not run by the app.
    pub fn init_create(&self, answers: Value) -> AppResult<ProjectSummary> {
        let a: init::Answers = serde_json::from_value(answers)
            .map_err(|e| AppError::new(Code::ConfigField, &format!("bad answers: {e}")))?;
        let dir = PathBuf::from(
            a.folder
                .clone()
                .ok_or_else(|| AppError::new(Code::ConfigField, "folder is required"))?,
        );
        let name = std::path::absolute(&dir)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "project".into());
        let plan = init::resolve(a, &name)?;
        init::scaffold(&dir, &plan)?;
        self.project_open(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_escapes_and_caps() {
        assert_eq!(
            sanitize("\u{1b}[31mred\u{1b}[0m ok\r\n\ttab\u{7}"),
            "red ok\n\ttab"
        );
        let long = "é".repeat(MAX_TEXT);
        let s = sanitize(&long);
        assert!(s.len() <= MAX_TEXT && s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn command_names_unique() {
        let mut v = COMMANDS.to_vec();
        v.sort();
        v.dedup();
        assert_eq!(v.len(), COMMANDS.len());
    }
}
