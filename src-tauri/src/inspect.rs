//! The file report: one normalized, factual view of a file, folder or link
//! (real type, extension check, risk indicators, permissions, Mori marks).
//! Built from metadata and a bounded read of the first and last bytes;
//! nothing is decoded, executed or followed.

use crate::filetype::{self, FileType};
use crate::risk::{self, Finding};
use crate::secure::display_safe;
use serde::Serialize;
use std::fs::{self, File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes read from the start (JPEG size markers can follow large metadata).
const HEAD_READ: usize = 256 * 1024;
const TAIL_READ: u64 = 1024;

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    /// "rwxr-xr-x"
    pub mode: String,
    /// "755"
    pub octal: String,
    pub owner: String,
    pub group: String,
    pub special: Vec<String>,
    /// Filesystem flags (hidden, locked/immutable, append-only, compressed…).
    pub flags: Vec<String>,
    /// Extended attribute names (values are not read).
    pub xattrs: Vec<String>,
    /// Number of ACL entries (macOS), if any.
    pub acl_entries: Option<usize>,
    pub hard_links: u64,
}

#[derive(Serialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Marks {
    pub private: bool,
    pub inside_private: bool,
    pub protected: bool,
    pub inside_protected: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub name: String,
    pub path: String,
    /// "file" | "folder" | "link"
    pub kind: &'static str,
    pub size: u64,
    pub modified: Option<i64>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub ext: String,
    pub detected: Option<FileType>,
    /// None when Mori has no expectation for this extension.
    pub ext_matches: Option<bool>,
    pub findings: Vec<Finding>,
    pub summary: String,
    pub permissions: Option<Permissions>,
    pub link_target: Option<String>,
    pub link_outside: bool,
    pub marks: Marks,
}

fn millis(t: std::io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64)
}

/// Read the head and tail of an already-opened (non-following) file.
pub fn head_and_tail(file: &mut File, size: u64) -> (Vec<u8>, Vec<u8>) {
    let mut head = Vec::with_capacity(HEAD_READ.min(size as usize));
    let _ = file.by_ref().take(HEAD_READ as u64).read_to_end(&mut head);
    let mut tail = Vec::new();
    if size > head.len() as u64 && file.seek(SeekFrom::Start(size.saturating_sub(TAIL_READ))).is_ok() {
        let _ = file.by_ref().take(TAIL_READ).read_to_end(&mut tail);
    } else {
        tail = head[head.len().saturating_sub(TAIL_READ as usize)..].to_vec();
    }
    (head, tail)
}

/// Extra facts the caller knows (decode history, Mori marks).
#[derive(Default)]
pub struct Context {
    pub decode_failed: bool,
    pub media_blocked: bool,
    pub broken_container: bool,
    pub link_target: Option<String>,
    pub link_outside: bool,
    pub marks: Marks,
}

/// Build a report. `file` is the item opened without following links
/// (None for folders and links). `display` is the root-relative path.
pub fn report(path: &Path, display: &str, meta: &Metadata, file: Option<&mut File>, ctx: Context) -> FileReport {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        "link"
    } else if ft.is_dir() {
        "folder"
    } else {
        "file"
    };
    let ext = if kind == "file" {
        match name.rfind('.') {
            Some(i) if i > 0 => name[i + 1..].to_lowercase(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let (detected, head, tail) = match file {
        Some(f) => {
            let (h, t) = head_and_tail(f, meta.len());
            (Some(filetype::detect(&h, meta.len())), h, t)
        }
        None => (None, Vec::new(), Vec::new()),
    };
    let input = risk::Input {
        name: &name,
        ext: &ext,
        size: meta.len(),
        head: &head,
        tail: &tail,
        detected,
        executable_bit: exec_bit(meta),
        quarantined: has_xattr(path, "com.apple.quarantine"),
        decode_failed: ctx.decode_failed,
        media_blocked: ctx.media_blocked,
        broken_container: ctx.broken_container,
        symlink_target: ctx.link_target.as_deref(),
        symlink_outside: ctx.link_outside,
    };
    let findings = if kind == "folder" { risk::name_findings(&name) } else { risk::assess(&input) };
    let summary = risk::summary(&findings, detected, &ext);
    FileReport {
        name: display_safe(&name),
        path: display_safe(display),
        kind,
        size: if kind == "file" { meta.len() } else { 0 },
        modified: millis(meta.modified()),
        created: millis(meta.created()),
        accessed: millis(meta.accessed()),
        ext_matches: detected.and_then(|d| filetype::expected_ids(&ext).map(|e| e.contains(&d.id))),
        ext: display_safe(&ext),
        detected,
        findings,
        summary,
        permissions: permissions(path, meta),
        link_target: ctx.link_target.as_deref().map(display_safe),
        link_outside: ctx.link_outside,
        marks: ctx.marks,
    }
}

#[cfg(unix)]
fn exec_bit(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn exec_bit(_: &Metadata) -> bool {
    false
}

/// Names of extended attributes, read without following links.
#[cfg(target_os = "macos")]
pub fn xattr_names(path: &Path) -> Vec<String> {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return Vec::new() };
    let n = unsafe { libc::listxattr(c.as_ptr(), std::ptr::null_mut(), 0, libc::XATTR_NOFOLLOW) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf = vec![0u8; (n as usize).min(64 * 1024)];
    let n =
        unsafe { libc::listxattr(c.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len(), libc::XATTR_NOFOLLOW) };
    if n <= 0 {
        return Vec::new();
    }
    buf[..n as usize]
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .take(64)
        .map(|s| display_safe(&String::from_utf8_lossy(s)))
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn xattr_names(_: &Path) -> Vec<String> {
    Vec::new()
}

fn has_xattr(path: &Path, name: &str) -> bool {
    xattr_names(path).iter().any(|n| n == name)
}

#[cfg(unix)]
fn permissions(path: &Path, meta: &Metadata) -> Option<Permissions> {
    use std::os::unix::fs::MetadataExt;
    let mode = meta.mode();
    let bits = |m: u32, r: u32, w: u32, x: u32, special: bool, s: char| {
        format!(
            "{}{}{}",
            if m & r != 0 { 'r' } else { '-' },
            if m & w != 0 { 'w' } else { '-' },
            match (m & x != 0, special) {
                (true, true) => s,
                (false, true) => s.to_ascii_uppercase(),
                (true, false) => 'x',
                (false, false) => '-',
            }
        )
    };
    let text = format!(
        "{}{}{}",
        bits(mode, 0o400, 0o200, 0o100, mode & 0o4000 != 0, 's'),
        bits(mode, 0o040, 0o020, 0o010, mode & 0o2000 != 0, 's'),
        bits(mode, 0o004, 0o002, 0o001, mode & 0o1000 != 0, 't'),
    );
    let mut special = Vec::new();
    if mode & 0o4000 != 0 {
        special.push("setuid (runs as its owner)".into());
    }
    if mode & 0o2000 != 0 {
        special.push("setgid".into());
    }
    if mode & 0o1000 != 0 {
        special.push("sticky".into());
    }
    Some(Permissions {
        mode: text,
        octal: format!("{:o}", mode & 0o7777),
        owner: user_name(meta.uid()),
        group: group_name(meta.gid()),
        special,
        flags: fs_flags(meta),
        xattrs: xattr_names(path),
        acl_entries: acl_entries(path),
        hard_links: meta.nlink(),
    })
}

#[cfg(not(unix))]
fn permissions(_: &Path, meta: &Metadata) -> Option<Permissions> {
    Some(Permissions {
        mode: if meta.permissions().readonly() { "read-only".into() } else { "writable".into() },
        ..Default::default()
    })
}

#[cfg(unix)]
fn user_name(uid: u32) -> String {
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc == 0 && !out.is_null() && !pw.pw_name.is_null() {
        let n = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) };
        return format!("{} ({uid})", display_safe(&n.to_string_lossy()));
    }
    uid.to_string()
}

#[cfg(unix)]
fn group_name(gid: u32) -> String {
    let mut gr: libc::group = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut out: *mut libc::group = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut gr, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc == 0 && !out.is_null() && !gr.gr_name.is_null() {
        let n = unsafe { std::ffi::CStr::from_ptr(gr.gr_name) };
        return format!("{} ({gid})", display_safe(&n.to_string_lossy()));
    }
    gid.to_string()
}

#[cfg(target_os = "macos")]
fn fs_flags(meta: &Metadata) -> Vec<String> {
    use std::os::macos::fs::MetadataExt;
    let f = meta.st_flags();
    [
        (0x0000_0002u32, "locked (user immutable)"),
        (0x0000_0004, "append-only (user)"),
        (0x0000_8000, "hidden"),
        (0x0000_0020, "compressed"),
        (0x0002_0000, "locked by the system (immutable)"),
        (0x0004_0000, "append-only (system)"),
        (0x0000_0040, "tracked"),
        (0x0010_0000, "dataless (stored in the cloud)"),
    ]
    .iter()
    .filter(|(bit, _)| f & bit != 0)
    .map(|(_, s)| s.to_string())
    .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn fs_flags(_: &Metadata) -> Vec<String> {
    Vec::new()
}

/// Count ACL entries (macOS extended ACLs), without following links.
#[cfg(target_os = "macos")]
fn acl_entries(path: &Path) -> Option<usize> {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: libc::c_int, entry: *mut *mut c_void) -> libc::c_int;
        fn acl_free(obj: *mut c_void) -> libc::c_int;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    const ACL_FIRST_ENTRY: libc::c_int = 0;
    const ACL_NEXT_ENTRY: libc::c_int = -1;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let acl = unsafe { acl_get_link_np(c.as_ptr(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        return None;
    }
    let mut n = 0usize;
    let mut entry: *mut c_void = std::ptr::null_mut();
    let mut id = ACL_FIRST_ENTRY;
    while n < 1000 && unsafe { acl_get_entry(acl, id, &mut entry) } == 0 {
        n += 1;
        id = ACL_NEXT_ENTRY;
    }
    unsafe { acl_free(acl) };
    (n > 0).then_some(n)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn acl_entries(_: &Path) -> Option<usize> {
    None
}

/// Metadata of an item without following a final symlink.
pub fn lstat(path: &Path) -> std::io::Result<Metadata> {
    fs::symlink_metadata(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_type_mismatch_permissions_and_links_without_following() {
        let d = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-inspect-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let p = d.join("holiday.jpg");
        fs::write(&p, b"\xCF\xFA\xED\xFE\x07\0\0\x01 not really a photo").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let meta = lstat(&p).unwrap();
        let mut f = File::open(&p).unwrap();
        let r = report(&p, "holiday.jpg", &meta, Some(&mut f), Context::default());
        assert_eq!(r.detected.unwrap().id, "macho");
        assert_eq!(r.ext_matches, Some(false));
        assert_eq!(r.findings[0].code, "disguised-executable");
        assert!(!r.summary.contains("No anomaly"));
        #[cfg(unix)]
        {
            let perms = r.permissions.unwrap();
            assert_eq!(perms.mode, "rwxr-xr-x");
            assert_eq!(perms.octal, "755");
            assert!(perms.owner.contains('('));
            // A link is reported as itself.
            std::os::unix::fs::symlink("/etc/hosts", d.join("hosts-link")).unwrap();
            let lm = lstat(&d.join("hosts-link")).unwrap();
            let r = report(
                &d.join("hosts-link"),
                "hosts-link",
                &lm,
                None,
                Context { link_target: Some("/etc/hosts".into()), link_outside: true, ..Default::default() },
            );
            assert_eq!(r.kind, "link");
            assert!(r.detected.is_none(), "the target was not read");
            assert_eq!(r.findings[0].code, "symlink-outside");
        }
        fs::remove_dir_all(d).unwrap();
    }
}
