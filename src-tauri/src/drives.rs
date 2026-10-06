//! Drives Mori has seen, and Safe Inspection Mode.
//!
//! A drive connected while Mori is running that Mori has never been told
//! about can be opened in Safe Inspection Mode: only filesystem metadata is
//! indexed (names, sizes, dates — the scanner never reads file contents) and
//! no media is decoded automatically. Thumbnails, previews and video frames
//! are refused by the backend until the user chooses "Generate previews"
//! for that drive. Explicit isolated views stay available.

use crate::privacy::{self, VolumeId};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Drive {
    /// Volume UUID, or "path:<mount>" when the OS gives none.
    pub key: String,
    pub label: String,
    /// The user allowed automatic previews (thumbnails) for this drive.
    pub previews: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Data {
    drives: Vec<Drive>,
}

pub struct Store {
    file: PathBuf,
    data: Mutex<Data>,
}

pub fn key_of(v: &VolumeId) -> String {
    v.uuid.clone().unwrap_or_else(|| format!("path:{}", v.mount.to_string_lossy()))
}

impl Store {
    pub fn load(file: PathBuf) -> Store {
        let data = fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Store { file, data: Mutex::new(data) }
    }

    fn save(&self, d: &Data) {
        if let Some(dir) = self.file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        if fs::write(&tmp, serde_json::to_vec_pretty(d).unwrap_or_default()).is_ok() {
            let _ = fs::rename(tmp, &self.file);
        }
    }

    pub fn get(&self, key: &str) -> Option<Drive> {
        self.data.lock().unwrap_or_else(PoisonError::into_inner).drives.iter().find(|d| d.key == key).cloned()
    }

    pub fn set(&self, key: &str, label: &str, previews: bool) {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.drives.retain(|x| x.key != key);
        d.drives.push(Drive { key: key.to_owned(), label: label.to_owned(), previews });
        self.save(&d);
    }

    #[cfg_attr(not(test), allow(dead_code))] // used by Forget this drive (organization)
    pub fn forget(&self, key: &str) {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.drives.retain(|x| x.key != key);
        self.save(&d);
    }

    /// Safe Inspection Mode applies to roots on a known drive without previews.
    pub fn safe_for(&self, root: &Path) -> bool {
        self.get(&key_of(&privacy::volume_of(root))).is_some_and(|d| !d.previews)
    }
}

/// Mounted, user-visible volumes (macOS: /Volumes/*, minus hidden system
/// volumes and the boot volume alias).
#[cfg(target_os = "macos")]
pub fn mounted() -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    const MNT_DONTBROWSE: u32 = 0x0010_0000;
    let Ok(rd) = fs::read_dir("/Volumes") else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
                return false;
            }
            let mnt = unsafe { std::ffi::CStr::from_ptr(st.f_mntonname.as_ptr()) };
            // A real mount point (not a folder on the boot volume), browsable.
            Path::new(std::ffi::OsStr::from_bytes(mnt.to_bytes())) == p.as_path() && st.f_flags & MNT_DONTBROWSE == 0
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn mounted() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_drives_persist_and_drive_safe_mode() {
        let dir = std::env::temp_dir().join(format!("mori-drives-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let canon = fs::canonicalize(&dir).unwrap();
        let s = Store::load(canon.join("drives.json"));
        let key = key_of(&privacy::volume_of(&canon));
        assert!(!s.safe_for(&canon), "unknown drives aren't in safe mode by themselves");
        s.set(&key, "Test", false);
        let s = Store::load(canon.join("drives.json"));
        assert!(s.safe_for(&canon));
        s.set(&key, "Test", true);
        assert!(!s.safe_for(&canon));
        s.forget(&key);
        assert!(s.get(&key).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn lists_only_real_mount_points() {
        for m in mounted() {
            assert!(m.starts_with("/Volumes"));
        }
    }
}
