//! The only operations through which Mori changes the filesystem: moving
//! items to the OS Trash / Recycle Bin (and restoring them), renaming and
//! moving without ever overwriting, copying and creating new files, and —
//! only on explicit, confirmed request — deleting permanently. Every mutation takes a `&Policy`
//! (read-only mode, protected folders) and checks it first. Symbolic links
//! are always acted on as themselves; their targets are never touched.

use crate::policy::{Op, Policy};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Move a file or folder to the OS Trash (macOS) / Recycle Bin (Windows) /
/// freedesktop trash (Linux). Recoverable by the user. Never falls back to a
/// permanent delete: if the platform refuses, the item stays where it is.
/// Returns where the item now is in the Trash when the platform says so
/// (macOS), which makes Undo possible.
pub fn move_to_trash(policy: &Policy, path: &Path) -> Result<Option<PathBuf>, String> {
    policy.check(Op::Trash, path)?;
    #[cfg(target_os = "macos")]
    {
        macos_trash(path).map(Some)
    }
    #[cfg(not(target_os = "macos"))]
    {
        trash::TrashContext::default().delete(path).map(|_| None).map_err(|e| describe_trash_error(&e))
    }
}

/// NSFileManager `trashItemAtURL:resultingItemURL:error:` (no Finder
/// automation prompt); the resulting URL is the item's place in the Trash.
#[cfg(target_os = "macos")]
fn macos_trash(path: &Path) -> Result<PathBuf, String> {
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if fs::symlink_metadata(path).is_err() {
        return Err("file no longer exists".into());
    }
    let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "invalid path")?;
    objc2::rc::autoreleasepool(|_| unsafe {
        let s: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()];
        if s.is_null() {
            return Err("invalid path".to_string());
        }
        let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: s];
        let fm: *mut AnyObject = msg_send![class!(NSFileManager), defaultManager];
        let mut out: *mut AnyObject = std::ptr::null_mut();
        let mut err: *mut AnyObject = std::ptr::null_mut();
        let ok: bool = msg_send![fm, trashItemAtURL: url, resultingItemURL: &mut out, error: &mut err];
        if !ok {
            let code: isize = if err.is_null() { 0 } else { msg_send![err, code] };
            // NSFileWriteNoPermissionError = 513, NSFileNoSuchFileError = 4.
            return Err(match code {
                513 | 257 => "permission denied".into(),
                4 => "file no longer exists".into(),
                _ => "the system Trash refused the item".into(),
            });
        }
        if out.is_null() {
            return Ok(PathBuf::new());
        }
        let p: *mut AnyObject = msg_send![out, path];
        let cs: *const std::ffi::c_char =
            if p.is_null() { std::ptr::null() } else { msg_send![p, fileSystemRepresentation] };
        if cs.is_null() {
            return Ok(PathBuf::new());
        }
        Ok(PathBuf::from(std::ffi::OsString::from_vec(CStr::from_ptr(cs).to_bytes().to_vec())))
    })
}

/// Put an item Mori moved to the Trash back where it was. Refuses if the
/// original place is taken (never overwrites) or the item is no longer in
/// the Trash.
pub fn restore_from_trash(policy: &Policy, trashed: &Path, original: &Path) -> Result<(), String> {
    policy.check(Op::Restore, original)?;
    fs::symlink_metadata(trashed).map_err(|_| "it is no longer in the Trash".to_string())?;
    if fs::symlink_metadata(original).is_ok() {
        return Err("something with the same name is already there".into());
    }
    let parent = original.parent().ok_or("invalid path")?;
    if !fs::symlink_metadata(parent).is_ok_and(|m| m.is_dir()) {
        return Err("its folder no longer exists".into());
    }
    platform_rename_excl(trashed, original).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "something with the same name is already there".to_string()
        } else {
            "the item couldn't be moved back".to_string()
        }
    })
}

/// Delete permanently — no Trash, not recoverable by Mori. Only on explicit,
/// confirmed request. A link is removed as itself (its target is never
/// touched); a folder is removed with everything in it without following
/// any link inside. Returns the bytes freed (link and folder entries count 0).
pub fn delete_permanently(policy: &Policy, path: &Path) -> Result<u64, String> {
    policy.check(Op::Delete, path)?;
    let meta = fs::symlink_metadata(path).map_err(|_| "file no longer exists".to_string())?;
    let ft = meta.file_type();
    if ft.is_symlink() || ft.is_file() {
        fs::remove_file(path).map_err(|e| io_reason(&e))?;
        return Ok(if ft.is_file() { meta.len() } else { 0 });
    }
    if !ft.is_dir() {
        return Err("not a regular file or folder".into());
    }
    let mut bytes = 0;
    for e in walkdir::WalkDir::new(path).follow_links(false).into_iter().flatten() {
        if e.file_type().is_file() {
            bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    // std's remove_dir_all never follows symlinks (it unlinks them) and
    // guards against symlink races on unix.
    fs::remove_dir_all(path).map_err(|e| io_reason(&e))?;
    Ok(bytes)
}

fn io_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        std::io::ErrorKind::NotFound => "file no longer exists".into(),
        _ => "the item couldn't be deleted".into(),
    }
}

#[cfg(not(target_os = "macos"))]
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

// ------------------------------------------------------------ move & copy

/// Deepest folder tree a copy descends into (like the index's own limit).
const MAX_COPY_DEPTH: usize = 64;

/// `dest` is `src` itself or somewhere inside it (moving or copying a
/// folder into itself would never end).
pub fn is_within(dest: &Path, src: &Path) -> bool {
    dest == src || dest.starts_with(src)
}

/// A name for `name` that is free in `dir`, Finder-style: "photo.jpg" →
/// "photo 2.jpg", "photo 3.jpg"… (folders and extensionless names get the
/// number at the end). `taken` says whether a candidate is already used.
pub fn free_name(name: &str, is_dir: bool, taken: &dyn Fn(&str) -> bool) -> Option<String> {
    if !taken(name) {
        return Some(name.to_owned());
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    // "photo 2.jpg" kept again becomes "photo 3.jpg", not "photo 2 2.jpg".
    let stem = match stem.rsplit_once(' ') {
        Some((base, n)) if !base.is_empty() && n.len() <= 4 && n.parse::<u32>().is_ok_and(|n| n >= 2) => base,
        _ => stem,
    };
    (2..10_000).map(|n| format!("{stem} {n}{ext}")).find(|c| c.len() <= 255 && !taken(c))
}

/// How a move was carried out.
#[derive(Debug, PartialEq)]
pub enum Moved {
    /// Renamed in place (same volume): atomic, nothing copied.
    Renamed,
    /// Another volume: copied and verified, then the original went to the
    /// Trash (`Some` = where, when the platform says so). Never a permanent
    /// delete of the original.
    Copied { bytes: u64, trashed: Option<PathBuf> },
    /// Copied and verified, but the original couldn't be moved to the Trash:
    /// both now exist. The reason is reported, nothing is lost.
    CopiedOriginalKept { bytes: u64, reason: String },
}

/// Move `src` (a file, folder or link, acted on as itself) to the new path
/// `dest` (which must not exist). Same volume: an atomic no-replace rename.
/// Across volumes: a verified copy, then the original to the Trash.
/// `cancel` is polled between files of a cross-volume copy.
pub fn move_item(policy: &Policy, src: &Path, dest: &Path, cancel: &dyn Fn() -> bool) -> Result<Moved, String> {
    policy.check(Op::Move, src)?;
    policy.check(Op::Create, dest)?;
    if is_within(dest, src) {
        return Err("A folder can't be moved into itself.".into());
    }
    fs::symlink_metadata(src).map_err(|_| "item no longer exists".to_string())?;
    match platform_rename_excl(src, dest) {
        Ok(()) => Ok(Moved::Renamed),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(TAKEN.into()),
        Err(e) if cross_device(&e) => {
            let bytes = copy_checked(src, dest, cancel)?;
            match move_to_trash(policy, src) {
                Ok(trashed) => Ok(Moved::Copied { bytes, trashed }),
                Err(reason) => Ok(Moved::CopiedOriginalKept { bytes, reason }),
            }
        }
        Err(e) => Err(transfer_reason(&e)),
    }
}

/// Copy `src` (file, folder or link) to the new path `dest`, which must not
/// exist. Links are recreated as links (what they point to is never read or
/// copied). Nothing is ever replaced; a copy that fails or is cancelled
/// half-way is removed again (only what this copy created). Returns bytes.
pub fn copy_item(policy: &Policy, src: &Path, dest: &Path, cancel: &dyn Fn() -> bool) -> Result<u64, String> {
    policy.check(Op::Create, dest)?;
    if is_within(dest, src) {
        return Err("A folder can't be copied into itself.".into());
    }
    copy_checked(src, dest, cancel)
}

const TAKEN: &str = "An item with that name is already there.";

fn cross_device(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE: MoveFileExW without MOVEFILE_COPY_ALLOWED.
        e.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

fn transfer_reason(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        std::io::ErrorKind::NotFound => "the item or the destination is no longer available".into(),
        std::io::ErrorKind::AlreadyExists => TAKEN.into(),
        std::io::ErrorKind::StorageFull => "the destination is full".into(),
        std::io::ErrorKind::ReadOnlyFilesystem => "the destination is read-only".into(),
        _ => "the item couldn't be transferred".into(),
    }
}

/// Copy, then check every regular file arrived with its full size; undo the
/// partial copy on any failure.
fn copy_checked(src: &Path, dest: &Path, cancel: &dyn Fn() -> bool) -> Result<u64, String> {
    let mut created = Vec::new();
    let r = copy_tree(src, dest, 0, cancel, &mut created);
    if r.is_err() {
        // Only our own new items, newest first (files before their folders).
        for p in created.iter().rev() {
            match fs::symlink_metadata(p) {
                Ok(m) if m.is_dir() => {
                    let _ = fs::remove_dir(p);
                }
                Ok(_) => {
                    let _ = fs::remove_file(p);
                }
                Err(_) => {}
            }
        }
    }
    r
}

fn copy_tree(
    src: &Path,
    dest: &Path,
    depth: usize,
    cancel: &dyn Fn() -> bool,
    created: &mut Vec<PathBuf>,
) -> Result<u64, String> {
    if cancel() {
        return Err("cancelled".into());
    }
    if depth > MAX_COPY_DEPTH {
        return Err("the folder is nested too deeply to copy".into());
    }
    let meta = fs::symlink_metadata(src).map_err(|_| "item no longer exists".to_string())?;
    let ft = meta.file_type();
    if ft.is_symlink() {
        copy_link(src, dest, &meta)?;
        created.push(dest.to_path_buf());
        return Ok(0);
    }
    if ft.is_file() {
        let bytes = copy_file(src, dest, &meta, created)?;
        return Ok(bytes);
    }
    if !ft.is_dir() {
        return Err("it contains an item that isn't a file, folder or link".into());
    }
    fs::create_dir(dest).map_err(|e| transfer_reason(&e))?;
    created.push(dest.to_path_buf());
    let mut names: Vec<_> = fs::read_dir(src)
        .map_err(|_| "a folder couldn't be read".to_string())?
        .map(|e| e.map(|e| e.file_name()).map_err(|_| "a folder couldn't be read".to_string()))
        .collect::<Result<_, _>>()?;
    names.sort();
    let mut bytes = 0;
    for n in names {
        bytes += copy_tree(&src.join(&n), &dest.join(&n), depth + 1, cancel, created)?;
    }
    let _ = fs::set_permissions(dest, meta.permissions());
    Ok(bytes)
}

fn copy_file(src: &Path, dest: &Path, meta: &fs::Metadata, created: &mut Vec<PathBuf>) -> Result<u64, String> {
    let mut open = fs::OpenOptions::new();
    open.read(true);
    let mut make = fs::OpenOptions::new();
    make.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never read through, or write through, a link swapped in meanwhile.
        open.custom_flags(libc::O_NOFOLLOW);
        make.custom_flags(libc::O_NOFOLLOW);
    }
    let mut from = open.open(src).map_err(|e| transfer_reason(&e))?;
    let mut to = make.open(dest).map_err(|e| transfer_reason(&e))?;
    created.push(dest.to_path_buf());
    let n = std::io::copy(&mut from, &mut to).map_err(|e| transfer_reason(&e))?;
    to.sync_all().map_err(|e| transfer_reason(&e))?;
    if n != meta.len() || to.metadata().map(|m| m.len()).ok() != Some(meta.len()) {
        return Err("the copy is incomplete (the file changed or the destination failed)".into());
    }
    let _ = to.set_permissions(meta.permissions());
    if let Ok(t) = meta.modified() {
        let _ = to.set_modified(t);
    }
    Ok(n)
}

/// Recreate a link with the same link text; its target is never touched.
fn copy_link(src: &Path, dest: &Path, meta: &fs::Metadata) -> Result<(), String> {
    let target = fs::read_link(src).map_err(|_| "a link couldn't be read".to_string())?;
    #[cfg(unix)]
    {
        let _ = meta;
        std::os::unix::fs::symlink(&target, dest).map_err(|e| transfer_reason(&e))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        let r = if meta.file_type().is_symlink_dir() {
            std::os::windows::fs::symlink_dir(&target, dest)
        } else {
            std::os::windows::fs::symlink_file(&target, dest)
        };
        r.map_err(|_| "links can't be created here (Windows needs Developer Mode for that)".to_string())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, dest, meta);
        Err("links can't be copied on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(name: &str) -> PathBuf {
        let d = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const NEVER: &dyn Fn() -> bool = &|| false;

    #[test]
    fn free_names_follow_finder() {
        let taken = |set: &'static [&'static str]| move |n: &str| set.contains(&n);
        assert_eq!(free_name("a.jpg", false, &taken(&[])).unwrap(), "a.jpg");
        assert_eq!(free_name("a.jpg", false, &taken(&["a.jpg"])).unwrap(), "a 2.jpg");
        assert_eq!(free_name("a.jpg", false, &taken(&["a.jpg", "a 2.jpg"])).unwrap(), "a 3.jpg");
        assert_eq!(free_name("a 2.jpg", false, &taken(&["a 2.jpg"])).unwrap(), "a 3.jpg");
        assert_eq!(free_name("Album.2024", true, &taken(&["Album.2024"])).unwrap(), "Album.2024 2");
        assert_eq!(free_name("README", false, &taken(&["README"])).unwrap(), "README 2");
        assert_eq!(free_name("2024", true, &taken(&["2024"])).unwrap(), "2024 2");
    }

    #[test]
    fn copy_never_replaces_and_copies_folders_completely() {
        let d = lab("copy");
        fs::create_dir_all(d.join("src/Album/Sub")).unwrap();
        fs::write(d.join("src/Album/a.jpg"), vec![7u8; 5000]).unwrap();
        fs::write(d.join("src/Album/Sub/b.txt"), b"b").unwrap();
        fs::write(d.join("src/Album/.hidden"), b"h").unwrap();
        fs::create_dir_all(d.join("dst")).unwrap();
        fs::write(d.join("dst/a.jpg"), b"existing").unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        // An existing destination is never replaced.
        assert!(copy_item(&p, &d.join("src/Album/a.jpg"), &d.join("dst/a.jpg"), NEVER).is_err());
        assert_eq!(fs::read(d.join("dst/a.jpg")).unwrap(), b"existing");
        // A folder copies with everything inside (hidden files too), source untouched.
        assert_eq!(copy_item(&p, &d.join("src/Album"), &d.join("dst/Album"), NEVER).unwrap(), 5002);
        assert_eq!(fs::read(d.join("dst/Album/Sub/b.txt")).unwrap(), b"b");
        assert!(d.join("dst/Album/.hidden").exists() && d.join("src/Album/a.jpg").exists());
        assert_eq!(
            fs::metadata(d.join("dst/Album/a.jpg")).unwrap().modified().unwrap(),
            fs::metadata(d.join("src/Album/a.jpg")).unwrap().modified().unwrap(),
            "dates are kept"
        );
        // Into itself: refused before anything is created.
        assert!(copy_item(&p, &d.join("src/Album"), &d.join("src/Album/Sub/Album"), NEVER).is_err());
        assert!(!d.join("src/Album/Sub/Album").exists());
        // Read-only and a protected destination refuse.
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(
            copy_item(&ro, &d.join("src/Album/a.jpg"), &d.join("dst/x.jpg"), NEVER).unwrap_err(),
            crate::policy::READ_ONLY
        );
        store.set(&d.join("dst"), 0, true).unwrap();
        assert!(copy_item(&p, &d.join("src/Album/a.jpg"), &d.join("dst/y.jpg"), NEVER)
            .unwrap_err()
            .contains("protected"));
        assert!(!d.join("dst/x.jpg").exists() && !d.join("dst/y.jpg").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_cancelled_or_failed_copy_leaves_nothing_behind() {
        let d = lab("copy-cancel");
        fs::create_dir_all(d.join("src/Album/Sub")).unwrap();
        for i in 0..6 {
            fs::write(d.join(format!("src/Album/Sub/{i}.jpg")), vec![1u8; 100]).unwrap();
        }
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        let polls = std::cell::Cell::new(0);
        let cancel = || {
            polls.set(polls.get() + 1);
            polls.get() > 4
        };
        assert_eq!(copy_item(&p, &d.join("src/Album"), &d.join("Copy"), &cancel).unwrap_err(), "cancelled");
        assert!(!d.join("Copy").exists(), "the partial copy was removed");
        assert_eq!(fs::read_dir(d.join("src/Album/Sub")).unwrap().count(), 6, "the source is untouched");
        fs::remove_dir_all(&d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn links_are_copied_and_moved_as_links() {
        let d = lab("copy-links");
        fs::create_dir_all(d.join("Elsewhere")).unwrap();
        fs::write(d.join("Elsewhere/precious.txt"), b"keep").unwrap();
        fs::create_dir_all(d.join("Album")).unwrap();
        std::os::unix::fs::symlink(d.join("Elsewhere"), d.join("Album/to-elsewhere")).unwrap();
        std::os::unix::fs::symlink(d.join("Elsewhere/precious.txt"), d.join("link.txt")).unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        assert_eq!(copy_item(&p, &d.join("Album"), &d.join("Album copy"), NEVER).unwrap(), 0);
        let m = fs::symlink_metadata(d.join("Album copy/to-elsewhere")).unwrap();
        assert!(m.file_type().is_symlink(), "a link inside a folder stays a link");
        assert_eq!(fs::read_link(d.join("Album copy/to-elsewhere")).unwrap(), d.join("Elsewhere"));
        assert!(!d.join("Album copy/to-elsewhere/precious.txt").symlink_metadata().unwrap().is_dir());
        assert_eq!(move_item(&p, &d.join("link.txt"), &d.join("Album/link.txt"), NEVER).unwrap(), Moved::Renamed);
        assert!(fs::symlink_metadata(d.join("Album/link.txt")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(d.join("Elsewhere/precious.txt")).unwrap(), b"keep", "the target never moved");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn moves_never_overwrite_and_obey_the_policy() {
        let d = lab("move");
        fs::create_dir_all(d.join("A/Inner")).unwrap();
        fs::create_dir_all(d.join("B")).unwrap();
        fs::write(d.join("A/x.jpg"), b"x").unwrap();
        fs::write(d.join("B/x.jpg"), b"other").unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        assert!(move_item(&p, &d.join("A/x.jpg"), &d.join("B/x.jpg"), NEVER).is_err());
        assert_eq!(fs::read(d.join("B/x.jpg")).unwrap(), b"other");
        assert!(d.join("A/x.jpg").exists());
        assert!(move_item(&p, &d.join("A"), &d.join("A/Inner/A"), NEVER).unwrap_err().contains("into itself"));
        let ro = Policy { read_only: true, protected: &store };
        assert_eq!(
            move_item(&ro, &d.join("A/x.jpg"), &d.join("B/y.jpg"), NEVER).unwrap_err(),
            crate::policy::READ_ONLY
        );
        // Protected source, protected destination, a folder containing a protected one.
        store.set(&d.join("A/Inner"), 0, true).unwrap();
        assert!(move_item(&p, &d.join("A"), &d.join("B/A"), NEVER).unwrap_err().contains("contains a protected"));
        store.set(&d.join("A/Inner"), 0, false).unwrap();
        store.set(&d.join("B"), 0, true).unwrap();
        assert!(move_item(&p, &d.join("A/x.jpg"), &d.join("B/y.jpg"), NEVER).unwrap_err().contains("protected"));
        store.set(&d.join("B"), 0, false).unwrap();
        assert_eq!(move_item(&p, &d.join("A/x.jpg"), &d.join("B/y.jpg"), NEVER).unwrap(), Moved::Renamed);
        assert_eq!(fs::read(d.join("B/y.jpg")).unwrap(), b"x");
        assert!(move_item(&p, &d.join("A/missing.jpg"), &d.join("B/z.jpg"), NEVER).is_err());
        fs::remove_dir_all(&d).unwrap();
    }

    /// A real cross-volume move (copy, verify, original to the Trash). Needs
    /// a second volume: `MORI_XVOL_TEST_DIR=/Volumes/MoriTest cargo test -- --ignored cross_volume`
    #[test]
    #[ignore]
    fn cross_volume_move_copies_then_trashes_the_original() {
        let other = PathBuf::from(std::env::var_os("MORI_XVOL_TEST_DIR").expect("MORI_XVOL_TEST_DIR"));
        let d = lab("xvol");
        let dest = fs::canonicalize(&other).unwrap().join(format!("mori-xvol-{}", std::process::id()));
        fs::create_dir_all(&dest).unwrap();
        fs::create_dir_all(d.join("Album/Sub")).unwrap();
        fs::write(d.join("Album/Sub/a.jpg"), vec![3u8; 4096]).unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        let r = move_item(&p, &d.join("Album"), &dest.join("Album"), NEVER).unwrap();
        assert!(matches!(r, Moved::Copied { bytes: 4096, .. }), "{r:?}");
        assert_eq!(fs::read(dest.join("Album/Sub/a.jpg")).unwrap(), vec![3u8; 4096]);
        assert!(!d.join("Album").exists(), "the original went to the Trash");
        fs::remove_dir_all(&dest).unwrap();
        fs::remove_dir_all(&d).unwrap();
    }

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
    fn permanent_delete_never_follows_links_and_obeys_the_policy() {
        let d = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-delete-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("Album/Sub")).unwrap();
        fs::create_dir_all(d.join("Elsewhere")).unwrap();
        fs::write(d.join("Elsewhere/precious.txt"), b"keep").unwrap();
        fs::write(d.join("Album/a.jpg"), vec![1u8; 1000]).unwrap();
        std::os::unix::fs::symlink(d.join("Elsewhere"), d.join("Album/Sub/to-elsewhere")).unwrap();
        std::os::unix::fs::symlink(d.join("Elsewhere/precious.txt"), d.join("link.txt")).unwrap();
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        // A link: only the link goes.
        assert_eq!(delete_permanently(&p, &d.join("link.txt")).unwrap(), 0);
        assert!(fs::symlink_metadata(d.join("link.txt")).is_err());
        assert_eq!(fs::read(d.join("Elsewhere/precious.txt")).unwrap(), b"keep");
        // Read-only and protected folders refuse.
        let ro = Policy { read_only: true, protected: &store };
        assert!(delete_permanently(&ro, &d.join("Album")).is_err());
        store.set(&d.join("Album/Sub"), 0, true).unwrap();
        assert!(delete_permanently(&p, &d.join("Album")).is_err(), "a protected folder inside");
        store.set(&d.join("Album/Sub"), 0, false).unwrap();
        // A folder with a link to elsewhere inside: the link's target survives.
        assert_eq!(delete_permanently(&p, &d.join("Album")).unwrap(), 1000);
        assert!(!d.join("Album").exists());
        assert_eq!(fs::read(d.join("Elsewhere/precious.txt")).unwrap(), b"keep");
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
