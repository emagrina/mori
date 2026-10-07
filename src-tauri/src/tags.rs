//! Local tags: names the user attaches to files and folders, kept in Mori's
//! own app data (never written into the files or their metadata).
//!
//! Like private folders, assignments are stored per volume (UUID on macOS)
//! with volume-relative paths, so they survive remounts under another name.

use crate::privacy::{volume_of, VolumeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const VERSION: u32 = 1;
const MAX_TAGS: usize = 500;
const MAX_NAME: usize = 60;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Tag {
    pub id: u32,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Assigned {
    path: String,
    tags: Vec<u32>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Volume {
    uuid: Option<String>,
    mount: String,
    items: Vec<Assigned>,
}

#[derive(Serialize, Deserialize)]
struct Data {
    version: u32,
    next_id: u32,
    tags: Vec<Tag>,
    volumes: Vec<Volume>,
}

impl Default for Data {
    fn default() -> Self {
        Data { version: VERSION, next_id: 1, tags: Vec::new(), volumes: Vec::new() }
    }
}

pub struct Store {
    file: PathBuf,
    data: Mutex<Data>,
}

fn same(v: &Volume, id: &VolumeId) -> bool {
    match (&v.uuid, &id.uuid) {
        (Some(a), Some(b)) => a == b,
        (None, None) => Path::new(&v.mount) == id.mount,
        _ => false,
    }
}

fn clean_name(name: &str) -> Result<String, String> {
    let n: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect();
    let n = n.trim().to_owned();
    if n.is_empty() {
        return Err("A tag needs a name.".into());
    }
    if n.chars().count() > MAX_NAME {
        return Err("That tag name is too long.".into());
    }
    Ok(n)
}

impl Store {
    pub fn load(file: PathBuf) -> Store {
        let data = match fs::read(&file) {
            Ok(b) => match serde_json::from_slice::<Data>(&b) {
                Ok(d) if d.version == VERSION => d,
                _ => {
                    let _ = fs::copy(&file, file.with_extension("json.bak"));
                    Data::default()
                }
            },
            Err(_) => Data::default(),
        };
        Store { file, data: Mutex::new(data) }
    }

    fn save(&self, d: &Data) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let tmp = self.file.with_extension("tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(d).map_err(|_| "Could not save tags.")?)
            .map_err(|_| "Could not save tags.")?;
        fs::rename(tmp, &self.file).map_err(|_| "Could not save tags.".to_string())
    }

    pub fn list(&self) -> Vec<Tag> {
        let mut t = self.data.lock().unwrap_or_else(PoisonError::into_inner).tags.clone();
        t.sort_by_key(|t| t.name.to_lowercase());
        t
    }

    /// The tag with this name (case-insensitive), created if needed.
    pub fn ensure(&self, name: &str) -> Result<u32, String> {
        let name = clean_name(name)?;
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(t) = d.tags.iter().find(|t| t.name.to_lowercase() == name.to_lowercase()) {
            return Ok(t.id);
        }
        if d.tags.len() >= MAX_TAGS {
            return Err("Too many tags.".into());
        }
        let id = d.next_id;
        d.next_id += 1;
        d.tags.push(Tag { id, name });
        self.save(&d)?;
        Ok(id)
    }

    pub fn rename(&self, id: u32, name: &str) -> Result<(), String> {
        let name = clean_name(name)?;
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if d.tags.iter().any(|t| t.id != id && t.name.to_lowercase() == name.to_lowercase()) {
            return Err("A tag with that name already exists.".into());
        }
        let t = d.tags.iter_mut().find(|t| t.id == id).ok_or("Unknown tag")?;
        t.name = name;
        self.save(&d)
    }

    /// Delete a tag everywhere. Files are not touched.
    pub fn delete(&self, id: u32) -> Result<(), String> {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        d.tags.retain(|t| t.id != id);
        for v in &mut d.volumes {
            for a in &mut v.items {
                a.tags.retain(|t| *t != id);
            }
            v.items.retain(|a| !a.tags.is_empty());
        }
        d.volumes.retain(|v| !v.items.is_empty());
        self.save(&d)
    }

    /// Add or remove tag `id` on canonical paths.
    pub fn assign(&self, paths: &[PathBuf], id: u32, on: bool) -> Result<(), String> {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        if !d.tags.iter().any(|t| t.id == id) {
            return Err("Unknown tag".into());
        }
        for p in paths {
            let vol = volume_of(p);
            let Some(rel) = vol.rel(p).filter(|r| !r.is_empty()) else { continue };
            let vi = match d.volumes.iter().position(|v| same(v, &vol)) {
                Some(i) => i,
                None => {
                    d.volumes.push(Volume {
                        uuid: vol.uuid.clone(),
                        mount: vol.mount.to_string_lossy().into_owned(),
                        items: Vec::new(),
                    });
                    d.volumes.len() - 1
                }
            };
            let v = &mut d.volumes[vi];
            match v.items.iter_mut().find(|a| a.path == rel) {
                Some(a) => {
                    a.tags.retain(|t| *t != id);
                    if on {
                        a.tags.push(id);
                    }
                }
                None if on => v.items.push(Assigned { path: rel, tags: vec![id] }),
                None => {}
            }
            v.items.retain(|a| !a.tags.is_empty());
        }
        d.volumes.retain(|v| !v.items.is_empty());
        self.save(&d)
    }

    /// Tags of everything below `root`, by root-relative path.
    pub fn for_root(&self, root: &Path) -> HashMap<String, Vec<u32>> {
        let vol = volume_of(root);
        let Some(base) = vol.rel(root) else { return HashMap::new() };
        let d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(v) = d.volumes.iter().find(|v| same(v, &vol)) else { return HashMap::new() };
        v.items
            .iter()
            .filter_map(|a| {
                let rel =
                    if base.is_empty() { a.path.clone() } else { a.path.strip_prefix(&format!("{base}/"))?.to_owned() };
                Some((rel, a.tags.clone()))
            })
            .collect()
    }

    /// Follow a rename made through Mori (the item and everything below it).
    pub fn renamed(&self, old: &Path, new: &Path) {
        let vol = volume_of(old);
        let (Some(o), Some(n)) = (vol.rel(old), vol.rel(new)) else { return };
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(v) = d.volumes.iter_mut().find(|v| same(v, &vol)) else { return };
        let mut changed = false;
        for a in &mut v.items {
            if a.path == o {
                a.path = n.clone();
                changed = true;
            } else if a.path.starts_with(&format!("{o}/")) {
                a.path = format!("{n}{}", &a.path[o.len()..]);
                changed = true;
            }
        }
        if changed {
            let _ = self.save(&d);
        }
    }

    /// Forget every tag and assignment (Clear Mori Data). The file is removed too.
    pub fn clear(&self) {
        *self.data.lock().unwrap_or_else(PoisonError::into_inner) = Data::default();
        let _ = fs::remove_file(&self.file);
    }

    /// "Forget this drive": drop every assignment on that volume (tag names stay).
    pub fn forget_volume(&self, vol: &VolumeId) {
        let mut d = self.data.lock().unwrap_or_else(PoisonError::into_inner);
        let before = d.volumes.len();
        d.volumes.retain(|v| !same(v, vol));
        if d.volumes.len() != before {
            let _ = self.save(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_assign_rename_delete_and_follow_renames() {
        let dir = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-tags-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Photos")).unwrap();
        fs::write(dir.join("Photos/a.jpg"), b"a").unwrap();
        fs::write(dir.join("b.jpg"), b"b").unwrap();
        let file = dir.join("tags.json");
        let s = Store::load(file.clone());
        let family = s.ensure("Family").unwrap();
        assert_eq!(s.ensure(" family ").unwrap(), family, "names are case-insensitive");
        assert!(s.ensure("  ").is_err() && s.ensure(&"x".repeat(100)).is_err());
        let trip = s.ensure("Trip\u{202E}").unwrap();
        assert_eq!(s.list().iter().find(|t| t.id == trip).unwrap().name, "Trip", "control characters dropped");
        s.assign(&[dir.join("Photos/a.jpg"), dir.join("b.jpg")], family, true).unwrap();
        s.assign(&[dir.join("Photos/a.jpg")], trip, true).unwrap();
        let s = Store::load(file.clone());
        let m = s.for_root(&dir);
        assert_eq!(m["Photos/a.jpg"], vec![family, trip]);
        assert_eq!(m["b.jpg"], vec![family]);
        assert_eq!(s.for_root(&dir.join("Photos"))["a.jpg"], vec![family, trip], "relative to any root");
        s.renamed(&dir.join("Photos"), &dir.join("Pictures"));
        assert!(s.for_root(&dir).contains_key("Pictures/a.jpg"));
        assert!(s.rename(trip, "family").is_err(), "no duplicate names");
        s.rename(trip, "Holidays").unwrap();
        s.assign(&[dir.join("b.jpg")], family, false).unwrap();
        assert!(!s.for_root(&dir).contains_key("b.jpg"));
        s.delete(family).unwrap();
        assert_eq!(s.for_root(&dir)["Pictures/a.jpg"], vec![trip]);
        assert_eq!(fs::read(dir.join("b.jpg")).unwrap(), b"b", "files untouched");
        s.forget_volume(&volume_of(&dir));
        assert!(s.for_root(&dir).is_empty());
        assert_eq!(s.list().len(), 1, "tag names survive forgetting a drive");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Renaming keeps the tag (same id, so every association); deleting
    /// removes the tag and its associations only. Both survive a restart.
    #[test]
    fn rename_and_delete_keep_files_and_identity() {
        let dir = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-tags-rd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = (0..84).map(|i| dir.join(format!("{i}.jpg"))).collect();
        for f in &files {
            fs::write(f, b"photo").unwrap();
        }
        let file = dir.join("tags.json");
        let s = Store::load(file.clone());
        let important = s.ensure("Important").unwrap();
        let other = s.ensure("Other").unwrap();
        let empty = s.ensure("Empty").unwrap();
        s.assign(&files, important, true).unwrap();
        s.assign(&files[..3], other, true).unwrap();
        let tagged = |s: &Store, id: u32| s.for_root(&dir).values().filter(|t| t.contains(&id)).count();

        // Populated rename: same id, all 84 associations, after a restart too.
        s.rename(important, "  Archive  ").unwrap();
        let s = Store::load(file.clone());
        let t = s.list().into_iter().find(|t| t.id == important).unwrap();
        assert_eq!(t.name, "Archive", "trimmed, same id");
        assert_eq!(tagged(&s, important), 84);
        assert!(s.list().iter().all(|t| t.name != "Important"), "no stale old name");
        // Empty, whitespace-only, too long, duplicate (any case): refused, nothing changes.
        for bad in ["", "   ", &"x".repeat(MAX_NAME + 1), "other", "OTHER", " Other "] {
            assert!(s.rename(important, bad).is_err(), "{bad:?}");
        }
        assert_eq!(s.list().len(), 3, "no tag was created or merged");
        assert_eq!(tagged(&s, important), 84);
        assert_eq!(tagged(&s, other), 3);
        // Case-only rename of the same tag, and Unicode.
        s.rename(important, "ARCHIVE").unwrap();
        s.rename(other, "Viaje ✈️ 日本").unwrap();
        s.rename(empty, "Vacío").unwrap();
        let s = Store::load(file.clone());
        let names: Vec<String> = s.list().into_iter().map(|t| t.name).collect();
        assert!(
            names.contains(&"ARCHIVE".into())
                && names.contains(&"Viaje ✈️ 日本".into())
                && names.contains(&"Vacío".into())
        );
        assert_eq!(tagged(&s, other), 3);
        assert!(s.rename(9999, "Ghost").is_err(), "unknown tag");

        // Delete an empty tag, then a populated one: associations go, files stay.
        s.delete(empty).unwrap();
        s.delete(important).unwrap();
        let s = Store::load(file.clone());
        assert!(s.list().iter().all(|t| t.id != empty && t.id != important));
        assert_eq!(tagged(&s, important), 0);
        assert_eq!(tagged(&s, other), 3, "other tags keep their items");
        assert!(files.iter().all(|f| fs::read(f).unwrap() == b"photo"), "no file was deleted or changed");
        fs::remove_dir_all(&dir).unwrap();
    }
}
