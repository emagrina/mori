//! File integrity: SHA-256 checksums, exact comparison of two files, and
//! integrity snapshots (a file or a folder) that can be verified later.
//!
//! - Hashing reads bytes in fixed 1 MiB chunks; nothing is parsed or decoded.
//! - Checksums are cached **in memory only** (per session), keyed by the
//!   file's identity and change information (device, inode, size, mtime,
//!   ctime). Any change invalidates the entry; nothing is written to disk.
//! - Snapshots are opt-in local state, stored only in Mori's app data
//!   (`integrity/<id>.json`), never next to the user's files, and refused in
//!   a temporary session. A snapshot records relative paths, sizes and
//!   SHA-256 values — it is as sensitive as a file listing.
//!
//! A SHA-256 match means byte-for-byte identical content. It has nothing to
//! do with Similar Media, which estimates visual similarity.

use crate::dupes::Cancelled;
use crate::index;
use crate::privacy::{self, VolumeId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

const CHUNK: usize = 1024 * 1024;
const CACHE_MAX: usize = 20_000;
const VERSION: u32 = 1;

pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Streaming SHA-256 of an open file. Fails if the size changes while reading.
pub fn sha256(file: &mut File, len: u64, cancel: &AtomicBool, progress: &AtomicU64) -> Result<String, String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err("the file couldn't be read".into()),
        };
        h.update(&buf[..n]);
        total += n as u64;
        progress.fetch_add(n as u64, Ordering::Relaxed);
        if total > len {
            return Err("the file changed while it was read".into());
        }
    }
    // Don't keep file contents around longer than needed.
    buf.fill(0);
    if total != len {
        return Err("the file changed while it was read".into());
    }
    Ok(hex(&h.finalize()))
}

/// Identity + change information. If any part differs, the file may have changed.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Version {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: u128,
    ctime_ns: i128,
}

pub fn version_of(meta: &fs::Metadata) -> Version {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Version {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime_ns: crate::dupes::mtime_ns(meta),
            ctime_ns: meta.ctime() as i128 * 1_000_000_000 + meta.ctime_nsec() as i128,
        }
    }
    #[cfg(not(unix))]
    {
        Version { dev: 0, ino: 0, len: meta.len(), mtime_ns: crate::dupes::mtime_ns(meta), ctime_ns: 0 }
    }
}

/// Session-only checksum cache. Never written to disk.
#[derive(Default)]
pub struct Cache(Mutex<HashMap<(PathBuf, Version), String>>);

impl Cache {
    pub fn get(&self, path: &Path, v: &Version) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(&(path.to_path_buf(), v.clone())).cloned()
    }
    pub fn put(&self, path: &Path, v: Version, sum: String) {
        let mut m = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if m.len() >= CACHE_MAX {
            m.clear();
        }
        // Older versions of the same path are dropped: only the current one can be valid.
        m.retain(|(p, _), _| p != path);
        m.insert((path.to_path_buf(), v), sum);
    }
    pub fn clear(&self) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
    pub fn len(&self) -> usize {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

// ------------------------------------------------------------- snapshots

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub rel: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub version: u32,
    pub id: String,
    pub created: i64,
    /// "file" or "folder".
    pub kind: String,
    pub label: String,
    /// Where it was taken: volume (UUID when known) and volume-relative path.
    pub volume: Option<String>,
    pub mount: String,
    pub root: String,
    pub items: Vec<Item>,
    /// Not recorded: links (never followed), private folders below the root, unreadable files.
    pub skipped: Skipped,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    pub links: u64,
    pub private_folders: u64,
    pub unreadable: u64,
}

#[derive(Serialize, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Verification {
    pub unchanged: u64,
    pub changed: Vec<String>,
    pub missing: Vec<String>,
    pub added: Vec<String>,
    pub unreadable: Vec<String>,
}

/// Files under `root` that a snapshot covers: regular files only, links never
/// followed (counted), private folders below the root skipped (counted),
/// hidden system clutter skipped like everywhere else in Mori.
pub fn collect(
    root: &Path,
    private: &HashSet<String>,
    cancel: &AtomicBool,
) -> Result<(Vec<(String, u64)>, Skipped), Cancelled> {
    let mut out = Vec::new();
    let mut skipped = Skipped::default();
    let meta = fs::symlink_metadata(root).map_err(|_| Cancelled)?;
    if meta.is_file() {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok((vec![(name, meta.len())], skipped));
    }
    let walker =
        walkdir::WalkDir::new(root).follow_links(false).same_file_system(true).max_depth(64).into_iter().filter_entry(
            |e| {
                if e.depth() == 0 {
                    return true;
                }
                let is_dir = e.file_type().is_dir();
                let name_ok = e.file_name().to_str().is_some_and(|n| !index::should_skip(n, is_dir));
                name_ok && !(is_dir && rel_of(root, e.path()).is_some_and(|r| private.contains(&r)))
            },
        );
    for (i, e) in walker.enumerate() {
        if i % 256 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        let Ok(e) = e else {
            skipped.unreadable += 1;
            continue;
        };
        if e.depth() == 0 {
            continue;
        }
        let ft = e.file_type();
        if ft.is_symlink() {
            skipped.links += 1;
        } else if ft.is_file() {
            if let (Some(rel), Ok(m)) = (rel_of(root, e.path()), e.metadata()) {
                out.push((rel, m.len()));
            }
        }
    }
    // Private folders directly below the root that were skipped.
    skipped.private_folders = private.len() as u64;
    out.sort();
    Ok((out, skipped))
}

fn rel_of(root: &Path, p: &Path) -> Option<String> {
    let r = p.strip_prefix(root).ok()?.to_str()?;
    Some(if cfg!(windows) { r.replace('\\', "/") } else { r.to_owned() })
}

/// Open a file of a snapshot for hashing: confined to the root, no links.
fn open_item(root: &Path, single_file: bool, rel: &str) -> Option<(File, fs::Metadata)> {
    if single_file {
        let m = fs::symlink_metadata(root).ok().filter(|m| m.is_file())?;
        return File::open(root).ok().map(|f| (f, m));
    }
    crate::secure::open_inside(root, rel).ok().map(|(f, m, _)| (f, m))
}

/// Hash every listed file. Returns items and the unreadable ones.
pub fn hash_all(
    root: &Path,
    single_file: bool,
    list: &[(String, u64)],
    cancel: &AtomicBool,
    bytes: &AtomicU64,
) -> Result<(Vec<Item>, Vec<String>), Cancelled> {
    let mut items = Vec::with_capacity(list.len());
    let mut unreadable = Vec::new();
    for (rel, _) in list {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        let Some((mut f, m)) = open_item(root, single_file, rel) else {
            unreadable.push(rel.clone());
            continue;
        };
        match sha256(&mut f, m.len(), cancel, bytes) {
            Ok(sum) => items.push(Item { rel: rel.clone(), size: m.len(), sha256: sum }),
            Err(e) if e == "cancelled" => return Err(Cancelled),
            Err(_) => unreadable.push(rel.clone()),
        }
    }
    Ok((items, unreadable))
}

/// Compare a fresh hashing pass with a snapshot.
pub fn compare(snapshot: &[Item], now: &[Item], unreadable: Vec<String>, present: &HashSet<String>) -> Verification {
    let before: BTreeMap<&str, &Item> = snapshot.iter().map(|i| (i.rel.as_str(), i)).collect();
    let after: BTreeMap<&str, &Item> = now.iter().map(|i| (i.rel.as_str(), i)).collect();
    let mut v = Verification { unreadable, ..Default::default() };
    for (rel, old) in &before {
        match after.get(rel) {
            Some(new) if new.sha256 == old.sha256 && new.size == old.size => v.unchanged += 1,
            Some(_) => v.changed.push(rel.to_string()),
            None if present.contains(*rel) => {} // present but unreadable now (listed there)
            None => v.missing.push(rel.to_string()),
        }
    }
    for rel in after.keys() {
        if !before.contains_key(rel) {
            v.added.push(rel.to_string());
        }
    }
    v
}

/// Snapshot files in Mori's app data.
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    fn file(&self, id: &str) -> Option<PathBuf> {
        (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| self.dir.join(format!("{id}.json")))
    }

    pub fn save(&self, s: &Snapshot) -> Result<(), String> {
        let f = self.file(&s.id).ok_or("invalid id")?;
        fs::create_dir_all(&self.dir).map_err(|_| "Could not save the snapshot.")?;
        let tmp = f.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec(s).map_err(|_| "Could not save the snapshot.")?)
            .map_err(|_| "Could not save the snapshot.")?;
        fs::rename(tmp, f).map_err(|_| "Could not save the snapshot.".to_string())
    }

    pub fn load(&self, id: &str) -> Option<Snapshot> {
        let f = self.file(id)?;
        serde_json::from_slice::<Snapshot>(&fs::read(f).ok()?).ok().filter(|s| s.version == VERSION)
    }

    pub fn list(&self) -> Vec<Snapshot> {
        let Ok(rd) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut v: Vec<Snapshot> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                self.load(name.strip_suffix(".json")?)
            })
            .collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.created));
        v
    }

    pub fn delete(&self, id: &str) -> Result<(), String> {
        let f = self.file(id).ok_or("invalid id")?;
        fs::remove_file(f).map_err(|_| "That snapshot no longer exists.".to_string())
    }
}

pub fn new_id() -> String {
    let mut b = [0u8; 8];
    if let Ok(mut f) = File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    let t = index::now_millis() as u64 ^ (std::process::id() as u64) << 32;
    format!("{:016x}", u64::from_le_bytes(b) ^ t)
}

/// Where a snapshot's root is now: the recorded mount if it is still the same
/// volume, otherwise any mounted volume with the recorded UUID.
pub fn locate(s: &Snapshot) -> Option<PathBuf> {
    let same = |m: &Path| privacy::volume_of(m).uuid == s.volume;
    let mut mounts: Vec<PathBuf> = vec![PathBuf::from(&s.mount)];
    mounts.extend(crate::drives::mounted());
    mounts.push(PathBuf::from("/"));
    mounts
        .into_iter()
        .filter(|m| m.exists() && (s.volume.is_none() || same(m)))
        .map(|m| m.join(&s.root))
        .find(|p| fs::symlink_metadata(p).is_ok())
}

pub fn volume_parts(canon: &Path) -> Option<(VolumeId, String)> {
    let v = privacy::volume_of(canon);
    let rel = v.rel(canon)?;
    Some((v, rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab(name: &str) -> PathBuf {
        let d = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("mori-integrity-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn known_sha256_values() {
        let d = lab("known");
        fs::write(d.join("abc.txt"), b"abc").unwrap();
        fs::write(d.join("empty"), b"").unwrap();
        let none = AtomicBool::new(false);
        let p = AtomicU64::new(0);
        let mut f = File::open(d.join("abc.txt")).unwrap();
        assert_eq!(
            sha256(&mut f, 3, &none, &p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let mut f = File::open(d.join("empty")).unwrap();
        assert_eq!(
            sha256(&mut f, 0, &none, &p).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // A size that doesn't match what is read means the file changed.
        let mut f = File::open(d.join("abc.txt")).unwrap();
        assert!(sha256(&mut f, 2, &none, &p).is_err());
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn cache_is_invalidated_by_any_change() {
        let d = lab("cache");
        let f = d.join("a.bin");
        fs::write(&f, b"one").unwrap();
        let c = Cache::default();
        let v1 = version_of(&fs::metadata(&f).unwrap());
        c.put(&f, v1.clone(), "h1".into());
        assert_eq!(c.get(&f, &v1).as_deref(), Some("h1"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&f, b"two").unwrap(); // same size, new content
        let v2 = version_of(&fs::metadata(&f).unwrap());
        assert_ne!(v1, v2);
        assert!(c.get(&f, &v2).is_none(), "an old checksum is never reused for a changed file");
        c.put(&f, v2.clone(), "h2".into());
        assert!(c.get(&f, &v1).is_none() && c.len() == 1);
        fs::remove_dir_all(d).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn folder_snapshot_skips_links_and_private_folders_and_verifies() {
        let d = lab("snap");
        fs::create_dir_all(d.join("Album/Private")).unwrap();
        fs::create_dir_all(d.join("Album/Sub")).unwrap();
        fs::create_dir_all(d.join("Outside")).unwrap();
        fs::write(d.join("Outside/x.txt"), b"outside").unwrap();
        fs::write(d.join("Album/a.txt"), b"a").unwrap();
        fs::write(d.join("Album/Sub/b.txt"), b"b").unwrap();
        fs::write(d.join("Album/Private/secret.txt"), b"s").unwrap();
        fs::write(d.join("Album/.DS_Store"), b"junk").unwrap();
        std::os::unix::fs::symlink(d.join("Outside"), d.join("Album/link")).unwrap();
        let root = d.join("Album");
        let private: HashSet<String> = ["Private".to_string()].into();
        let none = AtomicBool::new(false);
        let (list, skipped) = collect(&root, &private, &none).unwrap();
        let names: Vec<&str> = list.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(names, ["Sub/b.txt", "a.txt"], "no private, no link target, no clutter");
        assert_eq!(skipped.links, 1);
        let (items, bad) = hash_all(&root, false, &list, &none, &AtomicU64::new(0)).unwrap();
        assert!(bad.is_empty());
        // Change, remove, add.
        fs::write(d.join("Album/a.txt"), b"A!").unwrap();
        fs::remove_file(d.join("Album/Sub/b.txt")).unwrap();
        fs::write(d.join("Album/new.txt"), b"n").unwrap();
        let (list2, _) = collect(&root, &private, &none).unwrap();
        let (now, bad) = hash_all(&root, false, &list2, &none, &AtomicU64::new(0)).unwrap();
        let present: HashSet<String> = list2.iter().map(|(r, _)| r.clone()).collect();
        let v = compare(&items, &now, bad, &present);
        assert_eq!(v.changed, ["a.txt"]);
        assert_eq!(v.missing, ["Sub/b.txt"]);
        assert_eq!(v.added, ["new.txt"]);
        assert_eq!(v.unchanged, 0);
        // Selecting the private folder itself works (it's the root then).
        let (inside, _) = collect(&d.join("Album/Private"), &HashSet::new(), &none).unwrap();
        assert_eq!(inside.len(), 1);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn store_round_trip_and_ids() {
        let d = lab("store");
        let s = Store::new(d.join("integrity"));
        let id = new_id();
        assert_eq!(id.len(), 16);
        let snap = Snapshot {
            version: VERSION,
            id: id.clone(),
            created: 1,
            kind: "file".into(),
            label: "x".into(),
            volume: None,
            mount: "/".into(),
            root: "tmp/x".into(),
            items: vec![Item { rel: "x".into(), size: 1, sha256: "00".into() }],
            skipped: Skipped::default(),
        };
        s.save(&snap).unwrap();
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.load(&id).unwrap().items, snap.items);
        assert!(s.load("../../etc/passwd").is_none(), "ids are validated");
        s.delete(&id).unwrap();
        assert!(s.list().is_empty());
        fs::remove_dir_all(d).unwrap();
    }
}
