//! Move / Copy end to end: runs the real (debug) Mori with an isolated HOME
//! and `MORI_DEBUG_TRANSFER`, which drives the transfer commands over a
//! scratch tree (see `debug_transfer` in main.rs), then checks what happened
//! on disk and in Mori's index.
//!
//! Covers: conflicts are never overwritten, Keep Both, Replace (the existing
//! file goes to the Trash) and its Undo, folder copies, links copied as
//! links, Read-only Mode and protected folders refusing, a partial failure,
//! a destination outside the browsed folder, favorites following a move,
//! and Quick Cleanup sessions never being saved in a temporary session.
//!
//! macOS only (it opens Mori's window for a few seconds; Replace + Undo
//! round-trips through the system Trash). Set `MORI_SKIP_GUI_TESTS=1` to skip.

#![cfg(target_os = "macos")]

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn move_and_copy_through_the_real_commands() {
    if std::env::var_os("MORI_SKIP_GUI_TESTS").is_some() {
        eprintln!("skipped (MORI_SKIP_GUI_TESTS)");
        return;
    }
    let base = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-transfer-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let tmp = base.join("tmp");
    let tree = base.join("Tree");
    let outside = base.join("Outside");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&tmp).unwrap();

    let out_file = base.join("stderr.txt");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mori"))
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("MORI_DEBUG_TRANSFER", &tree)
        .env("MORI_DEBUG_TRANSFER_OUT", &outside)
        .stderr(fs::File::create(&out_file).unwrap())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            panic!("Mori didn't finish:\n{}", fs::read_to_string(&out_file).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let log = fs::read_to_string(&out_file).unwrap_or_default();
    let has = |s: &str| assert!(log.contains(s), "missing “{s}” in:\n{log}");

    has("DEBUG transfer done");
    has("transfer plan: conflict=Some(\"file\") replaceable=true free=None guarded-blocked=true");
    has("transfer refusals: into-itself=true already-here=true");
    // The conflicting file was skipped, not overwritten; the other one moved and the index followed.
    has("transfer move: done=1 skipped=1 other-a=other-a moved-b=album-b index-old=false index-new=true");
    has("transfer favorite-follows=true");
    has("transfer keep-both: done=1 copy=album-a original=album-a indexed=true");
    // Replace sent the existing file to the Trash; Undo restored both.
    has("transfer replace: done=1 now=album-a undo=2 back-album=album-a back-other=other-a");
    has("transfer folder-copy: done=1 nested=sub-c source-kept=sub-c indexed=true link-is-link=true");
    has("transfer policy: read-only=true protected=true created=false");
    has("transfer partial: done=1 failed=1 moved=album-b");
    has("transfer outside: done=1 new-id=false arrived=sub-c left-index=true");
    has("transfer cleanup-store: saved=true resumed=true discarded=true temp-refused=true temp-none=true");

    // Nothing was left in the Trash's place: the Replace was undone.
    assert_eq!(fs::read(tree.join("Other/a.jpg")).unwrap(), b"other-a");
    let _ = fs::remove_dir_all(&base);
}
