//! Secure Overwrite: replace a file's contents once with random bytes, flush
//! them to the device, then delete it. Offered **only where that can reach
//! the original data**, and never described as forensic erasure:
//!
//! - refused on **APFS** (copy-on-write: the new bytes go to new blocks, the
//!   old ones stay until reused; snapshots may keep them too);
//! - refused on **SSD / flash** (wear-levelling and over-provisioning keep
//!   old copies the system can't address);
//! - refused on network volumes, and whenever Mori can't positively identify
//!   a spinning hard disk;
//! - refused for files with other hard links (their data would be destroyed
//!   under other names) and for links (only the link is removed, never its
//!   target).
//!
//! One pass is enough for modern drives; more passes add nothing. Even where
//! offered, copies elsewhere (backups, Time Machine, cloud, caches) are out
//! of reach. Whole-drive erasure is out of scope.

use crate::policy::{Op, Policy};
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // detected on macOS only
pub enum Medium {
    Rotational,
    SolidState,
    Unknown,
}

/// Why overwriting files at `path` would not reach the original data, or
/// `Ok` when it can (single pass, spinning disk, non-copy-on-write volume).
pub fn eligibility(path: &Path) -> Result<(), String> {
    let (fs_type, device) = volume_info(path).ok_or("Mori can't identify this drive.")?;
    decide(&fs_type, medium(&device))
}

/// The policy, separated from the system queries so it can be tested.
pub fn decide(fs_type: &str, medium: Medium) -> Result<(), String> {
    match fs_type {
        "apfs" | "zfs" | "btrfs" => {
            return Err("This drive uses a copy-on-write file system: overwriting writes new blocks and the original data can remain.".into())
        }
        "nfs" | "smbfs" | "afpfs" | "webdav" | "cifs" => return Err("Network volumes can't be overwritten reliably from this computer.".into()),
        "hfs" | "msdos" | "exfat" | "ntfs" | "ext4" | "ext3" | "ext2" | "fat32" | "vfat" => {}
        _ => return Err("Mori doesn't know how this file system stores data.".into()),
    }
    match medium {
        Medium::Rotational => Ok(()),
        Medium::SolidState => {
            Err("This is a solid-state or flash drive: wear-levelling keeps old copies the system can't reach, so overwriting isn't meaningful.".into())
        }
        Medium::Unknown => Err("Mori can't confirm this is a spinning hard disk, so overwriting may not reach the original data.".into()),
    }
}

/// Overwrite one regular file in place with random bytes (single pass),
/// flush it to the device, then delete it. Never follows a link.
pub fn overwrite_and_delete(policy: &Policy, path: &Path) -> Result<u64, String> {
    policy.check(Op::Overwrite, path)?;
    policy.check(Op::Delete, path)?;
    eligibility(path)?;
    overwrite_unchecked(path)
}

/// The overwrite itself (eligibility already decided).
fn overwrite_unchecked(path: &Path) -> Result<u64, String> {
    let before = fs::symlink_metadata(path).map_err(|_| "file no longer exists".to_string())?;
    if before.file_type().is_symlink() {
        return Err("A link is never overwritten (its target would be): delete the link instead.".into());
    }
    if !before.is_file() {
        return Err("Only files can be overwritten.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.nlink() > 1 {
            return Err("This file has other hard links; overwriting would destroy their data too.".into());
        }
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(path).map_err(|_| "the file couldn't be opened for writing".to_string())?;
    // Same file as checked (no swap between the check and the open).
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let now = f.metadata().map_err(|_| "the file changed".to_string())?;
        if now.ino() != before.ino() || now.dev() != before.dev() || !now.is_file() {
            return Err("the file changed while it was being prepared".into());
        }
    }
    let len = before.len();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut done = 0u64;
    let mut seed = (crate::index::now_millis() as u64) ^ (len.rotate_left(17)) ^ 0x9E37_79B9_7F4A_7C15;
    while done < len {
        let n = (len - done).min(buf.len() as u64) as usize;
        // Fast PRNG (xorshift): the goal is replacing the bytes, not secrecy of the filler.
        for chunk in buf[..n].chunks_mut(8) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let b = seed.to_le_bytes();
            chunk.copy_from_slice(&b[..chunk.len()]);
        }
        f.write_all(&buf[..n]).map_err(|_| "writing failed (disk full or removed?)".to_string())?;
        done += n as u64;
    }
    f.sync_all().map_err(|_| "flushing to the device failed".to_string())?;
    #[cfg(target_os = "macos")]
    unsafe {
        use std::os::fd::AsRawFd;
        // fsync alone may stay in the drive's cache; F_FULLFSYNC asks the device to commit.
        libc::fcntl(f.as_raw_fd(), libc::F_FULLFSYNC);
    }
    drop(f);
    fs::remove_file(path).map_err(|_| "the overwritten file couldn't be removed".to_string())?;
    Ok(len)
}

#[cfg(unix)]
fn volume_info(path: &Path) -> Option<(String, String)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let existing = path.ancestors().find(|p| fs::symlink_metadata(p).is_ok())?;
    let c = CString::new(existing.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CStr;
        let t = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) }.to_string_lossy().into_owned();
        let dev = unsafe { CStr::from_ptr(st.f_mntfromname.as_ptr()) }.to_string_lossy().into_owned();
        Some((t, dev))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = st;
        Some(("unknown".into(), String::new()))
    }
}

#[cfg(not(unix))]
fn volume_info(_: &Path) -> Option<(String, String)> {
    None
}

/// The physical medium behind a BSD device ("/dev/disk4s2"), from IOKit's
/// "Device Characteristics" → "Medium Type".
#[cfg(target_os = "macos")]
pub fn medium(device: &str) -> Medium {
    use std::ffi::{c_char, c_void, CString};
    type CFRef = *const c_void;
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOBSDNameMatching(main_port: u32, options: u32, name: *const c_char) -> CFRef;
        fn IOServiceGetMatchingService(main_port: u32, matching: CFRef) -> u32;
        fn IORegistryEntrySearchCFProperty(
            entry: u32,
            plane: *const c_char,
            key: CFRef,
            alloc: CFRef,
            options: u32,
        ) -> CFRef;
        fn IOObjectRelease(o: u32) -> i32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, enc: u32) -> CFRef;
        fn CFDictionaryGetValue(d: CFRef, key: CFRef) -> CFRef;
        fn CFGetTypeID(o: CFRef) -> usize;
        fn CFDictionaryGetTypeID() -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetCString(s: CFRef, buf: *mut c_char, len: isize, enc: u32) -> bool;
        fn CFRelease(o: CFRef);
    }
    const UTF8: u32 = 0x0800_0100;
    let Some(name) = device.strip_prefix("/dev/") else { return Medium::Unknown };
    let (Ok(cname), Ok(key), Ok(sub)) =
        (CString::new(name), CString::new("Device Characteristics"), CString::new("Medium Type"))
    else {
        return Medium::Unknown;
    };
    unsafe {
        let matching = IOBSDNameMatching(0, 0, cname.as_ptr());
        if matching.is_null() {
            return Medium::Unknown;
        }
        // Consumes `matching`.
        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            return Medium::Unknown;
        }
        let k = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), UTF8);
        // kIORegistryIterateRecursively | kIORegistryIterateParents
        let chars = IORegistryEntrySearchCFProperty(service, c"IOService".as_ptr(), k, std::ptr::null(), 1 | 2);
        CFRelease(k);
        IOObjectRelease(service);
        if chars.is_null() {
            return Medium::Unknown;
        }
        let mut out = Medium::Unknown;
        if CFGetTypeID(chars) == CFDictionaryGetTypeID() {
            let sk = CFStringCreateWithCString(std::ptr::null(), sub.as_ptr(), UTF8);
            let v = CFDictionaryGetValue(chars, sk);
            CFRelease(sk);
            if !v.is_null() && CFGetTypeID(v) == CFStringGetTypeID() {
                let mut buf = [0 as c_char; 64];
                if CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) {
                    out = match std::ffi::CStr::from_ptr(buf.as_ptr()).to_bytes() {
                        b"Rotational" => Medium::Rotational,
                        b"Solid State" => Medium::SolidState,
                        _ => Medium::Unknown,
                    };
                }
            }
        }
        CFRelease(chars);
        out
    }
}

#[cfg(not(target_os = "macos"))]
pub fn medium(_: &str) -> Medium {
    Medium::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_spinning_disks_without_copy_on_write_qualify() {
        assert!(decide("hfs", Medium::Rotational).is_ok());
        assert!(decide("msdos", Medium::Rotational).is_ok());
        assert!(decide("apfs", Medium::Rotational).unwrap_err().contains("copy-on-write"));
        assert!(decide("hfs", Medium::SolidState).unwrap_err().contains("solid-state"));
        assert!(decide("exfat", Medium::Unknown).is_err());
        assert!(decide("smbfs", Medium::Rotational).is_err());
        assert!(decide("weirdfs", Medium::Rotational).is_err());
    }

    /// This Mac's own disk is APFS on solid state: never eligible.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_disk_is_refused() {
        let e = eligibility(&std::env::temp_dir()).unwrap_err();
        assert!(e.contains("copy-on-write") || e.contains("solid-state"), "{e}");
        let (_, dev) = volume_info(Path::new("/")).unwrap();
        assert_ne!(medium(&dev), Medium::Rotational, "an internal Mac SSD is never reported as spinning");
    }

    #[cfg(unix)]
    #[test]
    fn overwrite_replaces_bytes_and_never_touches_link_targets_or_hard_links() {
        let d = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-overwrite-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        // Content replaced (checked through a second handle opened before), then removed.
        fs::write(d.join("secret.txt"), vec![b'S'; 3_000_000]).unwrap();
        let mut keep = fs::File::open(d.join("secret.txt")).unwrap();
        assert_eq!(overwrite_unchecked(&d.join("secret.txt")).unwrap(), 3_000_000);
        assert!(!d.join("secret.txt").exists());
        let mut after = Vec::new();
        std::io::Read::read_to_end(&mut keep, &mut after).unwrap();
        assert_eq!(after.len(), 3_000_000);
        assert!(after.iter().filter(|b| **b == b'S').count() < 30_000, "the old bytes were replaced in place");
        // Links: refused, target untouched.
        fs::write(d.join("target.txt"), b"precious").unwrap();
        std::os::unix::fs::symlink(d.join("target.txt"), d.join("link.txt")).unwrap();
        assert!(overwrite_unchecked(&d.join("link.txt")).is_err());
        assert_eq!(fs::read(d.join("target.txt")).unwrap(), b"precious");
        // Hard links: refused.
        fs::hard_link(d.join("target.txt"), d.join("twin.txt")).unwrap();
        assert!(overwrite_unchecked(&d.join("twin.txt")).unwrap_err().contains("hard links"));
        assert_eq!(fs::read(d.join("target.txt")).unwrap(), b"precious");
        // Policy and eligibility run first: here (APFS) nothing is touched.
        let store = crate::privacy::Store::load(d.join("protected.json"));
        let p = Policy { read_only: false, protected: &store };
        fs::write(d.join("x.txt"), b"x").unwrap();
        assert!(overwrite_and_delete(&p, &d.join("x.txt")).is_err());
        assert_eq!(fs::read(d.join("x.txt")).unwrap(), b"x");
        fs::remove_dir_all(&d).unwrap();
    }
}
