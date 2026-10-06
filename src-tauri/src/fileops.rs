//! The only operations through which Mori changes the filesystem:
//! moving items to the OS Trash / Recycle Bin, and renaming without ever
//! overwriting. Every mutation takes a `&Policy` (read-only mode, protected
//! folders) and checks it first.

use crate::policy::{Op, Policy};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Move a file or folder to the OS Trash (macOS) / Recycle Bin (Windows) /
/// freedesktop trash (Linux). Recoverable by the user. Never falls back to a
/// permanent delete: if the platform refuses, the item stays where it is.
pub fn move_to_trash(policy: &Policy, path: &Path) -> Result<(), String> {
    policy.check(Op::Trash, path)?;
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

/// Create a new file `name` in the canonical folder `dir` with `bytes`.
/// Never replaces anything: fails if the name exists (even as a dangling
/// symlink), and never follows a symlink in place of the folder or the file.
/// Returns `Ok(false)` when the name is taken, so the caller can pick another.
pub fn create_new(policy: &Policy, dir: &Path, name: &str, bytes: &[u8]) -> Result<bool, String> {
    validate_name(name)?;
    let target = dir.join(name);
    policy.check(Op::Create, &target)?;
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::io::Write;
        use std::os::fd::FromRawFd;
        use std::os::unix::ffi::OsStrExt;
        let cdir = CString::new(dir.as_os_str().as_bytes()).map_err(|_| "invalid folder")?;
        let cname = CString::new(name.as_bytes()).map_err(|_| "invalid name")?;
        let dfd = unsafe {
            libc::open(cdir.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        };
        if dfd < 0 {
            return Err("the folder can't be opened".into());
        }
        let fd = unsafe {
            libc::openat(
                dfd,
                cname.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644 as libc::c_uint,
            )
        };
        let err = std::io::Error::last_os_error();
        unsafe { libc::close(dfd) };
        if fd < 0 {
            return match err.kind() {
                std::io::ErrorKind::AlreadyExists => Ok(false),
                std::io::ErrorKind::PermissionDenied => Err("permission denied".into()),
                _ => Err("the file couldn't be created".into()),
            };
        }
        let mut f = unsafe { fs::File::from_raw_fd(fd) };
        if f.write_all(bytes).and_then(|_| f.sync_all()).is_err() {
            drop(f);
            // Our own partial file: remove it rather than leave a broken copy.
            let _ = fs::remove_file(&target);
            return Err("writing the file failed (disk full?)".into());
        }
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        let meta = fs::symlink_metadata(dir).map_err(|_| "the folder can't be opened")?;
        if !meta.is_dir() {
            return Err("the folder can't be opened".into());
        }
        let mut f = match fs::OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(_) => return Err("the file couldn't be created".into()),
        };
        if f.write_all(bytes).and_then(|_| f.sync_all()).is_err() {
            drop(f);
            let _ = fs::remove_file(&target);
            return Err("writing the file failed (disk full?)".into());
        }
        Ok(true)
    }
}

/// Resolve an existing item for a filesystem change. Unlike reading, this
/// never follows a final symlink: a link is acted on as itself (its target
/// is never touched). The item must be a plain file, directory or link
/// strictly inside `root` (never the root).
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
    if !(ft.is_file() || ft.is_dir() || ft.is_symlink()) {
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
pub fn rename_no_replace(policy: &Policy, from: &Path, to: &Path) -> Result<(), String> {
    policy.check(Op::Rename, from)?;
    policy.check(Op::Create, to)?;
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
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        assert!(rename_no_replace(&p, &d.join("a.txt"), &d.join("b.txt")).is_err());
        assert_eq!(fs::read(d.join("b.txt")).unwrap(), b"b", "target untouched");
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(rename_no_replace(&ro, &d.join("a.txt"), &d.join("c.txt")).unwrap_err(), crate::policy::READ_ONLY);
        rename_no_replace(&p, &d.join("a.txt"), &d.join("c.txt")).unwrap();
        assert!(d.join("c.txt").exists() && !d.join("a.txt").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn create_new_never_replaces_or_follows_links() {
        let d = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-create-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("real")).unwrap();
        fs::write(d.join("taken.jpg"), b"original").unwrap();
        let outside = d.join("outside.txt");
        std::os::unix::fs::symlink(&outside, d.join("link.jpg")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("dirlink")).unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        assert_eq!(create_new(&p, &d, "taken.jpg", b"new"), Ok(false));
        assert_eq!(fs::read(d.join("taken.jpg")).unwrap(), b"original");
        assert_eq!(create_new(&p, &d, "link.jpg", b"new"), Ok(false), "a dangling link counts as taken");
        assert!(!outside.exists(), "the link target was never created");
        assert!(create_new(&p, &d.join("dirlink"), "x.jpg", b"new").is_err(), "a linked folder is refused");
        assert!(create_new(&p, &d, "../escape.jpg", b"new").is_err());
        assert_eq!(create_new(&p, &d, "fresh.jpg", b"new"), Ok(true));
        assert_eq!(fs::read(d.join("fresh.jpg")).unwrap(), b"new");
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(create_new(&ro, &d, "other.jpg", b"x").unwrap_err(), crate::policy::READ_ONLY);
        assert!(!d.join("other.jpg").exists());
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
        let (p, m) = confined_item(&root, "link.txt").unwrap();
        assert!(m.file_type().is_symlink() && p.ends_with("link.txt"), "a link is the item itself, never its target");
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
        let store = crate::privacy::Store::load(std::env::temp_dir().join("mori-trash-test-protected.json"));
        let p = Policy { read_only: false, protected: &store };
        move_to_trash(&p, &d.join("file.txt")).unwrap();
        move_to_trash(&p, &d.join("folder")).unwrap();
        assert!(!d.join("file.txt").exists() && !d.join("folder").exists());
        assert!(move_to_trash(&p, &d.join("missing.txt")).is_err(), "a missing item is an error, not a silent success");
        // A link goes to the Trash as itself; its target is untouched.
        fs::write(d.join("target.txt"), b"keep").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("target.txt"), d.join("link.txt")).unwrap();
            move_to_trash(&p, &d.join("link.txt")).unwrap();
            assert!(fs::symlink_metadata(d.join("link.txt")).is_err());
            assert_eq!(fs::read(d.join("target.txt")).unwrap(), b"keep");
        }
        fs::remove_file(d.join("target.txt")).unwrap();
        fs::remove_dir(&d).unwrap();
    }
}
