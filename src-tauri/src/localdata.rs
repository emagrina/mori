//! Everything Mori stores on this computer, in one place: what it is, where
//! it lives, why it exists, and how to remove it. Used by the Privacy & Data
//! view, Clear Mori Data, Reset Mori and the startup cleanup.
//!
//! Deletion here is limited to paths strictly inside Mori's own directories
//! (its app-data folder, its cache folder, and WebKit's folder for Mori's
//! bundle identifier). Nothing on a browsed drive is ever touched.

use serde::Serialize;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const IDENTIFIER: &str = "app.mori.viewer";

#[derive(Clone, Debug)]
pub struct Dirs {
    pub data: PathBuf,
    pub cache: PathBuf,
    /// WebKit's per-app website data (from versions before the web view
    /// became non-persistent). macOS only.
    pub webkit: Option<PathBuf>,
}

impl Dirs {
    pub fn new(data: PathBuf, cache: PathBuf) -> Dirs {
        let webkit = if cfg!(target_os = "macos") {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/WebKit").join(IDENTIFIER))
        } else {
            None
        };
        Dirs { data, cache, webkit }
    }

    /// Strictly inside one of Mori's directories (never one of them itself,
    /// never through `..`, never through a symbolic link).
    pub fn owns(&self, p: &Path) -> bool {
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return false;
        }
        let bases = [Some(&self.data), Some(&self.cache), self.webkit.as_ref()];
        let inside = bases.iter().flatten().any(|b| {
            p.starts_with(b)
                && p != b.as_path()
                && b.components().count() > 2
                && b.ends_with(IDENTIFIER)
                && fs::symlink_metadata(b).is_ok_and(|m| m.is_dir())
        });
        // No link between the base and the target.
        inside
            && p.ancestors()
                .take_while(|a| bases.iter().flatten().all(|b| a != b))
                .all(|a| fs::symlink_metadata(a).map_or(true, |m| !m.file_type().is_symlink()))
    }

    /// Remove a Mori-owned file or folder. Returns bytes freed.
    pub fn remove(&self, p: &Path) -> u64 {
        if !self.owns(p) {
            return 0;
        }
        let bytes = size_of(p).0;
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => {
                let _ = fs::remove_dir_all(p);
            }
            Ok(_) => {
                let _ = fs::remove_file(p);
            }
            Err(_) => return 0,
        }
        bytes
    }
}

/// (bytes, files) of a path, without following links.
pub fn size_of(p: &Path) -> (u64, u64) {
    let Ok(m) = fs::symlink_metadata(p) else { return (0, 0) };
    if !m.is_dir() {
        return (m.len(), 1);
    }
    walkdir::WalkDir::new(p)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .fold((0, 0), |(b, n), e| (b + e.metadata().map(|m| m.len()).unwrap_or(0), n + 1))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    /// Thumbnails, similarity fingerprints, index caches, web view caches.
    Cache,
    /// "Not duplicates" decisions, media-engine blocklist.
    Analysis,
    /// Last folder, known drives, crash marker.
    History,
    /// Tags, favorites, screenshot corrections.
    Organization,
    /// Private and protected folder rules.
    Rules,
    /// Integrity snapshots.
    Integrity,
    /// View preferences and Read-only Mode (Reset only).
    Settings,
}

impl Category {
    pub fn parse(s: &str) -> Option<Category> {
        Some(match s {
            "cache" => Category::Cache,
            "analysis" => Category::Analysis,
            "history" => Category::History,
            "organization" => Category::Organization,
            "rules" => Category::Rules,
            "integrity" => Category::Integrity,
            "settings" => Category::Settings,
            _ => return None,
        })
    }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub path: String,
    pub bytes: u64,
    pub files: u64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub category: Category,
    pub title: &'static str,
    /// What it contains and why Mori keeps it.
    pub what: &'static str,
    /// Can it reveal anything about the user's files?
    pub sensitivity: &'static str,
    pub bytes: u64,
    pub files: u64,
    pub locations: Vec<Location>,
}

/// The files and folders of each category.
pub fn paths(d: &Dirs, c: Category) -> Vec<PathBuf> {
    let data = |n: &str| d.data.join(n);
    match c {
        Category::Cache => {
            let mut v = vec![d.cache.join("thumbs"), d.cache.join("similar"), data("index-v2"), d.cache.join("WebKit")];
            v.extend(d.webkit.clone());
            v
        }
        Category::Analysis => vec![data("similar-dismissed.bin"), data("blocked-media.json")],
        Category::History => vec![data("drives.json"), data("last-panic.txt")],
        Category::Organization => {
            vec![data("tags.json"), data("favorites.json"), data("capture-not.json"), data("capture-yes.json")]
        }
        Category::Rules => vec![data("private-folders.json"), data("protected-folders.json")],
        Category::Integrity => vec![data("integrity")],
        Category::Settings => vec![data("settings.json")],
    }
}

pub fn describe(c: Category) -> (&'static str, &'static str, &'static str) {
    match c {
        Category::Cache => (
            "Cache & thumbnails",
            "Thumbnails and video frames made by the sandboxed worker, similarity fingerprints (64×64 grayscale), and the index of each browsed folder (names, sizes, dates). They make browsing fast and are rebuilt when needed.",
            "Thumbnails and fingerprints show what images look like; indexes list file and folder names.",
        ),
        Category::Analysis => (
            "Analysis data",
            "“Not duplicates” decisions (pairs of content hashes) and the list of videos that stopped the system media engine (opaque hashes).",
            "No names or paths.",
        ),
        Category::History => (
            "History",
            "Drives you inspected safely (volume ID and name), the last folder opened, and a crash marker (time and source-code line only).",
            "Drive names and the last folder's path.",
        ),
        Category::Organization => ("Tags & favorites", "Your tags, favorites and screenshot corrections, by volume and path.", "Paths of tagged items and tag names."),
        Category::Rules => ("Folder privacy & protection rules", "Which folders are private or protected (Never Modify), by volume and path.", "Paths of those folders."),
        Category::Integrity => ("Integrity snapshots", "Snapshots you saved: relative paths, sizes and SHA-256 of the files.", "File names and checksums of the snapshotted items."),
        Category::Settings => ("Settings", "View preferences and Read-only Mode.", "Nothing about your files besides the last folder (listed under History)."),
    }
}

pub fn inventory(d: &Dirs) -> Vec<Group> {
    [
        Category::Cache,
        Category::Analysis,
        Category::History,
        Category::Organization,
        Category::Rules,
        Category::Integrity,
        Category::Settings,
    ]
    .into_iter()
    .map(|c| {
        let (title, what, sensitivity) = describe(c);
        let locations: Vec<Location> = paths(d, c)
            .into_iter()
            .filter(|p| fs::symlink_metadata(p).is_ok())
            .map(|p| {
                let (bytes, files) = size_of(&p);
                Location { path: crate::secure::plain_path(&p), bytes, files }
            })
            .collect();
        Group {
            category: c,
            title,
            what,
            sensitivity,
            bytes: locations.iter().map(|l| l.bytes).sum(),
            files: locations.iter().map(|l| l.files).sum(),
            locations,
        }
    })
    .collect()
}

/// Delete the files of these categories (only inside Mori's directories).
pub fn clear(d: &Dirs, cats: &[Category]) -> u64 {
    cats.iter().flat_map(|c| paths(d, *c)).map(|p| d.remove(&p)).sum()
}

/// At startup: remove leftovers of interrupted writes (`*.tmp`, `*.part`)
/// that only Mori creates, inside its own directories. Returns how many.
pub fn startup_cleanup(d: &Dirs) -> u64 {
    let mut n = 0;
    for base in [&d.data, &d.cache] {
        for e in walkdir::WalkDir::new(base).follow_links(false).max_depth(4).into_iter().flatten() {
            let p = e.path();
            let stale =
                e.file_type().is_file() && matches!(p.extension().and_then(|x| x.to_str()), Some("tmp" | "part"));
            if stale && d.owns(p) && fs::remove_file(p).is_ok() {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(name: &str) -> (PathBuf, Dirs) {
        let base = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("mori-localdata-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let d =
            Dirs { data: base.join("data").join(IDENTIFIER), cache: base.join("cache").join(IDENTIFIER), webkit: None };
        fs::create_dir_all(&d.data).unwrap();
        fs::create_dir_all(&d.cache).unwrap();
        (base, d)
    }

    #[test]
    fn only_mori_directories_are_ever_removed() {
        let (base, d) = dirs("owns");
        fs::write(d.data.join("tags.json"), b"{}").unwrap();
        fs::create_dir_all(base.join("user")).unwrap();
        fs::write(base.join("user/photo.jpg"), b"x").unwrap();
        assert!(d.owns(&d.data.join("tags.json")));
        assert!(!d.owns(&d.data), "never the folder itself");
        assert!(!d.owns(&base.join("user/photo.jpg")));
        assert!(!d.owns(&d.data.join("../../user/photo.jpg")));
        // A link planted inside Mori's folder pointing at user files is not followed.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("user"), d.cache.join("thumbs")).unwrap();
            assert!(!d.owns(&d.cache.join("thumbs/photo.jpg")));
            assert_eq!(d.remove(&d.cache.join("thumbs/photo.jpg")), 0);
            assert!(base.join("user/photo.jpg").exists());
        }
        assert_eq!(clear(&d, &[Category::Organization]), 2);
        assert!(!d.data.join("tags.json").exists());
        assert!(base.join("user/photo.jpg").exists(), "user files untouched");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn inventory_and_startup_cleanup() {
        let (base, d) = dirs("inv");
        fs::create_dir_all(d.cache.join("thumbs/v1/ab")).unwrap();
        fs::write(d.cache.join("thumbs/v1/ab/x.jpg"), vec![0u8; 100]).unwrap();
        fs::write(d.cache.join("thumbs/v1/ab/y.part"), b"half").unwrap();
        fs::write(d.data.join("tags.tmp"), b"half").unwrap();
        fs::write(d.data.join("tags.json"), b"{}").unwrap();
        let inv = inventory(&d);
        let cache = inv.iter().find(|g| g.category == Category::Cache).unwrap();
        assert_eq!((cache.bytes, cache.files), (104, 2));
        assert_eq!(startup_cleanup(&d), 2);
        assert!(!d.cache.join("thumbs/v1/ab/y.part").exists() && !d.data.join("tags.tmp").exists());
        assert!(d.data.join("tags.json").exists() && d.cache.join("thumbs/v1/ab/x.jpg").exists());
        fs::remove_dir_all(base).unwrap();
    }
}
