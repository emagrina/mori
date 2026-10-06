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
