// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Glue only: every `#[tauri::command]` forwards to the `beacon_serve::app::AppCore` method of
//! the same name (tested without a webview in `crates/serve/tests/app.rs`). The set is
//! `beacon_serve::app::COMMANDS`; `capabilities/default.json` allows exactly that set.
//!
//! Commands run on Tauri's thread pool (`async`), and a method still `todo!()` (later briefs)
//! comes back as an error instead of taking the app down.
//!
//! esbuild: the installer ships it as a second externalBin next to `beacon`. On first launch
//! it must be copied into the tools dir under the name and with the `.blake3` sidecar the
//! bundler's `download::verified` expects: `AppCore::first_launch` does it (hash-checked, see
//! `crates/serve/src/app/setup.rs`).

mod smoke;

use beacon_serve::app::{
    AppCore, AppError, AppInfo, AppResult, PlayReport, ProjectSummary, SettingsDoc, SettingsFile,
    TrustPrompt, TrustState,
};
use beacon_serve::update::Decision;
use serde_json::Value;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tauri::{Emitter, Manager, State};

/// Stop flag of the running content-log tail (one at a time): set by the next Play and when
/// the window is destroyed.
#[derive(Default)]
struct TailStop(Mutex<Arc<AtomicBool>>);

impl TailStop {
    /// Stops the current tail and returns the flag for a new one.
    fn renew(&self) -> Arc<AtomicBool> {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.store(true, Ordering::Relaxed);
        *g = Arc::new(AtomicBool::new(false));
        g.clone()
    }
}

fn guard<T>(f: impl FnOnce() -> AppResult<T>) -> AppResult<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(AppError {
            code: "internal".into(),
            message: "this action is not available in this build yet".into(),
        })
    })
}

#[tauri::command]
fn app_info(core: State<AppCore>) -> AppInfo {
    core.app_info()
}

#[tauri::command(async)]
fn setup_status(core: State<AppCore>) -> AppResult<beacon_engine::setup::SetupReport> {
    guard(|| core.setup_status())
}

#[tauri::command(async)]
fn setup_run(
    app: tauri::AppHandle,
    core: State<AppCore>,
    accepted: Vec<beacon_engine::setup::Tool>,
    accept_eula: bool,
) -> AppResult<beacon_engine::setup::SetupReport> {
    guard(|| {
        core.setup_run_with(&accepted, accept_eula, &mut |p| {
            let _ = app.emit(beacon_serve::app::events::PROGRESS, p);
        })
    })
}

#[tauri::command(async)]
fn project_open(core: State<AppCore>, root: PathBuf) -> AppResult<ProjectSummary> {
    guard(|| core.project_open(root))
}

#[tauri::command(async)]
fn trust_prompt(core: State<AppCore>, root: PathBuf) -> AppResult<TrustPrompt> {
    guard(|| core.trust_prompt(root))
}

#[tauri::command(async)]
fn trust_grant(core: State<AppCore>, root: PathBuf, shown: Vec<Value>) -> AppResult<TrustState> {
    guard(|| core.trust_grant(root, shown))
}

#[tauri::command]
fn init_questions(core: State<AppCore>) -> Value {
    core.init_questions()
}

#[tauri::command(async)]
fn init_create(core: State<AppCore>, answers: Value) -> AppResult<ProjectSummary> {
    guard(|| core.init_create(answers))
}

#[tauri::command(async)]
fn check(core: State<AppCore>, root: PathBuf) -> AppResult<Vec<beacon_engine::diag::Diagnostic>> {
    guard(|| core.check(root))
}

#[tauri::command(async)]
fn play(
    app: tauri::AppHandle,
    core: State<AppCore>,
    tail: State<TailStop>,
    root: PathBuf,
    target: Option<String>,
) -> AppResult<PlayReport> {
    let stop = tail.renew(); // a new Play ends the previous tail
    let t0 = SystemTime::now();
    let report = guard(|| core.play(root.clone(), target.clone()))?;
    if report.deployed_to.is_some() {
        std::thread::spawn(move || {
            app.state::<AppCore>()
                .content_log_tail(root, target, t0, &stop, |l| {
                    let _ = app.emit(beacon_serve::app::events::CONTENT_LOG, l);
                });
        });
    }
    Ok(report)
}

#[tauri::command(async)]
fn settings_list(core: State<AppCore>, root: PathBuf) -> AppResult<Vec<String>> {
    guard(|| core.settings_list(root))
}

#[tauri::command(async)]
fn settings_read(
    core: State<AppCore>,
    root: PathBuf,
    file: SettingsFile,
) -> AppResult<SettingsDoc> {
    guard(|| core.settings_read(root, file))
}

#[tauri::command(async)]
fn settings_write(
    core: State<AppCore>,
    root: PathBuf,
    file: SettingsFile,
    base: String,
    value: Value,
) -> AppResult<SettingsDoc> {
    guard(|| core.settings_write(root, file, base, value))
}

#[tauri::command(async)]
fn test_run(
    core: State<AppCore>,
    root: PathBuf,
    opts: beacon_engine::gametest::TestOptions,
) -> AppResult<beacon_engine::gametest::TestReport> {
    guard(|| core.test_run(root, opts))
}

#[tauri::command(async)]
fn update_check(core: State<AppCore>) -> AppResult<Decision> {
    guard(|| core.update_check())
}

#[tauri::command(async)]
fn update_install(core: State<AppCore>, version: String) -> AppResult<()> {
    guard(|| core.update_install(version))
}

#[tauri::command(async)]
fn update_rollback(core: State<AppCore>) -> AppResult<()> {
    guard(|| core.update_rollback())
}

/// The bundled `beacon` sidecar: Tauri installs an externalBin next to the app executable
/// (triple suffix stripped). It starts the project daemons; never `current_exe` (this app).
fn beacon_exe() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.with_file_name(if cfg!(windows) {
        "beacon.exe"
    } else {
        "beacon"
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--smoke") {
        std::process::exit(smoke::run(&args));
    }
    tauri::Builder::default()
        .manage(AppCore::new(beacon_exe()).first_launch())
        .manage(TailStop::default())
        .on_window_event(|w, e| {
            if matches!(e, tauri::WindowEvent::Destroyed) {
                w.state::<TailStop>().renew();
            }
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            setup_status,
            setup_run,
            project_open,
            trust_prompt,
            trust_grant,
            init_questions,
            init_create,
            check,
            play,
            settings_list,
            settings_read,
            settings_write,
            test_run,
            update_check,
            update_install,
            update_rollback,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Beacon");
}
