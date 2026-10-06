//! Thumbnail cache. Only images freshly encoded by the sandboxed worker are
//! stored here, under internally generated hash names (never the original
//! filename). Originals are never touched and no thumbnail is written beside
//! them.

use crate::worker::{OutFormat, Output};
use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Bump to invalidate every cached thumbnail.
const CACHE_VERSION: &str = "v2";

/// 64-bit FNV-1a: stable across Rust versions, unlike `DefaultHasher`.
pub fn fnv(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3))
}

/// Cache path (without extension) for a given file version and size.
pub fn stem(cache: &Path, canon: &Path, meta: &Metadata, max: u32) -> PathBuf {
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let key = format!("{CACHE_VERSION}|{}|{}|{}|{}", canon.to_string_lossy(), meta.len(), mtime, max);
    let h = format!("{:016x}", fnv(key.as_bytes()));
    cache.join(&h[..2]).join(h)
}

pub fn cached(stem: &Path) -> Option<(Vec<u8>, &'static str)> {
    for f in [OutFormat::Jpeg, OutFormat::Png] {
        if let Ok(bytes) = fs::read(stem.with_extension(f.ext())) {
            return Some((bytes, f.mime()));
        }
    }
    None
}

/// A previous attempt failed for this exact file version: don't retry.
pub fn is_miss(stem: &Path) -> bool {
    stem.with_extension("none").is_file()
}

pub fn mark_miss(stem: &Path) {
    if let Some(dir) = stem.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(stem.with_extension("none"), b"");
}

pub fn store(stem: &Path, out: &Output) {
    let Some(dir) = stem.parent() else { return };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let target = stem.with_extension(out.format.ext());
    let tmp = stem.with_extension("part");
    if fs::write(&tmp, &out.bytes).is_ok() {
        let _ = fs::rename(&tmp, &target);
    }
}

/// Thumbnails of files outside the browsed folder (shown only by the
/// Duplicate Analyzer). Kept in memory, never written to disk, and dropped
/// with the analysis, so no trace of e.g. ~/Pictures is left in the cache.
#[derive(Default)]
pub struct Volatile(std::sync::Mutex<VolatileMap>);

/// Cache stem → encoded thumbnail and MIME type (`None` = known miss).
type VolatileMap = std::collections::HashMap<PathBuf, Option<(Vec<u8>, &'static str)>>;

/// Enough for every thumbnail on screen plus generous scrolling history.
const VOLATILE_MAX: usize = 2000;

impl Volatile {
    fn map(&self) -> std::sync::MutexGuard<'_, VolatileMap> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `Some(None)` = known miss.
    pub fn get(&self, stem: &Path) -> Option<Option<(Vec<u8>, &'static str)>> {
        self.map().get(stem).cloned()
    }

    pub fn put(&self, stem: &Path, value: Option<(Vec<u8>, &'static str)>) {
        let mut m = self.map();
        if m.len() >= VOLATILE_MAX {
            m.clear();
        }
        m.insert(stem.to_owned(), value);
    }

    pub fn clear(&self) {
        self.map().clear();
    }
}
