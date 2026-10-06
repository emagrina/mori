//! The only operations through which Mori changes the filesystem:
//! moving items to the OS Trash / Recycle Bin, and renaming without ever
//! overwriting. There is deliberately no permanent-delete function.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Move a file or folder to the OS Trash (macOS) / Recycle Bin (Windows) /
/// freedesktop trash (Linux). Recoverable by the user. Never falls back to a
/// permanent delete: if the platform refuses, the item stays where it is.
pub fn move_to_trash(path: &Path) -> Result<(), String> {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        // NSFileManager.trashItemAtURL: no AppleScript/Finder automation prompt.
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx.delete(path).map_err(|e| describe_trash_error(&e))
}

fn describe_trash_error(e: &trash::Error) -> String {
    let s = e.to_string();
    if s.contains("ermission") {
        "permission denied".into()
    } else if s.contains("not found") || s.contains("No such file") {
        "file no longer exists".into()
    } else {
        "the system Trash refused the item".into()
    }
}

/// Resolve an existing item for a filesystem change. Unlike reading, this
/// must not follow a final symlink (we act on the entry itself) and the item
/// must be a plain file or directory strictly inside `root` (never the root).
pub fn confined_item(root: &Path, rel: &str) -> Result<(PathBuf, fs::Metadata), String> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err("invalid path".into());
    }
    let parent = rel_path.parent().unwrap_or(Path::new(""));
    let name = rel_path.file_name().ok_or("invalid path")?;
    // Canonicalise the *parent* (resolving any symlinked directories) and make
    // sure it is still inside the root; then look at the entry itself.
    let parent_canon = fs::canonicalize(root.join(parent)).map_err(|_| "item unavailable")?;
    if !parent_canon.starts_with(root) {
        return Err("item is outside the allowed folder".into());
    }
    let path = parent_canon.join(name);
    let meta = fs::symlink_metadata(&path).map_err(|_| "item no longer exists")?;
    let ft = meta.file_type();
    if !(ft.is_file() || ft.is_dir()) || ft.is_symlink() {
        return Err("not a regular file or folder".into());
    }
    if path == root {
        return Err("refusing to act on the root folder".into());
    }
    Ok((path, meta))
}

/// Validate a user-typed file name. Rejects anything that could change the
/// directory, hide the real extension, or be invalid on common filesystems.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.trim().is_empty() {
        return Err("The name can't be empty.".into());
    }
    if name == "." || name == ".." {
        return Err("That name isn't allowed.".into());
    }
    if name.len() > 255 {
        return Err("The name is too long.".into());
    }
    if name.starts_with('.') {
        return Err("Names starting with a dot would hide the item.".into());
    }
    let bad = |c: char| {
        matches!(c, '/' | '\\' | ':' | '\0' | '<' | '>' | '"' | '|' | '?' | '*')
            || c.is_control()
            || matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
    };
    if name.chars().any(bad) {
        return Err(
            "The name contains characters that aren't allowed (/ \\ : < > \" | ? * or control characters).".into()
        );
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err("The name can't end with a space or a dot.".into());
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return Err("That name is reserved on Windows.".into());
    }
    Ok(())
}

/// Rename `from` to `to` atomically, failing (never overwriting) if `to` exists.
pub fn rename_no_replace(from: &Path, to: &Path) -> Result<(), String> {
    let r = platform_rename_excl(from, to);
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err("An item with that name already exists.".into()),
        Err(_) => Err("The item couldn't be renamed.".into()),
    }
}

#[cfg(target_os = "macos")]
fn platform_rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(from.as_os_str().as_bytes())?;
    let b = CString::new(to.as_os_str().as_bytes())?;
    // RENAME_EXCL: fail with EEXIST instead of replacing the destination.
    let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_EXCL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn platform_rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(from.as_os_str().as_bytes())?;
    let b = CString::new(to.as_os_str().as_bytes())?;
    let rc = unsafe { libc::renameat2(libc::AT_FDCWD, a.as_ptr(), libc::AT_FDCWD, b.as_ptr(), libc::RENAME_NOREPLACE) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn platform_rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    // Flags = 0: no MOVEFILE_REPLACE_EXISTING, so an existing target fails.
    let ok = unsafe { MoveFileExW(wide(from).as_ptr(), wide(to).as_ptr(), 0) };
    if ok != 0 {
        Ok(())
    } else {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(183) || e.raw_os_error() == Some(80) {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, e));
        }
        Err(e)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn platform_rename_excl(from: &Path, to: &Path) -> std::io::Result<()> {
    if to.exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "exists"));
    }
    fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(validate_name("holiday 2024.jpg").is_ok());
        assert!(validate_name("cumpleaños.heic").is_ok());
        for bad in [
            "",
            " ",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            ".hidden",
            "x\u{202E}gpj.exe",
            "name.",
            "name ",
            "CON",
            "com1.txt",
            "a\nb",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn rename_never_overwrites() {
        let d = std::env::temp_dir().join(format!("mori-rename-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("a.txt"), b"a").unwrap();
        fs::write(d.join("b.txt"), b"b").unwrap();
        assert!(rename_no_replace(&d.join("a.txt"), &d.join("b.txt")).is_err());
        assert_eq!(fs::read(d.join("b.txt")).unwrap(), b"b", "target untouched");
        rename_no_replace(&d.join("a.txt"), &d.join("c.txt")).unwrap();
        assert!(d.join("c.txt").exists() && !d.join("a.txt").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn confinement_rejects_escapes_and_symlinks() {
        let base = std::env::temp_dir().join(format!("mori-conf-{}", std::process::id()));
        let root = base.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(base.join("outside.txt"), b"x").unwrap();
        fs::write(root.join("sub/in.txt"), b"x").unwrap();
        std::os::unix::fs::symlink(base.join("outside.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&base, root.join("escape")).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        assert!(confined_item(&root, "sub/in.txt").is_ok());
        assert!(confined_item(&root, "sub").is_ok());
        assert!(confined_item(&root, "../outside.txt").is_err());
        assert!(confined_item(&root, "link.txt").is_err(), "symlink itself is not acted on");
        assert!(confined_item(&root, "escape/outside.txt").is_err(), "symlinked parent escaping the root");
        assert!(confined_item(&root, "").is_err());
        fs::remove_dir_all(&base).unwrap();
    }

    /// Uses the real system Trash, so it only runs on request:
    /// `MORI_TRASH_TEST_DIR=/Volumes/Drive cargo test -- --ignored trash_`
    #[test]
    #[ignore]
    fn trash_moves_items_to_the_system_trash() {
        let base = std::env::var_os("MORI_TRASH_TEST_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("mori-trash-test-{}", std::process::id()));
        fs::create_dir_all(d.join("folder")).unwrap();
        fs::write(d.join("file.txt"), b"x").unwrap();
        fs::write(d.join("folder/inner.txt"), b"y").unwrap();
        move_to_trash(&d.join("file.txt")).unwrap();
        move_to_trash(&d.join("folder")).unwrap();
        assert!(!d.join("file.txt").exists() && !d.join("folder").exists());
        assert!(move_to_trash(&d.join("missing.txt")).is_err(), "a missing item is an error, not a silent success");
        fs::remove_dir(&d).unwrap();
    }
}
