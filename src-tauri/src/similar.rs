//! Similar media: photos and videos that are probably copies of the same
//! picture or clip although their bytes differ (other format, size,
//! compression, metadata, a light edit).
//!
//! This is an *estimate*, kept strictly apart from exact duplicates
//! (`dupes.rs`), and every result is a suggestion for human review.
//!
//! Pipeline:
//!
//! 1. **Collect** the selected folders only (same walker as exact duplicates).
//! 2. **Fingerprint.** Photos: the sandboxed worker decodes them and returns
//!    two 64×64 grayscale miniatures (whole frame, central 90 %). Videos: 12
//!    sampled frames captured by the webview's guarded video path, sent back
//!    as raw 64×64 pixels. Results are cached per file version.
//! 3. **Candidates.** 64-bit DCT perceptual hashes go into a multi-index hash
//!    table (7 bands; probing each band at distance ≤ 1 finds every pair
//!    within 13 bits without comparing all pairs). Videos only meet videos of
//!    similar aspect ratio and duration.
//! 4. **Verify** candidates on the miniatures themselves after
//!    brightness/contrast normalization, globally and per 8×8-pixel block, so
//!    a moved subject or different text in one region rejects a pair.
//! 5. **Group** around the best-quality copy: every other member must have
//!    been verified against it directly.
//!
//! Cleanup reuses `dupes::execute`, so the same rules apply: at least one copy
//! kept, the kept copy re-checked first, Live Photos moved as a whole.

use crate::dupes::{self, Analysis, Cancelled, FileRec, Group, Member, Root, Stats};
use crate::index::Kind;
use crate::secure;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SIDE: usize = 64;
pub const PLANE: usize = SIDE * SIDE;
/// Frames sampled per video, at (i + 0.5) / VIDEO_SAMPLES of its duration.
pub const VIDEO_SAMPLES: usize = 12;
const PHOTO_EXT: &[&str] = &["jpg", "jpeg", "png", "webp", "heic", "heif"];
const VIDEO_EXT: &[&str] = &["mp4", "mov", "m4v", "webm"];
/// Largest photo handed to the worker (same as previews).
const MAX_PHOTO: u64 = crate::worker::MAX_INPUT;

// ------------------------------------------------------------- settings

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Sensitivity {
    Strict,
    Balanced,
    Broad,
}

/// Acceptance limits. Distances are on 64-bit perceptual hashes; `mad` is the
/// mean absolute difference of the normalized miniatures (in standard
/// deviations) and `block` the worst 8×8 block.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    pub hash: u32,
    pub mad: f32,
    pub block: f32,
    pub frame_hash: u32,
    pub frame_mad: f32,
    pub frame_block: f32,
    /// Share of a video's informative frames that must match, in order.
    pub frame_ratio: f32,
}

impl Sensitivity {
    pub fn thresholds(self) -> Thresholds {
        match self {
            Sensitivity::Strict => Thresholds {
                hash: 6,
                mad: 0.12,
                block: 0.30,
                frame_hash: 8,
                frame_mad: 0.16,
                frame_block: 0.45,
                frame_ratio: 0.8,
            },
            Sensitivity::Balanced => Thresholds {
                hash: 10,
                mad: 0.20,
                block: 0.45,
                frame_hash: 12,
                frame_mad: 0.24,
                frame_block: 0.60,
                frame_ratio: 0.7,
            },
            Sensitivity::Broad => Thresholds {
                hash: 13,
                mad: 0.28,
                block: 0.60,
                frame_hash: 14,
                frame_mad: 0.32,
                frame_block: 0.75,
                frame_ratio: 0.6,
            },
        }
    }
}

pub struct Spec {
    pub roots: Vec<Root>,
    pub photos: bool,
    pub videos: bool,
    pub recursive: bool,
    pub sensitivity: Sensitivity,
}

// ------------------------------------------------------------- progress

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Collecting,
    Photos,
    Videos,
    Comparing,
    Done,
}

#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stage: Stage,
    pub photos_total: u64,
    pub photos_done: u64,
    pub videos_total: u64,
    pub videos_done: u64,
    pub compare_total: u64,
    pub compare_done: u64,
    pub groups: u64,
}

#[derive(Default, Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SimStats {
    pub photos: u64,
    pub videos: u64,
    /// Files that could not be safely decoded / sampled.
    pub unanalyzable: u64,
    /// Too plain (e.g. a solid colour) to compare meaningfully.
    pub uninformative: u64,
    pub cached: u64,
    /// Hash comparisons made while generating candidates.
    pub hash_comparisons: u64,
    /// Candidate pairs checked on the miniatures themselves.
    pub verified: u64,
    pub matches: u64,
    /// Matches hidden because you marked them as not duplicates.
    pub dismissed: u64,
    pub live_pairs: u64,
    pub elapsed_ms: u64,
}

// ------------------------------------------------------- environment

/// Video frame source: (file index, file, sample times in seconds).
pub type CaptureFn<'a> = dyn FnMut(usize, &FileRec, &[f64]) -> Result<Capture, CaptureError> + 'a;

/// What the analysis needs from the app, injected so the core is testable.
pub struct Env<'a> {
    /// Fingerprint cache directory (`None` = no cache).
    pub cache_dir: Option<PathBuf>,
    /// Image bytes → (oriented width, height, `FP_VARIANTS` miniatures).
    /// In the app this is the sandboxed worker.
    pub decode: &'a (dyn Fn(Vec<u8>) -> Option<(u32, u32, Vec<u8>)> + Sync),
    /// Sampled video frames for `files[index]`: at `times` (seconds), or at
    /// the standard `VIDEO_SAMPLES` positions when `times` is empty. Called
    /// on the analysis thread, one video at a time.
    pub capture: &'a mut CaptureFn<'a>,
    /// Called once the file list is known (the app registers opaque ids).
    pub registered: &'a mut dyn FnMut(&[Root], &[FileRec]),
    /// Pairs the user marked as "not duplicates".
    pub dismissed: &'a HashSet<PairKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureError {
    /// The video can't be safely decoded (blocked codec, damaged, failed):
    /// remembered until the file changes.
    Failed,
    /// Not answered (cancelled, timed out, UI busy): tried again next time.
    Unavailable,
}

/// Frames reported for one video.
pub struct Capture {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub container: String,
    pub codec: String,
    /// One grayscale 64×64 plane per requested time (`VIDEO_SAMPLES` by default).
    pub planes: Vec<u8>,
}

// ---------------------------------------------------------------- results

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub width: u32,
    pub height: u32,
    pub duration_ms: Option<u32>,
    pub container: Option<String>,
    pub codec: Option<String>,
    /// Camera metadata present (EXIF block found).
    pub exif: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberMeta {
    /// Estimated similarity to the suggested copy (0–99), or 100 when the
    /// bytes are confirmed identical by a full hash.
    pub similarity: u8,
    pub exact: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupMeta {
    pub video: bool,
    /// Lowest member similarity in the group.
    pub similarity: u8,
    pub members: Vec<MemberMeta>,
}

pub struct SimilarAnalysis {
    /// Groups and files in the same shape as exact duplicates, so cleanup
    /// shares `dupes::execute`.
    pub analysis: Analysis,
    pub meta: Vec<GroupMeta>,
    pub media: HashMap<usize, MediaInfo>,
    /// Stable identity (size + content fingerprint) per file in a group.
    pub keys: HashMap<usize, IdKey>,
    pub stats: SimStats,
}

/// Content identity of a file version, independent of its name or location.
pub type IdKey = [u8; 16];
pub type PairKey = (IdKey, IdKey);

pub fn pair_key(a: IdKey, b: IdKey) -> PairKey {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn id_key(size: u64, partial: &[u8; 32]) -> IdKey {
    let mut h = blake3::Hasher::new();
    h.update(b"mori-similar-id");
    h.update(&size.to_le_bytes());
    h.update(partial);
    h.finalize().as_bytes()[..16].try_into().unwrap()
}

// ------------------------------------------------------------ image math

/// Mean/standard-deviation normalized plane (brightness and contrast
/// independent). `None` if the plane is nearly uniform.
fn normalize(p: &[u8]) -> Option<Vec<f32>> {
    let n = p.len() as f32;
    let mean = p.iter().map(|&v| v as f32).sum::<f32>() / n;
    let var = p.iter().map(|&v| (v as f32 - mean).powi(2)).sum::<f32>() / n;
    let sd = var.sqrt();
    (sd >= 6.0).then(|| p.iter().map(|&v| (v as f32 - mean) / sd).collect())
}

/// 3×3 box blur: tolerates the sub-pixel shifts that resampling introduces.
fn blur(v: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0; PLANE];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let mut s = 0.0;
            let mut n = 0.0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if (0..SIDE as i32).contains(&xx) && (0..SIDE as i32).contains(&yy) {
                        s += v[yy as usize * SIDE + xx as usize];
                        n += 1.0;
                    }
                }
            }
            out[y * SIDE + x] = s / n;
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub struct Diff {
    pub mad: f32,
    pub block: f32,
}

/// Difference between two normalized, blurred planes.
fn diff(a: &[f32], b: &[f32]) -> Diff {
    let mut blocks = [0f32; 64];
    let mut total = 0.0;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let d = (a[y * SIDE + x] - b[y * SIDE + x]).abs();
            total += d;
            blocks[(y / 8) * 8 + x / 8] += d;
        }
    }
    let block = blocks.iter().fold(0f32, |m, &v| m.max(v / 64.0));
    Diff { mad: total / PLANE as f32, block }
}

/// Prepared plane: normalized + blurred, or `None` when uninformative.
fn prepare(p: &[u8]) -> Option<Vec<f32>> {
    normalize(p).map(|v| blur(&v))
}

fn cos_table() -> &'static [[f32; 32]; 8] {
    static T: std::sync::OnceLock<[[f32; 32]; 8]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0f32; 32]; 8];
        for (k, row) in t.iter_mut().enumerate() {
            for (n, c) in row.iter_mut().enumerate() {
                *c = (std::f32::consts::PI * (2 * n + 1) as f32 * k as f32 / 64.0).cos();
            }
        }
        t
    })
}

/// 64-bit perceptual hash: the 8×8 lowest DCT frequencies of a 32×32
/// reduction, each bit = above the median (DC excluded).
pub fn phash(plane: &[u8]) -> u64 {
    let mut x = [[0f32; 32]; 32];
    for (y, row) in x.iter_mut().enumerate() {
        for (xx, v) in row.iter_mut().enumerate() {
            let i = (2 * y) * SIDE + 2 * xx;
            *v = (plane[i] as f32 + plane[i + 1] as f32 + plane[i + SIDE] as f32 + plane[i + SIDE + 1] as f32) / 4.0;
        }
    }
    let c = cos_table();
    // Rows then columns, only the 8 lowest frequencies.
    let mut t = [[0f32; 32]; 8];
    for k in 0..8 {
        for m in 0..32 {
            t[k][m] = (0..32).map(|n| c[k][n] * x[n][m]).sum();
        }
    }
    let mut coef = [0f32; 64];
    for k in 0..8 {
        for l in 0..8 {
            coef[k * 8 + l] = (0..32).map(|m| c[l][m] * t[k][m]).sum();
        }
    }
    let mut ac: Vec<f32> = coef[1..].to_vec();
    ac.sort_by(|a, b| a.total_cmp(b));
    let median = ac[ac.len() / 2];
    let mut h = 0u64;
    for (i, &v) in coef.iter().enumerate().skip(1) {
        if v > median {
            h |= 1 << i;
        }
    }
    h
}

/// 0–99 % from a verified difference (identical bytes are reported as 100
/// separately, never from this estimate).
fn similarity(d: Diff) -> u8 {
    // Two unrelated normalized planes differ by ≈1.13 on average.
    (100.0 * (1.0 - d.mad / 1.13).clamp(0.0, 1.0)).round().min(99.0) as u8
}

// ---------------------------------------------------------- fingerprint cache

const MAGIC: &[u8; 4] = b"MSF1";
/// Bump when the fingerprint pipeline changes: older records are ignored.
const CACHE_TAG: u32 = 0x5132;

enum Record {
    Photo {
        width: u32,
        height: u32,
        exif: bool,
        planes: Vec<u8>,
    },
    Video {
        width: u32,
        height: u32,
        duration_ms: u32,
        container: String,
        codec: String,
        planes: Vec<u8>,
    },
    /// Could not be safely analyzed: not retried until the file changes.
    Failed,
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = &s.as_bytes()[..s.len().min(32)];
    out.push(b.len() as u8);
    out.extend_from_slice(b);
}

fn encode(r: &Record) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    match r {
        Record::Failed => out.push(0),
        Record::Photo { width, height, exif, planes } => {
            out.push(1);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            out.push(*exif as u8);
            out.extend_from_slice(planes);
        }
        Record::Video { width, height, duration_ms, container, codec, planes } => {
            out.push(2);
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&height.to_le_bytes());
            out.extend_from_slice(&duration_ms.to_le_bytes());
            put_str(&mut out, container);
            put_str(&mut out, codec);
            out.extend_from_slice(planes);
        }
    }
    out
}

/// Strict parser: anything malformed is treated as a cache miss.
fn decode_record(b: &[u8]) -> Option<Record> {
    let rest = b.strip_prefix(MAGIC)?;
    let (&kind, rest) = rest.split_first()?;
    let u32_at = |s: &[u8], i: usize| s.get(i..i + 4).map(|v| u32::from_le_bytes(v.try_into().unwrap()));
    match kind {
        0 if rest.is_empty() => Some(Record::Failed),
        1 => {
            let (width, height) = (u32_at(rest, 0)?, u32_at(rest, 4)?);
            let exif = *rest.get(8)? == 1;
            let planes = rest.get(9..)?;
            (planes.len() == crate::worker::FP_LEN).then(|| Record::Photo {
                width,
                height,
                exif,
                planes: planes.to_vec(),
            })
        }
        2 => {
            let (width, height, duration_ms) = (u32_at(rest, 0)?, u32_at(rest, 4)?, u32_at(rest, 8)?);
            let mut at = 12;
            let mut take_str = || {
                let n = *rest.get(at)? as usize;
                let s = std::str::from_utf8(rest.get(at + 1..at + 1 + n)?).ok()?.to_owned();
                at += 1 + n;
                Some(s)
            };
            let container = take_str()?;
            let codec = take_str()?;
            let planes = rest.get(at..)?;
            (planes.len() == VIDEO_SAMPLES * PLANE).then(|| Record::Video {
                width,
                height,
                duration_ms,
                container,
                codec,
                planes: planes.to_vec(),
            })
        }
        _ => None,
    }
}

fn cache_path(dir: &Path, canon: &Path, meta: &fs::Metadata) -> PathBuf {
    crate::thumbs::stem(dir, canon, meta, CACHE_TAG).with_extension("sim")
}

fn cache_read(path: &Path) -> Option<Record> {
    let b = fs::read(path).ok()?;
    if b.len() > 64 + VIDEO_SAMPLES * PLANE {
        return None;
    }
    decode_record(&b)
}

fn cache_write(path: &Path, r: &Record) -> bool {
    let Some(dir) = path.parent() else { return false };
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = path.with_extension("part");
    fs::write(&tmp, encode(r)).is_ok() && fs::rename(&tmp, path).is_ok()
}

// ------------------------------------------------------------------ items

/// Where an item's miniatures live: the cache file, or memory if caching failed.
#[derive(Clone)]
enum Planes {
    Cache(PathBuf),
    Mem(Arc<Vec<u8>>),
}

impl Planes {
    fn load(&self) -> Option<Arc<Vec<u8>>> {
        match self {
            Planes::Mem(v) => Some(v.clone()),
            Planes::Cache(p) => match cache_read(p)? {
                Record::Photo { planes, .. } | Record::Video { planes, .. } => Some(Arc::new(planes)),
                Record::Failed => None,
            },
        }
    }
}

struct Photo {
    file: usize,
    width: u32,
    height: u32,
    exif: bool,
    /// Hash per variant (whole, central 90 %); `None` if uninformative.
    hashes: [Option<u64>; 2],
    planes: Planes,
}

struct Video {
    file: usize,
    width: u32,
    height: u32,
    duration_ms: u32,
    container: String,
    codec: String,
    /// Per sampled frame: hash if the frame is informative (not black/flat).
    frames: Vec<Option<u64>>,
    planes: Planes,
}

/// Look for an EXIF block in the first bytes (JPEG APP1 / HEIF item); raw
/// byte search only, nothing is parsed.
fn has_exif(head: &[u8]) -> bool {
    head.windows(6).any(|w| w == b"Exif\0\0") || head.windows(8).any(|w| w == b"infeExif" || &w[4..] == b"Exif")
}

type Decode<'a> = &'a (dyn Fn(Vec<u8>) -> Option<(u32, u32, Vec<u8>)> + Sync);

fn photo_item(
    root: &Path,
    i: usize,
    f: &FileRec,
    cache_dir: Option<&Path>,
    decode: Decode,
    cached: &AtomicU64,
) -> Option<Photo> {
    let (mut file, meta, canon) = secure::open_inside(root, &f.rel).ok()?;
    let path = cache_dir.map(|d| cache_path(d, &canon, &meta));
    let from = |r: Record, planes: Planes| match r {
        Record::Photo { width, height, exif, planes: p } => {
            let hashes = [0, 1].map(|v| {
                let plane = &p[v * PLANE..(v + 1) * PLANE];
                normalize(plane).map(|_| phash(plane))
            });
            Some(Photo { file: i, width, height, exif, hashes, planes })
        }
        _ => None,
    };
    if let Some(p) = &path {
        if let Some(r) = cache_read(p) {
            cached.fetch_add(1, Ordering::Relaxed);
            return from(r, Planes::Cache(p.clone()));
        }
    }
    // Magic bytes decide; the extension only pre-selected the file.
    let head = secure::read_head(&mut file, secure::SNIFF_LEN);
    {
        use std::io::Seek;
        file.rewind().ok()?;
    }
    if !secure::sniff(&head, &f.ext).is_image() || meta.len() > MAX_PHOTO {
        if let Some(p) = &path {
            cache_write(p, &Record::Failed);
        }
        return None;
    }
    let bytes = secure::read_limited(file, &meta, MAX_PHOTO).ok()?;
    let exif = has_exif(&bytes[..bytes.len().min(256 * 1024)]);
    let Some((width, height, planes)) = decode(bytes) else {
        if let Some(p) = &path {
            cache_write(p, &Record::Failed);
        }
        return None;
    };
    if planes.len() != crate::worker::FP_LEN {
        return None;
    }
    let record = Record::Photo { width, height, exif, planes: planes.clone() };
    let store = match &path {
        Some(p) if cache_write(p, &record) => Planes::Cache(p.clone()),
        _ => Planes::Mem(Arc::new(planes)),
    };
    from(record, store)
}

fn video_item(root: &Path, i: usize, f: &FileRec, env: &mut Env, cached: &mut u64) -> Option<Video> {
    let (_, meta, canon) = secure::open_inside(root, &f.rel).ok()?;
    let path = env.cache_dir.as_ref().map(|d| cache_path(d, &canon, &meta));
    let record = match path.as_ref().and_then(|p| cache_read(p)) {
        Some(r) => {
            *cached += 1;
            r
        }
        None => {
            let r = match (env.capture)(i, f, &[]) {
                Err(CaptureError::Unavailable) => return None,
                Ok(c) if c.planes.len() == VIDEO_SAMPLES * PLANE && c.width > 0 && c.height > 0 => Record::Video {
                    width: c.width,
                    height: c.height,
                    duration_ms: c.duration_ms,
                    container: c.container,
                    codec: c.codec,
                    planes: c.planes,
                },
                _ => Record::Failed,
            };
            if let Some(p) = &path {
                cache_write(p, &r);
            }
            r
        }
    };
    let Record::Video { width, height, duration_ms, container, codec, planes } = record else { return None };
    let frames = planes.chunks(PLANE).map(|p| normalize(p).map(|_| phash(p))).collect();
    let store = match &path {
        Some(p) if p.exists() => Planes::Cache(p.clone()),
        _ => Planes::Mem(Arc::new(planes)),
    };
    Some(Video { file: i, width, height, duration_ms, container, codec, frames, planes: store })
}

// ------------------------------------------------------------- candidates

/// Multi-index hashing: the 64-bit hash is split into 4 bands of 16 bits
/// (≈ log2 of a large collection, so buckets stay small). If two hashes are
/// within `r` bits, at least one band differs by at most ⌊r/4⌋ bits
/// (pigeonhole), so probing every band value within that radius finds every
/// such pair. Distances are checked while probing and only pairs within `r`
/// (for any variant combination) are kept: no all-pairs scan, no pair list
/// proportional to n².
fn photo_candidates(photos: &[Photo], r: u32, comparisons: &AtomicU64) -> Vec<(usize, usize)> {
    const BANDS: usize = 4;
    let band = |h: u64, b: usize| ((h >> (16 * b)) & 0xFFFF) as usize;
    // Every 16-bit flip mask with at most r/4 bits set.
    let radius = (r as usize / BANDS).min(4);
    let masks: Vec<usize> = (0..1usize << 16).filter(|m| m.count_ones() as usize <= radius).collect();
    let mut table: Vec<Vec<Vec<u32>>> = (0..BANDS).map(|_| vec![Vec::new(); 1 << 16]).collect();
    for (pi, p) in photos.iter().enumerate() {
        for h in p.hashes.iter().flatten() {
            for (b, t) in table.iter_mut().enumerate() {
                let slot = &mut t[band(*h, b)];
                if slot.last() != Some(&(pi as u32)) {
                    slot.push(pi as u32);
                }
            }
        }
    }
    let close = |a: &Photo, b: &Photo| {
        a.hashes.iter().flatten().any(|ha| b.hashes.iter().flatten().any(|hb| (ha ^ hb).count_ones() <= r))
    };
    let mut out = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut checked = 0u64;
    for (pi, p) in photos.iter().enumerate() {
        seen.clear();
        for h in p.hashes.iter().flatten() {
            for (b, t) in table.iter().enumerate() {
                let v = band(*h, b);
                for &m in &masks {
                    for &q in &t[v ^ m] {
                        if q as usize > pi && seen.insert(q) {
                            checked += 1;
                            if close(p, &photos[q as usize]) {
                                out.push((pi, q as usize));
                            }
                        }
                    }
                }
            }
        }
    }
    comparisons.fetch_add(checked, Ordering::Relaxed);
    out
}

fn aspect_close(aw: u32, ah: u32, bw: u32, bh: u32, tol: f64) -> bool {
    let (ra, rb) = (aw as f64 / ah.max(1) as f64, bw as f64 / bh.max(1) as f64);
    (ra.ln() - rb.ln()).abs() <= tol
}

/// Verified photo match → similarity estimate.
fn match_photos(a: &Photo, b: &Photo, t: &Thresholds) -> Option<u8> {
    if !aspect_close(a.width, a.height, b.width, b.height, 0.12) {
        return None;
    }
    // Best hash distance over (whole, central 90 %) combinations.
    let mut best: Option<(u32, usize, usize)> = None;
    for (va, ha) in a.hashes.iter().enumerate() {
        for (vb, hb) in b.hashes.iter().enumerate() {
            if let (Some(ha), Some(hb)) = (ha, hb) {
                let d = (ha ^ hb).count_ones();
                if best.is_none_or(|(bd, ..)| d < bd) {
                    best = Some((d, va, vb));
                }
            }
        }
    }
    let (d, va, vb) = best?;
    if d > t.hash {
        return None;
    }
    let (pa, pb) = (a.planes.load()?, b.planes.load()?);
    let (na, nb) = (prepare(&pa[va * PLANE..(va + 1) * PLANE])?, prepare(&pb[vb * PLANE..(vb + 1) * PLANE])?);
    let df = diff(&na, &nb);
    (df.mad <= t.mad && df.block <= t.block).then(|| similarity(df))
}

/// Videos match when most of their informative sampled frames match, in the
/// same order, spread across the clip (a shared intro alone is not enough).
fn match_videos(a: &Video, b: &Video, t: &Thresholds) -> Option<u8> {
    let info = |v: &Video| v.frames.iter().filter(|f| f.is_some()).count();
    let (ia, ib) = (info(a), info(b));
    if ia < 4 || ib < 4 {
        return None;
    }
    let (pa, pb) = (a.planes.load()?, b.planes.load()?);
    let prep = |p: &[u8], k: usize| prepare(&p[k * PLANE..(k + 1) * PLANE]);
    let pb_frames: Vec<Option<Vec<f32>>> = (0..VIDEO_SAMPLES).map(|k| b.frames[k].and(prep(&pb, k))).collect();
    // (frame in a, best frame in b, similarity)
    let mut matches: Vec<(usize, usize, u8)> = Vec::new();
    for (i, ha) in a.frames.iter().enumerate() {
        let Some(ha) = ha else { continue };
        let Some(fa) = prep(&pa, i) else { continue };
        let mut best: Option<(f32, usize, Diff)> = None;
        for (j, hb) in b.frames.iter().enumerate() {
            let (Some(hb), Some(fb)) = (hb, &pb_frames[j]) else { continue };
            if (ha ^ hb).count_ones() > t.frame_hash {
                continue;
            }
            let d = diff(&fa, fb);
            if d.mad <= t.frame_mad && d.block <= t.frame_block && best.is_none_or(|(m, ..)| d.mad < m) {
                best = Some((d.mad, j, d));
            }
        }
        if let Some((_, j, d)) = best {
            matches.push((i, j, similarity(d)));
        }
    }
    // Longest order-preserving chain of matches.
    let n = matches.len();
    let mut len = vec![1usize; n];
    let mut prev = vec![usize::MAX; n];
    for x in 0..n {
        for y in 0..x {
            if matches[y].1 < matches[x].1 && len[y] + 1 > len[x] {
                len[x] = len[y] + 1;
                prev[x] = y;
            }
        }
    }
    let end = (0..n).max_by_key(|&x| len[x])?;
    let mut chain = Vec::new();
    let mut k = end;
    while k != usize::MAX {
        chain.push(matches[k]);
        k = prev[k];
    }
    chain.reverse();
    let need = ((t.frame_ratio * ia.min(ib) as f32).ceil() as usize).max(4);
    let span = chain.last()?.0 - chain.first()?.0;
    if chain.len() < need || (span as f32) < 0.6 * (VIDEO_SAMPLES - 1) as f32 {
        return None;
    }
    Some((chain.iter().map(|m| m.2 as u32).sum::<u32>() / chain.len() as u32) as u8)
}

/// Sample time (seconds) of standard frame `k` in a clip of `duration_ms`.
fn sample_time(duration_ms: u32, k: usize) -> f64 {
    duration_ms as f64 / 1000.0 * (k as f64 + 0.5) / VIDEO_SAMPLES as f64
}

/// Worth a closer look: a couple of sampled frames already agree.
fn plausible_videos(a: &Video, b: &Video, t: &Thresholds) -> bool {
    a.frames
        .iter()
        .flatten()
        .filter(|ha| b.frames.iter().flatten().any(|hb| (*ha ^ hb).count_ones() <= t.frame_hash))
        .count()
        >= 2
}

/// Second stage for trimmed clips: re-sample the shorter video exactly at the
/// times matching the longer one's samples, assuming the clips line up at the
/// start or at the end, and require most aligned frames to match.
fn match_aligned(long: &Video, short: &Video, file: &FileRec, env: &mut Env, t: &Thresholds) -> Option<u8> {
    let delta = long.duration_ms.saturating_sub(short.duration_ms) as f64 / 1000.0;
    let dur_short = short.duration_ms as f64 / 1000.0;
    let pl = long.planes.load()?;
    let mut best: Option<u8> = None;
    for offset in [0.0, delta] {
        // Long-clip samples that fall inside the short clip under this hypothesis.
        let ks: Vec<usize> = (0..VIDEO_SAMPLES)
            .filter(|&k| long.frames[k].is_some())
            .filter(|&k| (0.05..=dur_short - 0.05).contains(&(sample_time(long.duration_ms, k) - offset)))
            .collect();
        if ks.len() < 4 {
            continue;
        }
        let times: Vec<f64> = ks.iter().map(|&k| sample_time(long.duration_ms, k) - offset).collect();
        let Ok(c) = (env.capture)(short.file, file, &times) else { continue };
        if c.planes.len() != times.len() * PLANE {
            continue;
        }
        let mut sims = Vec::new();
        let mut matched = Vec::new();
        for (n, &k) in ks.iter().enumerate() {
            let p = &c.planes[n * PLANE..(n + 1) * PLANE];
            let (Some(hl), Some(fs)) = (long.frames[k], prepare(p)) else { continue };
            if (hl ^ phash(p)).count_ones() > t.frame_hash {
                continue;
            }
            let Some(fl) = prepare(&pl[k * PLANE..(k + 1) * PLANE]) else { continue };
            let d = diff(&fl, &fs);
            if d.mad <= t.frame_mad && d.block <= t.frame_block {
                sims.push(similarity(d) as u32);
                matched.push(k);
            }
        }
        let need = ((t.frame_ratio * ks.len() as f32).ceil() as usize).max(4);
        let span_ok = match (matched.first(), matched.last(), ks.first(), ks.last()) {
            (Some(a), Some(b), Some(lo), Some(hi)) => (b - a) as f32 >= 0.6 * (hi - lo) as f32,
            _ => false,
        };
        if matched.len() >= need && span_ok {
            let sim = (sims.iter().sum::<u32>() / sims.len() as u32) as u8;
            best = Some(best.map_or(sim, |b| b.max(sim)));
        }
    }
    best
}

fn video_candidates(videos: &[Video], comparisons: &AtomicU64) -> Vec<(usize, usize)> {
    // Bucket by aspect ratio, then a sliding window over duration.
    let mut order: Vec<usize> = (0..videos.len()).collect();
    order.sort_by_key(|&i| videos[i].duration_ms);
    let mut out = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        let a = &videos[i];
        let limit = (a.duration_ms as f64 * 1.15) as u32 + 2000;
        for &j in &order[k + 1..] {
            let b = &videos[j];
            if b.duration_ms > limit {
                break;
            }
            comparisons.fetch_add(1, Ordering::Relaxed);
            if aspect_close(a.width, a.height, b.width, b.height, 0.05) {
                out.push((i.min(j), i.max(j)));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- quality

fn name_penalty(f: &FileRec) -> f64 {
    let n = f.name.to_lowercase();
    let words = [
        "whatsapp",
        "copy",
        "compressed",
        "resized",
        "small",
        "edited",
        "-min",
        "export",
        "screenshot",
        "telegram",
        "signal-",
        "share",
        "thumb",
        "preview",
    ];
    let mut p = words.iter().filter(|w| n.contains(*w)).count() as f64 * 4.0;
    if (1..10).any(|k| n.contains(&format!("({k})"))) {
        p += 3.0;
    }
    let path = f.rel.to_lowercase();
    if ["download", "tmp", "temp", "cache", "whatsapp", "telegram"].iter().any(|w| path.contains(w)) {
        p += 2.0;
    }
    p
}

/// Higher is better. Resolution dominates; camera metadata, the original
/// capture format and less compression help; derivative-looking names hurt.
/// Only a suggestion.
fn quality(a: &SimilarAnalysisDraft, file: usize) -> f64 {
    let f = &a.files[file];
    let m = &a.media[&file];
    let pixels = (m.width as f64 * m.height as f64).max(1.0);
    let mut q = pixels.ln() * 10.0 - name_penalty(f);
    if let Some(d) = m.duration_ms {
        let secs = (d as f64 / 1000.0).max(0.1);
        q += secs.ln() * 8.0 + ((f.size as f64 / secs).max(1.0)).ln();
        if f.ext == "mov" {
            q += 1.0;
        }
    } else {
        q += (f.size as f64 / pixels).max(1e-6).ln();
        if m.exif {
            q += 3.0;
        }
        q += match f.ext.as_str() {
            "heic" | "heif" => 2.0,
            "jpg" | "jpeg" => 1.0,
            _ => 0.0,
        };
    }
    q
}

struct SimilarAnalysisDraft<'a> {
    files: &'a [FileRec],
    media: &'a HashMap<usize, MediaInfo>,
}

// ------------------------------------------------------------- the analysis

/// Map analysis-item index (photo or video) to member files (Live Photos: still + motion).
struct Node {
    file: usize,
    video: bool,
}

pub fn analyze(
    spec: Spec,
    env: &mut Env,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Result<SimilarAnalysis, Cancelled> {
    let started = Instant::now();
    let t = spec.sensitivity.thresholds();
    let mut last = Instant::now();
    let mut emit = |p: Progress, force: bool, progress: &mut dyn FnMut(&Progress)| {
        if force || last.elapsed() > Duration::from_millis(120) {
            last = Instant::now();
            progress(&p);
        }
    };
    let base = Progress {
        stage: Stage::Collecting,
        photos_total: 0,
        photos_done: 0,
        videos_total: 0,
        videos_done: 0,
        compare_total: 0,
        compare_done: 0,
        groups: 0,
    };

    // 1. Collect.
    let dspec = dupes::Spec { roots: spec.roots, kinds: None, recursive: spec.recursive };
    let mut dstats = Stats::default();
    let all = dupes::collect(&dspec, cancel, &mut dstats, &mut |n| {
        emit(Progress { photos_done: n, ..base.clone() }, false, progress)
    })?;
    let roots = dspec.roots;
    let pairs = dupes::live_pairs(&all);
    let motion_of: HashMap<usize, usize> = pairs.iter().copied().collect();
    let motions: HashSet<usize> = pairs.iter().map(|&(_, m)| m).collect();
    let is_photo = |f: &FileRec| f.kind == Kind::Photo && PHOTO_EXT.contains(&f.ext.as_str()) && f.size > 0;
    let is_video = |f: &FileRec| f.kind == Kind::Video && VIDEO_EXT.contains(&f.ext.as_str()) && f.size > 0;
    // Keep analyzed files plus the motion halves of Live Photo stills.
    let mut keep = vec![false; all.len()];
    for (i, f) in all.iter().enumerate() {
        if (spec.photos && is_photo(f)) || (spec.videos && is_video(f) && !motions.contains(&i)) {
            keep[i] = true;
            if let Some(&m) = motion_of.get(&i) {
                if spec.photos {
                    keep[m] = true;
                }
            }
        }
    }
    let mut remap = vec![usize::MAX; all.len()];
    let mut files: Vec<FileRec> = Vec::new();
    for (i, f) in all.into_iter().enumerate() {
        if keep[i] {
            remap[i] = files.len();
            files.push(f);
        }
    }
    let live: HashMap<usize, usize> = motion_of
        .iter()
        .filter(|(s, m)| remap[**s] != usize::MAX && remap[**m] != usize::MAX)
        .map(|(s, m)| (remap[*s], remap[*m]))
        .collect();
    let live_motion: HashSet<usize> = live.values().copied().collect();
    let photo_files: Vec<usize> =
        (0..files.len()).filter(|&i| spec.photos && is_photo(&files[i]) && !live_motion.contains(&i)).collect();
    let video_files: Vec<usize> =
        (0..files.len()).filter(|&i| spec.videos && is_video(&files[i]) && !live_motion.contains(&i)).collect();
    (env.registered)(&roots, &files);

    let mut stats = SimStats {
        photos: photo_files.len() as u64,
        videos: video_files.len() as u64,
        live_pairs: live.len() as u64,
        ..Default::default()
    };
    let base = Progress { photos_total: stats.photos, videos_total: stats.videos, ..base };

    // 2a. Photo fingerprints (bounded parallel workers; the global worker
    // limit applies on top).
    let slots: Vec<Mutex<Option<Photo>>> = photo_files.iter().map(|_| Mutex::new(None)).collect();
    let idx: Vec<usize> = (0..photo_files.len()).collect();
    let done = AtomicU64::new(0);
    let cached = AtomicU64::new(0);
    let roots_canon: Vec<PathBuf> = roots.iter().map(|r| r.canon.clone()).collect();
    {
        let cache_dir = env.cache_dir.clone();
        let decode = env.decode;
        dupes::parallel(
            &idx,
            3,
            cancel,
            &done,
            || {
                emit(
                    Progress { stage: Stage::Photos, photos_done: done.load(Ordering::Relaxed), ..base.clone() },
                    false,
                    progress,
                )
            },
            |k| {
                let fi = photo_files[k];
                let f = &files[fi];
                *slots[k].lock().unwrap() =
                    photo_item(&roots_canon[f.root], fi, f, cache_dir.as_deref(), decode, &cached);
                Ok(())
            },
        )?;
    }
    let mut photos: Vec<Photo> = Vec::new();
    for s in slots {
        match s.into_inner().unwrap() {
            Some(p) if p.hashes.iter().any(Option::is_some) => photos.push(p),
            Some(_) => stats.uninformative += 1,
            None => stats.unanalyzable += 1,
        }
    }

    // 2b. Video fingerprints, one video at a time.
    let mut videos: Vec<Video> = Vec::new();
    let mut vcached = 0u64;
    for (k, &fi) in video_files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        emit(
            Progress { stage: Stage::Videos, photos_done: stats.photos, videos_done: k as u64, ..base.clone() },
            k == 0,
            progress,
        );
        let f = files[fi].clone();
        match video_item(&roots_canon[f.root], fi, &f, env, &mut vcached) {
            Some(v) if v.frames.iter().filter(|f| f.is_some()).count() >= 4 => videos.push(v),
            Some(_) => stats.uninformative += 1,
            None => stats.unanalyzable += 1,
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    stats.cached = cached.load(Ordering::Relaxed) + vcached;

    // 3–4. Candidates, then verification.
    let comparisons = AtomicU64::new(0);
    let pc = photo_candidates(&photos, t.hash, &comparisons);
    let vc = video_candidates(&videos, &comparisons);
    stats.hash_comparisons = comparisons.load(Ordering::Relaxed);
    let total = (pc.len() + vc.len()) as u64;
    let cmp_base = Progress {
        stage: Stage::Comparing,
        photos_done: stats.photos,
        videos_done: stats.videos,
        compare_total: total,
        ..base.clone()
    };
    // Nodes: photos first, then videos.
    let mut nodes: Vec<Node> = photos.iter().map(|p| Node { file: p.file, video: false }).collect();
    nodes.extend(videos.iter().map(|v| Node { file: v.file, video: true }));
    let off = photos.len();
    let verified = AtomicU64::new(0);
    let edges: Mutex<Vec<(usize, usize, u8)>> = Mutex::new(Vec::new());
    let cmp_done = AtomicU64::new(0);
    let jobs: Vec<usize> = (0..pc.len()).collect();
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4);
    dupes::parallel(
        &jobs,
        workers,
        cancel,
        &cmp_done,
        || emit(Progress { compare_done: cmp_done.load(Ordering::Relaxed), ..cmp_base.clone() }, false, progress),
        |k| {
            let (a, b) = pc[k];
            verified.fetch_add(1, Ordering::Relaxed);
            if let Some(sim) = match_photos(&photos[a], &photos[b], &t) {
                edges.lock().unwrap().push((a, b, sim));
            }
            Ok(())
        },
    )?;
    let mut edges = edges.into_inner().unwrap();
    // Videos: sequential (a second, aligned sampling may be needed).
    for (n, &(a, b)) in vc.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        emit(Progress { compare_done: pc.len() as u64 + n as u64, ..cmp_base.clone() }, false, progress);
        verified.fetch_add(1, Ordering::Relaxed);
        let (va, vb) = (&videos[a], &videos[b]);
        let mut sim = match_videos(va, vb, &t);
        if sim.is_none() && plausible_videos(va, vb, &t) {
            let (long, short) = if va.duration_ms >= vb.duration_ms { (va, vb) } else { (vb, va) };
            let file = files[short.file].clone();
            sim = match_aligned(long, short, &file, env, &t);
        }
        if let Some(sim) = sim {
            edges.push((off + a, off + b, sim));
        }
    }
    stats.verified = verified.load(Ordering::Relaxed);
    edges.sort_unstable();
    stats.matches = edges.len() as u64;

    // Identity keys for every file that might end up in a group (also used by
    // cleanup to re-check files), and exact-byte confirmation.
    let mut involved: HashSet<usize> = HashSet::new();
    for &(a, b, _) in &edges {
        for n in [a, b] {
            involved.insert(nodes[n].file);
            if let Some(&m) = live.get(&nodes[n].file) {
                involved.insert(m);
            }
        }
    }
    let mut keys: HashMap<usize, IdKey> = HashMap::new();
    for &fi in &involved {
        let f = &files[fi];
        let size = f.size;
        let fp = dupes::open_unchanged(&roots_canon[f.root], f)
            .ok()
            .and_then(|mut file| dupes::partial_fingerprint(&mut file, size).ok());
        if let Some((p, full)) = fp {
            files[fi].partial = Some(p);
            files[fi].full = full;
            keys.insert(fi, id_key(size, &p));
        }
    }
    // Drop edges the user dismissed, or whose files changed / vanished.
    let before = edges.len();
    edges.retain(|&(a, b, _)| match (keys.get(&nodes[a].file), keys.get(&nodes[b].file)) {
        (Some(&ka), Some(&kb)) => !env.dismissed.contains(&pair_key(ka, kb)),
        _ => false,
    });
    stats.dismissed = (before - edges.len()) as u64;

    // 5. Groups anchored on the best copy.
    let mut media: HashMap<usize, MediaInfo> = HashMap::new();
    for p in &photos {
        media.insert(p.file, MediaInfo { width: p.width, height: p.height, exif: p.exif, ..Default::default() });
    }
    for v in &videos {
        media.insert(
            v.file,
            MediaInfo {
                width: v.width,
                height: v.height,
                duration_ms: Some(v.duration_ms),
                container: Some(v.container.clone()),
                codec: Some(v.codec.clone()),
                exif: false,
            },
        );
    }
    let draft = SimilarAnalysisDraft { files: &files, media: &media };
    let mut adj: HashMap<usize, HashMap<usize, u8>> = HashMap::new();
    for &(a, b, s) in &edges {
        adj.entry(a).or_default().insert(b, s);
        adj.entry(b).or_default().insert(a, s);
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut meta: Vec<GroupMeta> = Vec::new();
    let mut visited: HashSet<usize> = HashSet::new();
    let mut starts: Vec<usize> = adj.keys().copied().collect();
    starts.sort_unstable();
    for s in starts {
        if visited.contains(&s) {
            continue;
        }
        // Connected component.
        let mut comp = vec![s];
        visited.insert(s);
        let mut k = 0;
        while k < comp.len() {
            let n = comp[k];
            for &m in adj[&n].keys() {
                if visited.insert(m) {
                    comp.push(m);
                }
            }
            k += 1;
        }
        // Split into keeper-anchored groups: members must match the keeper itself.
        let mut rest: Vec<usize> = comp;
        while rest.len() > 1 {
            // A Live Photo (still + motion) carries more than its still alone.
            let q =
                |n: usize| quality(&draft, nodes[n].file) + if live.contains_key(&nodes[n].file) { 5.0 } else { 0.0 };
            rest.sort_by(|&x, &y| {
                q(y).total_cmp(&q(x))
                    .then_with(|| files[nodes[x].file].modified.cmp(&files[nodes[y].file].modified))
                    .then_with(|| files[nodes[x].file].rel.cmp(&files[nodes[y].file].rel))
            });
            let keeper = rest[0];
            let linked: Vec<usize> = rest[1..].iter().copied().filter(|n| adj[&keeper].contains_key(n)).collect();
            if linked.is_empty() {
                rest.remove(0);
                continue;
            }
            let mut members = vec![Member { files: member_files(nodes[keeper].file, &live), locked: false }];
            let mut mmeta = vec![MemberMeta { similarity: 100, exact: false }];
            for &n in &linked {
                let exact = same_bytes(&files, &roots_canon, nodes[keeper].file, nodes[n].file, cancel);
                members.push(Member { files: member_files(nodes[n].file, &live), locked: false });
                mmeta.push(MemberMeta { similarity: if exact { 100 } else { adj[&keeper][&n] }, exact });
            }
            let lowest = mmeta[1..].iter().map(|m| m.similarity).min().unwrap_or(100);
            groups.push(Group { members, live: false, unit_size: 0, suggested: 0 });
            meta.push(GroupMeta { video: nodes[keeper].video, similarity: lowest, members: mmeta });
            rest.retain(|n| *n != keeper && !linked.contains(n));
        }
    }
    // Most similar first, photos before videos.
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&g| (meta[g].video, std::cmp::Reverse(meta[g].similarity), groups[g].members.len()));
    let groups: Vec<Group> = order.iter().map(|&g| groups[g].clone()).collect();
    let meta: Vec<GroupMeta> = order.iter().map(|&g| meta[g].clone()).collect();

    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    progress(&Progress {
        stage: Stage::Done,
        photos_done: stats.photos,
        videos_done: stats.videos,
        compare_done: total,
        compare_total: total,
        groups: groups.len() as u64,
        ..base
    });
    dstats.live_pairs = stats.live_pairs;
    Ok(SimilarAnalysis { analysis: Analysis { roots, files, groups, stats: dstats }, meta, media, keys, stats })
}

fn member_files(file: usize, live: &HashMap<usize, usize>) -> Vec<usize> {
    match live.get(&file) {
        Some(&m) => vec![file, m],
        None => vec![file],
    }
}

/// Confirm byte identity with the exact-duplicate hashes (never inferred
/// from the perceptual estimate).
fn same_bytes(files: &[FileRec], roots: &[PathBuf], a: usize, b: usize, cancel: &AtomicBool) -> bool {
    let (fa, fb) = (&files[a], &files[b]);
    if fa.size != fb.size || fa.partial.is_none() || fa.partial != fb.partial {
        return false;
    }
    let full = |f: &FileRec| -> Option<[u8; 32]> {
        if f.full.is_some() {
            return f.full;
        }
        let mut file = dupes::open_unchanged(&roots[f.root], f).ok()?;
        dupes::full_hash(&mut file, f.size, cancel, &AtomicU64::new(0)).ok().flatten()
    };
    matches!((full(fa), full(fb)), (Some(x), Some(y)) if x == y)
}

/// Drop removed files (cleanup or browser Trash) from the results.
pub fn forget(s: &mut SimilarAnalysis, removed: &HashSet<usize>) {
    let a = &mut s.analysis;
    let mut keep_groups = Vec::new();
    let mut keep_meta = Vec::new();
    for (g, m) in a.groups.drain(..).zip(s.meta.drain(..)) {
        let (members, mm): (Vec<_>, Vec<_>) = g
            .members
            .into_iter()
            .zip(m.members)
            .filter(|(mem, _)| !mem.files.iter().any(|f| removed.contains(f)))
            .unzip();
        if members.len() > 1 {
            let lowest = mm[1..].iter().map(|x: &MemberMeta| x.similarity).min().unwrap_or(100);
            keep_groups.push(Group { members, ..g });
            keep_meta.push(GroupMeta { members: mm, similarity: lowest, ..m });
        }
    }
    a.groups = keep_groups;
    s.meta = keep_meta;
    for g in &mut a.groups {
        g.suggested = 0;
    }
}

/// Remove one group (Not duplicates) or one member (Not a match) from the
/// results, returning the pairs to remember as dismissed.
pub fn dismiss(s: &mut SimilarAnalysis, group: usize, member: Option<usize>) -> Vec<PairKey> {
    let Some(g) = s.analysis.groups.get(group) else { return Vec::new() };
    let key = |m: &Member| s.keys.get(&m.files[0]).copied();
    let mut out = Vec::new();
    for (i, a) in g.members.iter().enumerate() {
        for (j, b) in g.members.iter().enumerate().skip(i + 1) {
            if member.is_none_or(|m| m == i || m == j) {
                if let (Some(ka), Some(kb)) = (key(a), key(b)) {
                    out.push(pair_key(ka, kb));
                }
            }
        }
    }
    match member {
        Some(m) if g.members.len() > 2 && m > 0 => {
            s.analysis.groups[group].members.remove(m);
            s.meta[group].members.remove(m);
            s.meta[group].similarity = s.meta[group].members[1..].iter().map(|x| x.similarity).min().unwrap_or(100);
        }
        _ => {
            s.analysis.groups.remove(group);
            s.meta.remove(group);
        }
    }
    out
}

// ------------------------------------------------------- dismissed pairs

/// "Not duplicates" decisions, stored locally as pairs of content identities
/// (no names or paths), so unchanged files aren't suggested again.
pub fn load_dismissed(path: &Path) -> HashSet<PairKey> {
    let Ok(b) = fs::read(path) else { return HashSet::new() };
    if b.len() > 32 * 200_000 || b.len() % 32 != 0 {
        return HashSet::new();
    }
    b.chunks(32).map(|c| (c[..16].try_into().unwrap(), c[16..].try_into().unwrap())).collect()
}

pub fn save_dismissed(path: &Path, set: &HashSet<PairKey>) -> std::io::Result<()> {
    let mut v: Vec<&PairKey> = set.iter().collect();
    v.sort();
    let mut out = Vec::with_capacity(v.len() * 32);
    for (a, b) in v {
        out.extend_from_slice(a);
        out.extend_from_slice(b);
    }
    let tmp = path.with_extension("part");
    fs::write(&tmp, out)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..PLANE).map(|i| f(i % SIDE, i / SIDE)).collect()
    }

    #[test]
    fn phash_is_stable_under_brightness_and_sensitive_to_content() {
        let a = plane(|x, y| ((x * 3 + y * 2) % 256) as u8 ^ ((x / 8 + y / 8) % 2 * 90) as u8);
        let brighter: Vec<u8> = a.iter().map(|&v| (v as f32 * 0.8 + 30.0) as u8).collect();
        let other = plane(|x, y| ((x * y) % 256) as u8);
        assert!((phash(&a) ^ phash(&brighter)).count_ones() <= 4);
        assert!((phash(&a) ^ phash(&other)).count_ones() > 16);
    }

    #[test]
    fn local_change_is_caught_by_block_test() {
        let a = plane(|x, y| ((x * 7 + y * 13) % 200) as u8 + ((x + y) % 3) as u8 * 10);
        let mut b = a.clone();
        for y in 20..30 {
            for x in 20..30 {
                b[y * SIDE + x] = 255 - b[y * SIDE + x];
            }
        }
        let (na, nb) = (prepare(&a).unwrap(), prepare(&b).unwrap());
        let d = diff(&na, &nb);
        assert!(d.block > d.mad * 3.0, "{d:?}");
    }

    #[test]
    fn uniform_planes_are_uninformative() {
        assert!(normalize(&vec![128u8; PLANE]).is_none());
    }

    #[test]
    fn cache_records_roundtrip_and_reject_garbage() {
        let r = Record::Video {
            width: 1920,
            height: 1080,
            duration_ms: 1234,
            container: "MP4".into(),
            codec: "avc1".into(),
            planes: vec![7; VIDEO_SAMPLES * PLANE],
        };
        match decode_record(&encode(&r)) {
            Some(Record::Video { width: 1920, duration_ms: 1234, codec, .. }) => assert_eq!(codec, "avc1"),
            _ => panic!("roundtrip"),
        }
        let mut bad = encode(&r);
        bad.truncate(bad.len() - 1);
        assert!(decode_record(&bad).is_none());
        assert!(decode_record(b"MSF1\x07").is_none());
        assert!(decode_record(b"").is_none());
    }

    #[test]
    fn dismissed_pairs_roundtrip() {
        let p = std::env::temp_dir().join(format!("mori-dismissed-{}", std::process::id()));
        let set: HashSet<PairKey> = [pair_key([1; 16], [2; 16]), pair_key([9; 16], [3; 16])].into();
        save_dismissed(&p, &set).unwrap();
        assert_eq!(load_dismissed(&p), set);
        assert_eq!(pair_key([9; 16], [3; 16]), ([3; 16], [9; 16]));
        let _ = fs::remove_file(p);
    }

    // ------------------------------------------------ end to end (in-process)

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn temp_lab() -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("mori-sim-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&d).unwrap();
        fs::canonicalize(d).unwrap()
    }

    /// A textured "photo": overlapping gradients and rings, seeded.
    fn scene(seed: u32, w: u32, h: u32) -> image::RgbImage {
        image::RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let s = seed as f32;
            let r = ((fx * (3.0 + s)).sin() * 0.5 + 0.5) * 255.0;
            let g = (((fx - 0.5).hypot(fy - 0.4) * (14.0 + s * 3.0)).cos() * 0.5 + 0.5) * 255.0;
            let b = ((fy * (5.0 + s * 2.0) + fx * s).cos() * 0.5 + 0.5) * 255.0;
            image::Rgb([r as u8, g as u8, b as u8])
        })
    }

    fn save(img: &image::RgbImage, path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(path).unwrap();
    }

    fn run(lab: &Path, dismissed: &HashSet<PairKey>, cancel: &AtomicBool) -> Result<SimilarAnalysis, Cancelled> {
        run_with(lab, HashSet::new(), dismissed, cancel)
    }

    fn run_with(
        lab: &Path,
        private: HashSet<String>,
        dismissed: &HashSet<PairKey>,
        cancel: &AtomicBool,
    ) -> Result<SimilarAnalysis, Cancelled> {
        let decode = |b: Vec<u8>| crate::worker::fingerprint_in_process(&b);
        let mut capture = |_: usize, _: &FileRec, _: &[f64]| Err(CaptureError::Failed);
        let mut registered = |_: &[Root], _: &[FileRec]| {};
        let mut env =
            Env { cache_dir: None, decode: &decode, capture: &mut capture, registered: &mut registered, dismissed };
        let spec = Spec {
            roots: vec![Root { canon: lab.to_path_buf(), label: "Lab".into(), private }],
            photos: true,
            videos: true,
            recursive: true,
            sensitivity: Sensitivity::Balanced,
        };
        analyze(spec, &mut env, cancel, &mut |_| {})
    }

    fn names(r: &SimilarAnalysis, g: usize) -> Vec<String> {
        r.analysis.groups[g].members.iter().map(|m| r.analysis.files[m.files[0]].rel.clone()).collect()
    }

    #[test]
    fn finds_variants_rejects_different_images_and_keeps_the_original() {
        let lab = temp_lab();
        let a = scene(1, 1600, 1200);
        save(&a, &lab.join("DCIM/IMG_0001.jpg"));
        save(
            &image::imageops::resize(&a, 800, 600, image::imageops::FilterType::Triangle),
            &lab.join("WhatsApp/WhatsApp Image.png"),
        );
        save(
            &image::imageops::resize(&a, 400, 300, image::imageops::FilterType::Triangle),
            &lab.join("Downloads/small copy.jpg"),
        );
        save(&scene(4, 1600, 1200), &lab.join("DCIM/IMG_0002.jpg"));
        save(&image::RgbImage::from_pixel(640, 480, image::Rgb([90, 90, 90])), &lab.join("flat.png"));
        let r = run(&lab, &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(
            r.analysis.groups.len(),
            1,
            "{:?}",
            (0..r.analysis.groups.len()).map(|g| names(&r, g)).collect::<Vec<_>>()
        );
        let n = names(&r, 0);
        assert_eq!(n[0], "DCIM/IMG_0001.jpg", "highest resolution original suggested");
        assert_eq!(n.len(), 3);
        assert!(r.meta[0].members.iter().skip(1).all(|m| !m.exact && m.similarity < 100), "never shown as identical");
        assert_eq!(r.stats.uninformative, 1, "the flat image is not compared");
        fs::remove_dir_all(lab).unwrap();
    }

    /// Private folders (privacy.rs): skipped from a parent, analysed when
    /// chosen explicitly.
    #[test]
    fn private_folders_are_skipped_unless_chosen_explicitly() {
        let lab = temp_lab();
        let a = scene(6, 1200, 900);
        save(&a, &lab.join("Private/original.jpg"));
        save(
            &image::imageops::resize(&a, 600, 450, image::imageops::FilterType::Triangle),
            &lab.join("Private/small.png"),
        );
        save(&scene(7, 1200, 900), &lab.join("Public/other.jpg"));
        let from_parent =
            run_with(&lab, ["Private".to_string()].into(), &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert!(from_parent.analysis.groups.is_empty());
        assert!(from_parent.analysis.files.iter().all(|f| !f.rel.starts_with("Private")));
        assert_eq!(from_parent.stats.photos, 1);
        let explicit = run(&lab.join("Private"), &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(explicit.analysis.groups.len(), 1);
        fs::remove_dir_all(lab).unwrap();
    }

    #[test]
    fn identical_bytes_are_confirmed_by_hash() {
        let lab = temp_lab();
        save(&scene(2, 1200, 900), &lab.join("a.png"));
        fs::create_dir_all(lab.join("b")).unwrap();
        fs::copy(lab.join("a.png"), lab.join("b/a copy.png")).unwrap();
        let r = run(&lab, &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.meta[0].members[1].similarity, 100);
        assert!(r.meta[0].members[1].exact);
        fs::remove_dir_all(lab).unwrap();
    }

    #[test]
    fn cleanup_keeps_a_copy_rechecks_the_keeper_and_moves_live_photos_whole() {
        let lab = temp_lab();
        let a = scene(3, 1600, 1200);
        save(&a, &lab.join("Live/IMG_0100.jpg"));
        fs::write(lab.join("Live/IMG_0100.mov"), b"motion").unwrap();
        save(
            &image::imageops::resize(&a, 800, 600, image::imageops::FilterType::Triangle),
            &lab.join("Export/IMG_0100-small.jpg"),
        );
        let r = run(&lab, &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.analysis.groups.len(), 1);
        let g = &r.analysis.groups[0];
        assert_eq!(g.members[0].files.len(), 2, "the Live Photo (still + motion) is one copy, and the suggested one");
        let trashed = std::cell::RefCell::new(Vec::<String>::new());
        let mut trash = |_: &Path, rel: &str| {
            trashed.borrow_mut().push(rel.to_string());
            Ok(())
        };
        // Never every copy.
        let all = [dupes::PlanItem { group: 0, trash: vec![0, 1] }];
        assert_eq!(
            dupes::execute(&r.analysis, &all, &mut trash, &AtomicBool::new(false), &mut |_, _| {}).unwrap_err(),
            "At least one copy must be kept."
        );
        // Trashing the Live Photo moves both halves.
        let live = [dupes::PlanItem { group: 0, trash: vec![0] }];
        let out = dupes::execute(&r.analysis, &live, &mut trash, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert_eq!(out.trashed_files, 2);
        assert_eq!(*trashed.borrow(), ["Live/IMG_0100.jpg", "Live/IMG_0100.mov"]);
        // The kept copy changed before cleanup: nothing in the group moves.
        trashed.borrow_mut().clear();
        save(&scene(9, 800, 600), &lab.join("Export/IMG_0100-small.jpg"));
        let out = dupes::execute(&r.analysis, &live, &mut trash, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        assert_eq!((out.trashed_files, out.groups_skipped), (0, 1));
        assert!(trashed.borrow().is_empty());
        fs::remove_dir_all(lab).unwrap();
    }

    #[test]
    fn not_duplicates_is_remembered_by_content_and_cancel_stops() {
        let lab = temp_lab();
        let a = scene(5, 1200, 900);
        save(&a, &lab.join("one.jpg"));
        save(&image::imageops::resize(&a, 600, 450, image::imageops::FilterType::Triangle), &lab.join("two.png"));
        let mut r = run(&lab, &HashSet::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(r.analysis.groups.len(), 1);
        let pairs = dismiss(&mut r, 0, None);
        assert_eq!(pairs.len(), 1);
        assert!(r.analysis.groups.is_empty());
        // Renamed/moved files are still recognised (identity is content, not name).
        fs::create_dir_all(lab.join("moved")).unwrap();
        fs::rename(lab.join("two.png"), lab.join("moved/renamed.png")).unwrap();
        let dismissed: HashSet<PairKey> = pairs.into_iter().collect();
        let r = run(&lab, &dismissed, &AtomicBool::new(false)).unwrap();
        assert!(r.analysis.groups.is_empty());
        assert_eq!(r.stats.dismissed, 1);
        assert!(run(&lab, &HashSet::new(), &AtomicBool::new(true)).is_err(), "cancelled");
        fs::remove_dir_all(lab).unwrap();
    }

    #[test]
    fn multi_index_finds_all_pairs_within_13_bits() {
        // Random hashes plus near neighbours at known distances.
        let mut rng = 0x1234_5678_9abc_def0u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mk = |h: u64, i: usize| Photo {
            file: i,
            width: 4,
            height: 3,
            exif: false,
            hashes: [Some(h), None],
            planes: Planes::Mem(Arc::new(Vec::new())),
        };
        let mut photos = Vec::new();
        let mut expected = HashSet::new();
        for i in 0..400 {
            let h = next();
            photos.push(mk(h, photos.len()));
            if i % 4 == 0 {
                let d = (i / 4) % 14; // distances 0..=13
                let mut g = h;
                let mut flipped = HashSet::new();
                while flipped.len() < d {
                    let bit = next() % 64;
                    if flipped.insert(bit) {
                        g ^= 1 << bit;
                    }
                }
                expected.insert((photos.len() - 1, photos.len()));
                photos.push(mk(g, photos.len()));
            }
        }
        let cmp = AtomicU64::new(0);
        let found: HashSet<(usize, usize)> = photo_candidates(&photos, 13, &cmp).into_iter().collect();
        for e in &expected {
            assert!(found.contains(e), "missed pair {e:?}");
        }
        let n = photos.len() as u64;
        assert!(cmp.load(Ordering::Relaxed) < n * n / 8, "not near all-pairs: {}", cmp.load(Ordering::Relaxed));
    }
}

/// Calibration and end-to-end checks on a generated lab (see the PR):
/// `MORI_SIMLAB=/Volumes/Drive/SimilarLab cargo test --release -- --ignored --nocapture simlab`
#[cfg(test)]
mod lab {
    use super::*;
    use std::process::Command;

    /// Frames as the webview capture produces them: 12 positions, 256 px
    /// intermediate, 64×64 grayscale.
    fn ffmpeg_capture(path: &Path, times: &[f64]) -> Option<Capture> {
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,codec_name:format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&probe.stdout);
        let mut lines = text.lines();
        let stream: Vec<&str> = lines.next()?.split(',').collect();
        let duration: f64 = lines.next()?.trim().parse().ok()?;
        let (codec, width, height) =
            (stream.first()?.to_string(), stream.get(1)?.parse().ok()?, stream.get(2)?.parse().ok()?);
        let mut planes = Vec::new();
        let default: Vec<f64> =
            (0..VIDEO_SAMPLES).map(|k| duration * (k as f64 + 0.5) / VIDEO_SAMPLES as f64).collect();
        for &t in if times.is_empty() { &default } else { times } {
            let out = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &format!("{t:.3}"), "-i"])
                .arg(path)
                .args([
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=256:256,scale=64:64:flags=area,format=gray",
                    "-f",
                    "rawvideo",
                    "-",
                ])
                .output()
                .ok()?;
            if out.stdout.len() != PLANE {
                return None;
            }
            planes.extend_from_slice(&out.stdout);
        }
        Some(Capture {
            width,
            height,
            duration_ms: (duration * 1000.0) as u32,
            container: "test".into(),
            codec,
            planes,
        })
    }

    fn run(lab: &Path, s: Sensitivity, cache: Option<PathBuf>) -> SimilarAnalysis {
        let decode = |b: Vec<u8>| crate::worker::fingerprint_in_process(&b);
        let lab_c = lab.to_path_buf();
        let mut capture = move |_: usize, f: &FileRec, times: &[f64]| {
            ffmpeg_capture(&lab_c.join(&f.rel), times).ok_or(CaptureError::Failed)
        };
        let mut registered = |_: &[Root], _: &[FileRec]| {};
        let dismissed = HashSet::new();
        let mut env = Env {
            cache_dir: cache,
            decode: &decode,
            capture: &mut capture,
            registered: &mut registered,
            dismissed: &dismissed,
        };
        let spec = Spec {
            roots: vec![Root { canon: fs::canonicalize(lab).unwrap(), label: "Lab".into(), ..Default::default() }],
            photos: true,
            videos: true,
            recursive: true,
            sensitivity: s,
        };
        analyze(spec, &mut env, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    fn dummy_rec() -> FileRec {
        FileRec {
            root: 0,
            rel: String::new(),
            name: String::new(),
            ext: String::new(),
            kind: Kind::Video,
            size: 0,
            mtime_ns: 0,
            modified: 0,
            created: None,
            partial: None,
            full: None,
        }
    }

    fn photo_at(path: &Path) -> Option<Photo> {
        let bytes = fs::read(path).ok()?;
        let (width, height, planes) = crate::worker::fingerprint_in_process(&bytes)?;
        let hashes = [0, 1].map(|v| {
            let p = &planes[v * PLANE..(v + 1) * PLANE];
            normalize(p).map(|_| phash(p))
        });
        Some(Photo { file: 0, width, height, exif: false, hashes, planes: Planes::Mem(Arc::new(planes)) })
    }

    fn video_at(path: &Path) -> Option<Video> {
        let c = ffmpeg_capture(path, &[])?;
        let frames = c.planes.chunks(PLANE).map(|p| normalize(p).map(|_| phash(p))).collect();
        Some(Video {
            file: 0,
            width: c.width,
            height: c.height,
            duration_ms: c.duration_ms,
            container: c.container,
            codec: c.codec,
            frames,
            planes: Planes::Mem(Arc::new(c.planes)),
        })
    }

    /// Raw metrics for every labelled pair (used to choose the thresholds).
    #[test]
    #[ignore]
    fn simlab_metrics() {
        let Some(lab) = std::env::var_os("MORI_SIMLAB").map(PathBuf::from) else { return };
        let expected: serde_json::Value =
            serde_json::from_slice(&fs::read(lab.join("expected.json")).unwrap()).unwrap();
        let loose = Thresholds {
            hash: 64,
            mad: 9.0,
            block: 9.0,
            frame_hash: 64,
            frame_mad: 9.0,
            frame_block: 9.0,
            frame_ratio: 0.0,
        };
        for list in ["must", "must_not"] {
            println!("--- {list}");
            for p in expected[list].as_array().unwrap() {
                let (a, b, why) =
                    (lab.join(p[0].as_str().unwrap()), lab.join(p[1].as_str().unwrap()), p[2].as_str().unwrap());
                if why.starts_with("video") {
                    let (Some(va), Some(vb)) = (video_at(&a), video_at(&b)) else {
                        println!("{why:45} video unavailable");
                        continue;
                    };
                    // Per-frame best distances, then the verdicts per sensitivity.
                    let best: Vec<String> = va
                        .frames
                        .iter()
                        .map(|h| match h {
                            Some(h) => vb
                                .frames
                                .iter()
                                .flatten()
                                .map(|g| (h ^ g).count_ones())
                                .min()
                                .map_or("-".into(), |d| d.to_string()),
                            None => "·".into(),
                        })
                        .collect();
                    let verdicts: Vec<String> = [Sensitivity::Strict, Sensitivity::Balanced, Sensitivity::Broad]
                        .iter()
                        .map(|s| {
                            let t = s.thresholds();
                            let direct = match_videos(&va, &vb, &t);
                            let aligned = if direct.is_none() && plausible_videos(&va, &vb, &t) {
                                let (long, short, sp) =
                                    if va.duration_ms >= vb.duration_ms { (&va, &vb, &b) } else { (&vb, &va, &a) };
                                let sp = sp.clone();
                                let mut cap = move |_: usize, _: &FileRec, ts: &[f64]| {
                                    ffmpeg_capture(&sp, ts).ok_or(CaptureError::Failed)
                                };
                                let decode = |_: Vec<u8>| None;
                                let mut reg = |_: &[Root], _: &[FileRec]| {};
                                let dis = HashSet::new();
                                let mut env = Env {
                                    cache_dir: None,
                                    decode: &decode,
                                    capture: &mut cap,
                                    registered: &mut reg,
                                    dismissed: &dis,
                                };
                                let fr = dummy_rec();
                                match_aligned(long, short, &fr, &mut env, &t)
                            } else {
                                None
                            };
                            format!("{direct:?}/{aligned:?}")
                        })
                        .collect();
                    println!(
                        "{why:45} frames {} | {} | dur {} vs {}",
                        best.join(","),
                        verdicts.join(" "),
                        va.duration_ms,
                        vb.duration_ms
                    );
                    let _ = &loose;
                } else {
                    let (Some(pa), Some(pb)) = (photo_at(&a), photo_at(&b)) else {
                        println!("{why:45} photo unavailable");
                        continue;
                    };
                    let mut rows = Vec::new();
                    for va in 0..2 {
                        for vb in 0..2 {
                            let (Some(ha), Some(hb)) = (pa.hashes[va], pb.hashes[vb]) else { continue };
                            let (Planes::Mem(xa), Planes::Mem(xb)) = (&pa.planes, &pb.planes) else { unreachable!() };
                            let d = diff(
                                &prepare(&xa[va * PLANE..(va + 1) * PLANE]).unwrap(),
                                &prepare(&xb[vb * PLANE..(vb + 1) * PLANE]).unwrap(),
                            );
                            rows.push(format!("{va}{vb}:h{:2} m{:.2} b{:.2}", (ha ^ hb).count_ones(), d.mad, d.block));
                        }
                    }
                    println!("{why:45} {}", rows.join("  "));
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn simlab() {
        let Some(lab) = std::env::var_os("MORI_SIMLAB").map(PathBuf::from) else { return };
        let expected: serde_json::Value =
            serde_json::from_slice(&fs::read(lab.join("expected.json")).unwrap()).unwrap();
        let cache = std::env::temp_dir().join(format!("mori-simlab-cache-{}", std::process::id()));
        for s in [Sensitivity::Strict, Sensitivity::Balanced, Sensitivity::Broad] {
            let t0 = Instant::now();
            let r = run(&lab, s, Some(cache.clone()));
            let took = t0.elapsed();
            // Which files share a group (with the keeper)?
            let mut together: HashSet<(String, String)> = HashSet::new();
            for g in &r.analysis.groups {
                let names: Vec<&str> = g.members.iter().map(|m| r.analysis.files[m.files[0]].rel.as_str()).collect();
                for a in &names {
                    for b in &names {
                        together.insert((a.to_string(), b.to_string()));
                    }
                }
            }
            let check = |list: &str| -> Vec<(String, String, String)> {
                expected[list]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| {
                        (p[0].as_str().unwrap().into(), p[1].as_str().unwrap().into(), p[2].as_str().unwrap().into())
                    })
                    .collect()
            };
            let missed: Vec<_> =
                check("must").into_iter().filter(|(a, b, _)| !together.contains(&(a.clone(), b.clone()))).collect();
            let wrong: Vec<_> = check("must_not")
                .into_iter()
                .filter(|(a, b, why)| !why.contains("allowed miss") && together.contains(&(a.clone(), b.clone())))
                .collect();
            let heavy: usize = check("must_not")
                .iter()
                .filter(|(a, b, why)| why.contains("heavy") && together.contains(&(a.clone(), b.clone())))
                .count();
            if s == Sensitivity::Balanced {
                for (g, m) in r.analysis.groups.iter().zip(&r.meta) {
                    let names: Vec<String> = g
                        .members
                        .iter()
                        .zip(&m.members)
                        .map(|(mem, mm)| {
                            format!(
                                "{}{} {}%",
                                r.analysis.files[mem.files[0]].rel,
                                if mem.files.len() > 1 { "+MOV" } else { "" },
                                mm.similarity
                            )
                        })
                        .collect();
                    println!("  [{}%] keep {} | {}", m.similarity, names[0], names[1..].join(" | "));
                }
            }
            println!(
                "\n== {s:?}: {} groups, {took:.1?}, stats {:?}\n   missed {} of {}: {:?}\n   false positives: {:?}\n   heavy crops grouped: {heavy}",
                r.analysis.groups.len(),
                r.stats,
                missed.len(),
                check("must").len(),
                missed.iter().map(|m| &m.2).collect::<Vec<_>>(),
                wrong
            );
        }
        let _ = fs::remove_dir_all(cache);
    }

    fn peak_rss_mb() -> f64 {
        let mut u: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
        // macOS reports bytes, Linux kilobytes.
        u.ru_maxrss as f64 / if cfg!(target_os = "macos") { 1024.0 * 1024.0 } else { 1024.0 }
    }

    /// Scale: 50,000 synthetic photo fingerprints (5 % near-duplicates),
    /// stored as cache files like in the app; candidate generation and
    /// verification only. `cargo test --release -- --ignored --nocapture bench_scale`
    #[test]
    #[ignore]
    fn bench_scale() {
        let n: usize = std::env::var("MORI_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(50_000);
        let dir = std::env::temp_dir().join(format!("mori-bench-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        let t0 = Instant::now();
        let mut photos = Vec::with_capacity(n);
        let mut last: Vec<u8> = Vec::new();
        for i in 0..n {
            let plane: Vec<u8> = if i % 20 == 19 {
                // Near-duplicate of the previous photo: noise + brightness.
                last.iter().map(|&v| (v as f32 * 0.92 + 12.0 + (rnd() - 0.5) * 10.0).clamp(0.0, 255.0) as u8).collect()
            } else {
                let waves: Vec<(f32, f32, f32, f32)> =
                    (0..6).map(|_| (rnd() * 0.4, rnd() * 0.4, rnd() * 6.3, rnd())).collect();
                (0..PLANE)
                    .map(|k| {
                        let (x, y) = ((k % SIDE) as f32, (k / SIDE) as f32);
                        let v: f32 = waves.iter().map(|(fx, fy, ph, a)| a * (fx * x + fy * y + ph).cos()).sum();
                        (128.0 + v * 40.0).clamp(0.0, 255.0) as u8
                    })
                    .collect()
            };
            let mut planes = plane.clone();
            planes.extend_from_slice(&plane);
            let path = dir.join(format!("{i}.sim"));
            assert!(cache_write(&path, &Record::Photo { width: 4000, height: 3000, exif: false, planes }));
            let h = phash(&plane);
            photos.push(Photo {
                file: i,
                width: 4000,
                height: 3000,
                exif: false,
                hashes: [Some(h), None],
                planes: Planes::Cache(path),
            });
            last = plane;
        }
        let prep = t0.elapsed();
        let sens = match std::env::var("MORI_BENCH_SENS").as_deref() {
            Ok("strict") => Sensitivity::Strict,
            Ok("broad") => Sensitivity::Broad,
            _ => Sensitivity::Balanced,
        };
        let t = sens.thresholds();
        println!("BENCH sensitivity {sens:?}");
        let cmp = AtomicU64::new(0);
        let t1 = Instant::now();
        let pairs = photo_candidates(&photos, t.hash, &cmp);
        let cand = t1.elapsed();
        let t2 = Instant::now();
        let matched = pairs.iter().filter(|(a, b)| match_photos(&photos[*a], &photos[*b], &t).is_some()).count();
        let verify = t2.elapsed();
        let all_pairs = (n as u64) * (n as u64 - 1) / 2;
        println!(
            "BENCH scale n={n}: fingerprints prepared in {prep:.1?}; hash comparisons {} ({:.4} % of {all_pairs} possible pairs) in {cand:.1?}; candidates within threshold {}; verified in {verify:.1?}; matches {matched} (expected ≈ {}); peak RSS {:.0} MB",
            cmp.load(Ordering::Relaxed),
            100.0 * cmp.load(Ordering::Relaxed) as f64 / all_pairs as f64,
            pairs.len(),
            n / 20,
            peak_rss_mb()
        );
        let _ = fs::remove_dir_all(dir);
    }

    /// End to end on a real folder through the real sandboxed worker binary
    /// (videos via ffmpeg instead of the webview). Cold, then cached.
    /// `MORI_BENCH_DIR=… MORI_WORKER_BIN=target/release/mori cargo test --release -- --ignored --nocapture bench_folder`
    #[test]
    #[ignore]
    fn bench_folder() {
        let (Some(dir), Some(bin)) = (std::env::var_os("MORI_BENCH_DIR"), std::env::var_os("MORI_WORKER_BIN")) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let bin = PathBuf::from(bin);
        let decode = |bytes: Vec<u8>| -> Option<(u32, u32, Vec<u8>)> {
            use std::io::{Read, Write};
            let heif = crate::heif_flag(&bytes);
            let mut cmd = Command::new(&bin);
            cmd.args(["--mori-worker", "fingerprint", "64"]);
            if heif {
                cmd.arg("heif");
            }
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let mut stdin = child.stdin.take()?;
            let w = std::thread::spawn(move || stdin.write_all(&bytes));
            let mut out = Vec::new();
            child.stdout.take()?.read_to_end(&mut out).ok()?;
            let _ = w.join();
            child.wait().ok()?.success().then_some(())?;
            (out.len() == 16 + crate::worker::FP_LEN && &out[..4] == b"MORI").then(|| {
                (
                    u32::from_le_bytes(out[4..8].try_into().unwrap()),
                    u32::from_le_bytes(out[8..12].try_into().unwrap()),
                    out[16..].to_vec(),
                )
            })
        };
        let cache = std::env::temp_dir().join(format!("mori-bench-cache-{}", std::process::id()));
        for pass in ["cold", "cached"] {
            let lab_c = dir.clone();
            let mut capture = move |_: usize, f: &FileRec, times: &[f64]| {
                ffmpeg_capture(&lab_c.join(&f.rel), times).ok_or(CaptureError::Failed)
            };
            let mut registered = |_: &[Root], _: &[FileRec]| {};
            let dismissed = HashSet::new();
            let mut env = Env {
                cache_dir: Some(cache.clone()),
                decode: &decode,
                capture: &mut capture,
                registered: &mut registered,
                dismissed: &dismissed,
            };
            let spec = Spec {
                roots: vec![Root {
                    canon: fs::canonicalize(&dir).unwrap(),
                    label: "Bench".into(),
                    ..Default::default()
                }],
                photos: true,
                videos: true,
                recursive: true,
                sensitivity: Sensitivity::Balanced,
            };
            let t0 = Instant::now();
            let r = analyze(spec, &mut env, &AtomicBool::new(false), &mut |_| {}).unwrap();
            let s = &r.stats;
            let n = s.photos + s.videos;
            if pass == "cold" {
                for (g, m) in r.analysis.groups.iter().zip(&r.meta) {
                    let names: Vec<String> = g
                        .members
                        .iter()
                        .zip(&m.members)
                        .map(|(x, mm)| format!("{} {}%", r.analysis.files[x.files[0]].rel, mm.similarity))
                        .collect();
                    if !names.iter().any(|n| n.starts_with("Shared/")) || names.len() > 2 {
                        println!("BENCH extra group: {}", names.join(" | "));
                    }
                }
            }
            println!(
                "BENCH folder ({pass}): {} photos, {} videos in {:.1?}; hash comparisons {} ({:.3} % of all pairs); verified {}; matches {}; groups {} ({} photo, {} video); unanalyzable {}; peak RSS {:.0} MB",
                s.photos,
                s.videos,
                t0.elapsed(),
                s.hash_comparisons,
                100.0 * s.hash_comparisons as f64 / (n * n.saturating_sub(1) / 2).max(1) as f64,
                s.verified,
                s.matches,
                r.analysis.groups.len(),
                r.meta.iter().filter(|m| !m.video).count(),
                r.meta.iter().filter(|m| m.video).count(),
                s.unanalyzable,
                peak_rss_mb()
            );
        }
        let _ = fs::remove_dir_all(cache);
    }
}
