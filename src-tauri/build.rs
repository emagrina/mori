// Every IPC command must be explicitly granted in capabilities/default.json;
// anything not listed here cannot be invoked from the UI at all.
const COMMANDS: &[&str] = &[
    "init",
    "update_settings",
    "saved_language",
    "set_language",
    "review_info",
    "review_state",
    "save_review_state",
    "request_review",
    "open_store_review",
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
    "trash_summary",
    "trash_items",
    "rename_item",
    "analysis_locations",
    "analysis_choose_folder",
    "analysis_start",
    "analysis_cancel",
    "analysis_results",
    "analysis_clear",
    "analysis_cleanup",
    "analysis_cleanup_cancel",
    "similar_start",
    "similar_cancel",
    "similar_frames",
    "similar_results",
    "similar_dismiss",
    "similar_dismissed_count",
    "similar_forget_decisions",
    "similar_clear",
    "similar_cleanup",
    "set_folder_private",
    "file_report",
    "set_read_only",
    "set_folder_protected",
    "pdf_info",
    "archive_listing",
    "open_drive_safely",
    "set_drive_previews",
    "set_capture_override",
    "storage_report",
    "set_favorite",
    "plan_operation",
    "checksum_file",
    "run_diagnostics",
    "local_data",
    "clear_mori_data",
    "reset_mori",
    "checksum_cancel",
    "compare_files",
    "integrity_save",
    "integrity_verify",
    "integrity_pause",
    "integrity_cancel",
    "integrity_list",
    "integrity_delete",
    "delete_items",
    "history_list",
    "history_undo",
    "tags_list",
    "tag_items",
    "tag_rename",
    "tag_delete",
    "open_temporary",
    "end_temporary",
    "start_private_inspection",
    "forget_drive",
    "clear_session_data",
    "empty_folders",
    "trash_empty_folders",
    "health_start",
    "health_pause",
    "health_cancel",
    "health_clear",
    "health_results",
    "file_metadata",
    "meta_start",
    "meta_pause",
    "meta_cancel",
    "meta_clear",
    "meta_results",
    "meta_places",
    "sanitize_copies",
    "plan_transfer",
    "transfer_items",
    "transfer_cancel",
    "transfer_choose_folder",
    "cleanup_saved",
    "cleanup_save",
    "cleanup_discard",
];

fn main() {
    // A signed universal decoder has passed the real App Sandbox/XPC smoke tests.
    // Retain a fail-closed release gate: missing, stale or over-entitled services
    // cannot be packaged. There is no environment-variable bypass.
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_MAC_APP_STORE");
    println!("cargo:rerun-if-env-changed=PROFILE");
    if std::env::var_os("CARGO_FEATURE_MAC_APP_STORE").is_some() {
        for path in [
            "xpc",
            "decoder/Cargo.toml",
            "decoder/Cargo.lock",
            "decoder/build.rs",
            "decoder/src",
            "src/worker.rs",
            "src/heif.rs",
            "src/pdf.rs",
            "src/metadata.rs",
            "src/sanitize.rs",
            "src/probe.rs",
            "src/i18n.rs",
            "locales",
            "target/store-decoder",
        ] {
            println!("cargo:rerun-if-changed={path}");
        }
        if std::env::var("PROFILE").as_deref() != Ok("debug") {
            let status = std::process::Command::new("python3")
                .args(["xpc/verify-decoder.py", "--prepared"])
                .status()
                .expect("MORI_STORE_DECODER_NOT_PREPARED: Python 3 is required to verify the decoder");
            assert!(status.success(), "MORI_STORE_DECODER_NOT_PREPARED: run src-tauri/xpc/prepare-decoder.sh before building Store release; never package without the verified XPC service");
        }
    }
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"decoder-service\"))");
    if std::env::var_os("CARGO_FEATURE_MAC_APP_STORE").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        cc::Build::new()
            .file("xpc/bridge.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .flag("-mmacosx-version-min=12.0")
            .compile("mori_xpc");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=Security");
        println!("cargo:rerun-if-changed=xpc/bridge.m");
    }
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
