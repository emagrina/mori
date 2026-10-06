//! Private folders: visibility boundaries inside Mori.
//!
//! A private folder is NOT encrypted, locked, renamed or modified in any way,
//! and nothing is written to the user's drive. It is a Mori-only rule:
//! when Mori looks at a folder *from outside* (drive-wide library views,
//! global search, counts, "Include subfolders", analysis started from a
//! parent folder), it does not cross into a private folder. When the user
//! opens the private folder itself, it is an ordinary folder again — except
//! that private folders nested inside it are boundaries in turn.
//!
//! The rule is applied in two central places:
//! - the index (`Index::apply_boundaries`): every entry knows the deepest
//!   private folder above it, and `index::query` / `index::stats` hide it
//!   unless the current scope is that folder or inside it;
//! - the analysis walker (`dupes::collect`, shared by Exact Duplicates and
//!   Similar Media), which stops at a boundary before reading anything.
//!
//! Records live in Mori's app data (`private-folders.json`), per volume:
//! the volume is identified by its filesystem UUID where the OS provides one
//! (macOS), otherwise by its mount path. Folder paths are relative to the
//! volume, so they survive re-mounting under another name, and each folder
//! also remembers its inode so a folder moved or renamed outside Mori is
//! found again on the next scan instead of silently becoming public.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Folder {
    /// Path relative to the volume root, `/`-separated.
    path: String,
    /// Inode / file id at the time it was marked (0 = unknown).
    #[serde(default)]
    ino: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Volume {
    /// Filesystem UUID (stable across mounts and renames of the volume).
    uuid: Option<String>,
    /// Last mount path seen (identity only when there is no UUID).
    mount: String,
    folders: Vec<Folder>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
struct Data {
    version: u32,
    volumes: Vec<Volume>,
}

/// The volume a path lives on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeId {
    pub uuid: Option<String>,
    pub mount: PathBuf,
}

impl VolumeId {
    /// Path of `canon` relative to the volume root ("" = the volume root).
    fn rel(&self, canon: &Path) -> Option<String> {
        let r = canon.strip_prefix(&self.mount).ok()?.to_str()?;
        Some(if cfg!(windows) { r.replace('\\', "/") } else { r.to_owned() })
    }
}

/// Identify the volume holding `canon` (a canonical path).
pub fn volume_of(canon: &Path) -> VolumeId {
    platform::volume_of(canon)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::VolumeId;
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};

    pub fn volume_of(canon: &Path) -> VolumeId {
        let Ok(c) = CString::new(canon.as_os_str().as_bytes()) else {
            return VolumeId { uuid: None, mount: PathBuf::from("/") };
        };
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return VolumeId { uuid: None, mount: PathBuf::from("/") };
        }
        let mnt = unsafe { CStr::from_ptr(st.f_mntonname.as_ptr()) };
        let mut mount = PathBuf::from(std::ffi::OsStr::from_bytes(mnt.to_bytes()));
        // The boot volume's data lives on /System/Volumes/Data but is seen
        // through firmlinks as /Users, /Applications…: use "/" as its root.
        if !canon.starts_with(&mount) {
            mount = PathBuf::from("/");
        }
        let uuid = volume_uuid(&CString::new(mnt.to_bytes()).unwrap_or_default());
        VolumeId { uuid, mount }
    }

    /// Filesystem UUID via getattrlist(ATTR_VOL_UUID) on the mount point.
    fn volume_uuid(mount: &CStr) -> Option<String> {
        let mut req: libc::attrlist = unsafe { std::mem::zeroed() };
        req.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        req.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
        #[repr(C)]
        struct Reply {
            len: u32,
            uuid: [u8; 16],
        }
        let mut reply = Reply { len: 0, uuid: [0; 16] };
        let rc = unsafe {
            libc::getattrlist(
                mount.as_ptr(),
                &mut req as *mut _ as *mut libc::c_void,
                &mut reply as *mut _ as *mut libc::c_void,
                std::mem::size_of::<Reply>(),
                0,
            )
        };
        if rc != 0 || reply.len < 20 || reply.uuid == [0; 16] {
            return None;
        }
        Some(reply.uuid.iter().map(|b| format!("{b:02x}")).collect())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::VolumeId;
    use std::path::{Path, PathBuf};

    /// Without a filesystem UUID the mount point is the identity: the
    /// nearest ancestor on the same device (Unix) or the path's prefix
    /// (Windows drive letter).
    pub fn volume_of(canon: &Path) -> VolumeId {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let dev = std::fs::metadata(canon).map(|m| m.dev()).ok();
            let mut mount = canon.to_path_buf();
            while let Some(parent) = mount.parent() {
                if std::fs::metadata(parent).map(|m| m.dev()).ok() != dev {
                    break;
                }
                mount = parent.to_path_buf();
            }
            VolumeId { uuid: None, mount }
        }
        #[cfg(not(unix))]
        {
            let mount = canon.ancestors().last().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("\\"));
            VolumeId { uuid: None, mount }
        }
    }
}

/// Inode / file id of a folder (0 where the platform doesn't expose one).
pub fn ino_of(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

pub struct Store {
    file: PathBuf,
    data: Mutex<Data>,
}

fn below(path: &str, base: &str) -> bool {
    base.is_empty() && !path.is_empty()
        || (path.len() > base.len() && path.as_bytes()[base.len()] == b'/' && path.starts_with(base))
}

impl Store {
    /// Load the records (an unreadable or newer file is treated as empty
    /// but never overwritten blindly: it is kept as `.bak`).
    pub fn load(file: PathBuf) -> Store {
        let data = match fs::read(&file) {
            Ok(b) => match serde_json::from_slice::<Data>(&b) {
                Ok(d) if d.version == VERSION => d,
                _ => {
                    let _ = fs::copy(&file, file.with_extension("json.bak"));
                    Data { version: VERSION, volumes: Vec::new() }
                }
            },
            Err(_) => Data { version: VERSION, volumes: Vec::new() },
        };
        Store { file, data: Mutex::new(data) }
    }

    fn save(&self, data: &Data) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = self.file.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(data)?)?;
        fs::rename(tmp, &self.file)
    }

    fn find<'a>(data: &'a mut Data, vol: &VolumeId) -> Option<&'a mut Volume> {
        let idx = data.volumes.iter().position(|v| match (&v.uuid, &vol.uuid) {
            (Some(a), Some(b)) => a == b,
            (None, None) => Path::new(&v.mount) == vol.mount,
            _ => false,
        })?;
        let v = &mut data.volumes[idx];
        // Same volume, possibly mounted under another name now.
        if let Some(m) = vol.mount.to_str() {
            if v.mount != m {
                v.mount = m.to_owned();
            }
        }
        Some(v)
    }

    /// Private folders strictly below `root` (canonical), relative to it.
    /// `root` itself, or a root inside a private folder, is an explicit
    /// choice by the user and is never a boundary for that root.
    pub fn boundaries(&self, root: &Path) -> HashSet<String> {
        let vol = volume_of(root);
        let Some(base) = vol.rel(root) else { return HashSet::new() };
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(v) = Self::find(&mut data, &vol) else { return HashSet::new() };
        v.folders
            .iter()
            .filter(|f| below(&f.path, &base))
            .map(|f| if base.is_empty() { f.path.clone() } else { f.path[base.len() + 1..].to_owned() })
            .collect()
    }

    #[cfg(test)]
    pub fn is_private(&self, dir: &Path) -> bool {
        let vol = volume_of(dir);
        let Some(rel) = vol.rel(dir) else { return false };
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        Self::find(&mut data, &vol).is_some_and(|v| v.folders.iter().any(|f| f.path == rel))
    }

    /// Mark or unmark a folder (canonical path) as private.
    pub fn set(&self, dir: &Path, ino: u64, private: bool) -> Result<(), String> {
        let vol = volume_of(dir);
        let rel = vol.rel(dir).filter(|r| !r.is_empty()).ok_or("This folder can't be made private.")?;
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if Self::find(&mut data, &vol).is_none() {
            data.volumes.push(Volume {
                uuid: vol.uuid.clone(),
                mount: vol.mount.to_string_lossy().into_owned(),
                folders: Vec::new(),
            });
        }
        let v = Self::find(&mut data, &vol).expect("just added");
        v.folders.retain(|f| f.path != rel);
        if private {
            v.folders.push(Folder { path: rel, ino });
            v.folders.sort_by(|a, b| a.path.cmp(&b.path));
        }
        data.volumes.retain(|v| !v.folders.is_empty());
        self.save(&data).map_err(|_| "Could not save the setting.".to_string())
    }

    /// A folder was renamed or moved by Mori: carry its privacy (and that of
    /// private folders inside it) to the new path.
    pub fn renamed(&self, old: &Path, new: &Path) {
        let vol = volume_of(old);
        let (Some(o), Some(n)) = (vol.rel(old), vol.rel(new)) else { return };
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(v) = Self::find(&mut data, &vol) else { return };
        let mut changed = false;
        for f in &mut v.folders {
            if f.path == o {
                f.path = n.clone();
                changed = true;
            } else if below(&f.path, &o) {
                f.path = format!("{n}{}", &f.path[o.len()..]);
                changed = true;
            }
        }
        if changed {
            let _ = self.save(&data);
        }
    }

    /// After a scan of `root`: a recorded private folder that no longer
    /// exists at its path but whose inode is found elsewhere under the root
    /// was moved or renamed outside Mori. Follow it, so it stays private.
    /// `dirs` are (root-relative path, inode) pairs. Returns true if any
    /// record changed.
    pub fn reconcile(&self, root: &Path, dirs: &[(&str, u64)]) -> bool {
        let vol = volume_of(root);
        let Some(base) = vol.rel(root) else { return false };
        let mut data = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(v) = Self::find(&mut data, &vol) else { return false };
        let present: HashSet<&str> = dirs.iter().map(|(p, _)| *p).collect();
        let to_vol = |rel: &str| if base.is_empty() { rel.to_owned() } else { format!("{base}/{rel}") };
        let mut changed = false;
        for i in 0..v.folders.len() {
            let f = &v.folders[i];
            if f.ino == 0 || !below(&f.path, &base) {
                continue;
            }
            let rel = if base.is_empty() { f.path.as_str() } else { &f.path[base.len() + 1..] };
            if present.contains(rel) {
                continue;
            }
            if let Some((new_rel, _)) = dirs.iter().find(|(_, ino)| *ino == f.ino) {
                let (old, new) = (f.path.clone(), to_vol(new_rel));
                // Nested private folders move with it.
                for g in &mut v.folders {
                    if g.path == old {
                        g.path = new.clone();
                    } else if below(&g.path, &old) {
                        g.path = format!("{new}{}", &g.path[old.len()..]);
                    }
                }
                changed = true;
            }
        }
        if changed {
            let _ = self.save(&data);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("mori-privacy-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let canon = fs::canonicalize(&dir).unwrap();
        (Store::load(canon.join("private-folders.json")), canon)
    }

    #[test]
    fn boundaries_are_relative_to_the_root_and_exclude_explicit_roots() {
        let (s, base) = store("rel");
        let tree = base.join("Pictures");
        fs::create_dir_all(tree.join("Private/More")).unwrap();
        fs::create_dir_all(tree.join("Travel")).unwrap();
        s.set(&tree.join("Private"), 1, true).unwrap();
        s.set(&tree.join("Private/More"), 2, true).unwrap();
        let b = s.boundaries(&tree);
        assert_eq!(b, ["Private".to_string(), "Private/More".to_string()].into());
        // Opening the private folder itself: only the nested one is a boundary.
        assert_eq!(s.boundaries(&tree.join("Private")), ["More".to_string()].into());
        assert!(s.boundaries(&tree.join("Private/More")).is_empty());
        assert!(s.boundaries(&tree.join("Travel")).is_empty());
        assert!(s.is_private(&tree.join("Private")));
        s.set(&tree.join("Private"), 1, false).unwrap();
        assert_eq!(s.boundaries(&tree), ["Private/More".to_string()].into());
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn persists_and_follows_renames_and_external_moves() {
        let (s, base) = store("persist");
        fs::create_dir_all(base.join("A/Secret/Inner")).unwrap();
        fs::create_dir_all(base.join("B")).unwrap();
        s.set(&base.join("A/Secret"), 77, true).unwrap();
        s.set(&base.join("A/Secret/Inner"), 78, true).unwrap();
        // Restart: reload from disk.
        let s = Store::load(s.file.clone());
        assert!(s.is_private(&base.join("A/Secret")));
        // Renamed through Mori.
        s.renamed(&base.join("A/Secret"), &base.join("A/Hidden"));
        assert_eq!(s.boundaries(&base), ["A/Hidden".to_string(), "A/Hidden/Inner".to_string()].into());
        // Moved outside Mori: found again by inode on the next scan.
        assert!(s.reconcile(&base, &[("A", 10), ("B", 11), ("B/Moved", 77), ("B/Moved/Inner", 78)]));
        assert_eq!(s.boundaries(&base), ["B/Moved".to_string(), "B/Moved/Inner".to_string()].into());
        // Not found anywhere: the record is kept (never silently dropped).
        assert!(!s.reconcile(&base, &[("A", 10)]));
        assert!(s.boundaries(&base).contains("B/Moved"));
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn volumes_match_by_uuid_even_when_mounted_elsewhere() {
        let mut data = Data { version: VERSION, volumes: Vec::new() };
        data.volumes.push(Volume {
            uuid: Some("abc".into()),
            mount: "/Volumes/Photos".into(),
            folders: vec![Folder { path: "Personal".into(), ino: 5 }],
        });
        data.volumes.push(Volume {
            uuid: None,
            mount: "/Volumes/USB".into(),
            folders: vec![Folder { path: "x".into(), ino: 0 }],
        });
        // Same drive mounted as "Photos 1": still found, mount path updated.
        let v = Store::find(&mut data, &VolumeId { uuid: Some("abc".into()), mount: "/Volumes/Photos 1".into() });
        assert_eq!(v.unwrap().folders[0].path, "Personal");
        assert_eq!(data.volumes[0].mount, "/Volumes/Photos 1");
        // Another drive mounted at the old path is NOT matched.
        assert!(
            Store::find(&mut data, &VolumeId { uuid: Some("zzz".into()), mount: "/Volumes/Photos".into() }).is_none()
        );
        // Without UUIDs the mount path is the identity.
        assert!(Store::find(&mut data, &VolumeId { uuid: None, mount: "/Volumes/USB".into() }).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_boot_volume_has_a_uuid() {
        let v = volume_of(&fs::canonicalize(std::env::temp_dir()).unwrap());
        assert!(v.uuid.is_some(), "{v:?}");
    }

    #[test]
    fn a_corrupt_file_is_kept_aside_not_overwritten() {
        let (_, base) = store("corrupt");
        let f = base.join("private-folders.json");
        fs::write(&f, b"{not json").unwrap();
        let s = Store::load(f.clone());
        assert!(s.boundaries(&base).is_empty());
        assert_eq!(fs::read(f.with_extension("json.bak")).unwrap(), b"{not json");
        fs::remove_dir_all(base).unwrap();
    }
}

/// Real external-drive check (opt-in): phase "mark" marks a folder private,
/// then the drive is ejected and mounted again (possibly under another
/// name); phase "check" must find the record again.
/// `MORI_PRIV_STORE=… MORI_PRIV_DIR=… MORI_PRIV_PHASE=mark|check cargo test -- --ignored --nocapture external_drive`
#[cfg(test)]
mod drive {
    use super::*;

    #[test]
    #[ignore]
    fn external_drive_roundtrip() {
        let (Some(store), Some(dir), Some(phase)) = (
            std::env::var_os("MORI_PRIV_STORE"),
            std::env::var_os("MORI_PRIV_DIR"),
            std::env::var("MORI_PRIV_PHASE").ok(),
        ) else {
            return;
        };
        let s = Store::load(PathBuf::from(store));
        let dir = fs::canonicalize(PathBuf::from(dir)).unwrap();
        let vol = volume_of(&dir);
        println!("DRIVE volume {vol:?}");
        match phase.as_str() {
            "mark" => {
                let ino = ino_of(&fs::metadata(&dir).unwrap());
                s.set(&dir, ino, true).unwrap();
                println!("DRIVE marked {}", dir.display());
            }
            _ => {
                let parent = dir.parent().unwrap();
                let b = s.boundaries(parent);
                println!("DRIVE boundaries below {}: {b:?}", parent.display());
                assert!(b.contains(dir.file_name().unwrap().to_str().unwrap()));
            }
        }
    }
}
