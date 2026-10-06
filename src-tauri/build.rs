// Every IPC command must be explicitly granted in capabilities/default.json;
// anything not listed here cannot be invoked from the UI at all.
const COMMANDS: &[&str] = &[
    "init",
    "update_settings",
    "choose_root",
    "rescan",
    "clear_cache",
    "get_status",
    "query",
    "stats",
    "subfolders",
    "inspect",
    "preview_info",
    "read_text",
    "open_file",
    "reveal_file",
    "copy_path",
    "store_frame",
    "video_session_start",
    "ui_alive",
    "video_session_end",
    "take_recovered",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
