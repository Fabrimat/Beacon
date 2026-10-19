fn main() {
    // One source for the IPC surface: beacon_serve::app::COMMANDS.
    let app = tauri_build::AppManifest::new().commands(beacon_serve::app::COMMANDS);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(app)).expect("tauri build");
}
