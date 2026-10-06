//! Exact duplicate analysis.
//!
//! Files are compared by *content only*, never by name, date or dimensions,
//! and are never decoded: bytes are read in bounded chunks and hashed.
//!
//!   1. size grouping       – a file with a unique size can't have a duplicate
//!   2. partial fingerprint – BLAKE3 of size + start + middle + end
//!   3. full verification   – streaming BLAKE3 of the whole file (1 MiB buffer)
//!
//! Only files whose full hashes match are reported. Nothing here deletes
//! anything; cleanup (`execute`) only ever acts on an explicit, validated plan
//! that keeps at least one copy of every group, and it re-verifies files right
//! before acting.

use crate::index::{self, Kind};
use crate::secure;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};
use walkdir::WalkDir;

/// Bytes read from each of the three regions for the partial fingerprint.
pub const PARTIAL_CHUNK: u64 = 64 * 1024;
/// Streaming buffer for full hashes: memory stays bounded for any file size.
const READ_BUF: usize = 1024 * 1024;
const STILL_EXT: &[&str] = &["heic", "heif", "jpg", "jpeg"];
const MOTION_EXT: &str = "mov";

/// A BLAKE3 digest.
type Hash = [u8; 32];
/// Per-file worker result: (partial fingerprint, full hash, failed).
type HashSlot = (Option<Hash>, Option<Hash>, bool);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    Collecting,
    ComparingSizes,
    Fingerprinting,
    Verifying,
    Done,
}

#[derive(Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub stage: Stage,
    pub files_total: u64,
    pub files_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub groups: u64,
    pub recoverable: u64,
}

#[derive(Default)]
pub struct Root {
    pub canon: PathBuf,
    pub label: String,
    /// Private folders strictly below this root (relative paths): the walk
    /// stops there, before reading anything inside. The root itself is the
    /// user's explicit choice and is never a boundary. See `privacy.rs`.
    pub private: HashSet<String>,
}

#[derive(Clone, Debug)]
pub struct FileRec {
    pub root: usize,
    pub rel: String,
    pub name: String,
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime_ns: u128,
    pub modified: i64,
    pub created: Option<i64>,
    pub partial: Option<[u8; 32]>,
    pub full: Option<[u8; 32]>,
}

/// One logical copy: a single file, or a Live Photo [still, motion] pair.
#[derive(Clone, Debug)]
pub struct Member {
    pub files: Vec<usize>,
    /// Part of a Live Photo whose other component isn't duplicated: removing
    /// it would break the Live Photo, so it must be kept.
    pub locked: bool,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub members: Vec<Member>,
    pub live: bool,
    /// Bytes per copy (sum of the member's files).
    pub unit_size: u64,
    /// Index of the member Mori suggests keeping (only a suggestion).
    pub suggested: usize,
}

impl Group {
    /// Space recovered if every copy except the ones that must stay is removed.
    pub fn recoverable(&self) -> u64 {
        let locked = self.members.iter().filter(|m| m.locked).count();
        self.unit_size * (self.members.len() - locked.max(1)) as u64
    }
}

#[derive(Default, Clone, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub scanned: u64,
    pub empty_ignored: u64,
    pub unreadable: u64,
    pub changed: u64,
    pub hardlinks_skipped: u64,
    pub live_pairs: u64,
}

pub struct Analysis {
    pub roots: Vec<Root>,
    pub files: Vec<FileRec>,
    pub groups: Vec<Group>,
    pub stats: Stats,
}

pub struct Spec {
    pub roots: Vec<Root>,
    /// `None` = all kinds.
    pub kinds: Option<Vec<Kind>>,
    pub recursive: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Cancelled;

// ------------------------------------------------------------------ hashing

pub(crate) fn mtime_ns(meta: &std::fs::Metadata) -> u128 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

/// Open a recorded file, confined to its root, refusing symlinks and
/// non-regular files, and check it still has the recorded size and mtime.
pub(crate) fn open_unchanged(root: &Path, f: &FileRec) -> Result<File, &'static str> {
    let (file, meta, _) = secure::open_inside(root, &f.rel).map_err(|_| "unavailable")?;
    if meta.len() != f.size || mtime_ns(&meta) != f.mtime_ns {
        return Err("changed");
    }
    Ok(file)
}

fn read_at(file: &mut File, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(at))?;
    file.read_exact(buf)
}

/// BLAKE3 over size + three regions. For small files this *is* the full
/// content hash (returned as `(partial, Some(full))`).
pub(crate) fn partial_fingerprint(file: &mut File, size: u64) -> std::io::Result<([u8; 32], Option<[u8; 32]>)> {
    if size <= 3 * PARTIAL_CHUNK {
        let mut all = Vec::with_capacity(size as usize);
        file.seek(SeekFrom::Start(0))?;
        file.by_ref().take(size + 1).read_to_end(&mut all)?;
        if all.len() as u64 != size {
            return Err(std::io::Error::other("size changed"));
        }
        let full = *blake3::hash(&all).as_bytes();
        let mut h = blake3::Hasher::new();
        h.update(&size.to_le_bytes());
        h.update(&all);
        return Ok((*h.finalize().as_bytes(), Some(full)));
    }
    let mut buf = vec![0u8; PARTIAL_CHUNK as usize];
    let mut h = blake3::Hasher::new();
    h.update(&size.to_le_bytes());
    for at in [0, size / 2 - PARTIAL_CHUNK / 2, size - PARTIAL_CHUNK] {
        read_at(file, at, &mut buf)?;
        h.update(&buf);
    }
    Ok((*h.finalize().as_bytes(), None))
}

/// Streaming full-content BLAKE3. Checks `cancel` between chunks.
pub(crate) fn full_hash(
    file: &mut File,
    size: u64,
    cancel: &AtomicBool,
    bytes: &AtomicU64,
) -> Result<Option<[u8; 32]>, Cancelled> {
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_BUF];
    let mut read_total = 0u64;
    if file.seek(SeekFrom::Start(0)).is_err() {
        return Ok(None);
    }
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled);
        }
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Ok(None),
        };
        h.update(&buf[..n]);
        read_total += n as u64;
        bytes.fetch_add(n as u64, Ordering::Relaxed);
        if read_total > size {
            return Ok(None); // grew while reading
        }
    }
    Ok((read_total == size).then(|| *h.finalize().as_bytes()))
}

// ------------------------------------------------------------- collection

pub(crate) fn collect(
    spec: &Spec,
    cancel: &AtomicBool,
    stats: &mut Stats,
    tick: &mut dyn FnMut(u64),
) -> Result<Vec<FileRec>, Cancelled> {
    let mut out = Vec::new();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    #[cfg(unix)]
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    for (ri, root) in spec.roots.iter().enumerate() {
        let walker = WalkDir::new(&root.canon)
            .follow_links(false) // never follow symlinks (no escapes, no loops)
            .same_file_system(true)
            .max_depth(if spec.recursive { 64 } else { 1 })
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                let is_dir = e.file_type().is_dir();
                let boundary = is_dir
                    && !root.private.is_empty()
                    && e.path().strip_prefix(&root.canon).ok().and_then(|r| r.to_str()).is_some_and(|r| {
                        root.private.contains(if cfg!(windows) { r.replace('\\', "/") } else { r.to_owned() }.as_str())
                    });
                !boundary && e.file_name().to_str().is_some_and(|n| !index::should_skip(n, is_dir))
            });
        for (i, item) in walker.enumerate() {
            if i % 256 == 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Cancelled);
                }
                tick(out.len() as u64);
            }
            let Ok(item) = item else {
                stats.unreadable += 1;
                continue;
            };
            // Regular files only: no symlinks, devices, FIFOs or sockets.
            if item.depth() == 0 || !item.file_type().is_file() {
                continue;
            }
            let Some(rel) = item.path().strip_prefix(&root.canon).ok().and_then(|r| r.to_str()) else { continue };
            let rel = if cfg!(windows) { rel.replace('\\', "/") } else { rel.to_owned() };
            let Ok(meta) = item.metadata() else {
                stats.unreadable += 1;
                continue;
            };
            // Overlapping locations (e.g. Home and Pictures) list a file once.
            if !seen_paths.insert(item.path().to_path_buf()) {
                continue;
            }
            // Hard links are the same file: not a duplicate, and trashing one
            // would recover nothing.
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() > 1 && !seen_inodes.insert((meta.dev(), meta.ino())) {
                    stats.hardlinks_skipped += 1;
                    continue;
                }
            }
            let name = item.file_name().to_string_lossy().into_owned();
            let entry = index::make_entry(rel.clone(), name.clone(), false, &meta);
            out.push(FileRec {
                root: ri,
                rel,
                name,
                ext: entry.ext,
                kind: entry.kind,
                size: meta.len(),
                mtime_ns: mtime_ns(&meta),
                modified: entry.modified,
                created: entry.created,
                partial: None,
                full: None,
            });
        }
    }
    stats.scanned = out.len() as u64;
    Ok(out)
}

fn stem_key(f: &FileRec) -> (usize, String) {
    let stem = f.rel.rsplit_once('.').map_or(f.rel.as_str(), |(s, _)| s);
    (f.root, stem.to_lowercase())
}

/// Live Photo components: exactly one still (HEIC/HEIF/JPEG) and one MOV with
/// the same name in the same folder.
pub fn live_pairs(files: &[FileRec]) -> Vec<(usize, usize)> {
    let mut by_stem: HashMap<(usize, String), (Vec<usize>, Vec<usize>)> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let slot = by_stem.entry(stem_key(f)).or_default();
        if STILL_EXT.contains(&f.ext.as_str()) {
            slot.0.push(i);
        } else if f.ext == MOTION_EXT {
            slot.1.push(i);
        }
    }
    let mut pairs: Vec<_> =
        by_stem.into_values().filter(|(s, m)| s.len() == 1 && m.len() == 1).map(|(s, m)| (s[0], m[0])).collect();
    pairs.sort();
    pairs
}

// --------------------------------------------------------------- analysis

/// Run in parallel over `items` with a few worker threads while the calling
/// thread reports progress.
pub(crate) fn parallel<F>(
    items: &[usize],
    workers: usize,
    cancel: &AtomicBool,
    done: &AtomicU64,
    mut report: impl FnMut(),
    work: F,
) -> Result<(), Cancelled>
where
    F: Fn(usize) -> Result<(), Cancelled> + Sync,
{
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                if k >= items.len() || cancel.load(Ordering::Relaxed) {
                    break;
                }
                if work(items[k]).is_err() {
                    failed.store(true, Ordering::Relaxed);
                    break;
                }
                done.fetch_add(1, Ordering::Relaxed);
            });
        }
        while (done.load(Ordering::Relaxed) as usize) < items.len()
            && !cancel.load(Ordering::Relaxed)
            && !failed.load(Ordering::Relaxed)
        {
            report();
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    report();
    if cancel.load(Ordering::Relaxed) || failed.load(Ordering::Relaxed) {
        return Err(Cancelled);
    }
    Ok(())
}

pub fn analyze(spec: Spec, cancel: &AtomicBool, progress: &mut dyn FnMut(&Progress)) -> Result<Analysis, Cancelled> {
    let mut stats = Stats::default();
    let mut last = Instant::now();
    let mut emit = |p: Progress, force: bool, progress: &mut dyn FnMut(&Progress)| {
        if force || last.elapsed() > Duration::from_millis(120) {
            last = Instant::now();
            progress(&p);
        }
    };
    let base = Progress {
        stage: Stage::Collecting,
        files_total: 0,
        files_done: 0,
        bytes_total: 0,
        bytes_done: 0,
        groups: 0,
        recoverable: 0,
    };

    // 0. Collect regular files from the authorised locations only.
    let all = collect(&spec, cancel, &mut stats, &mut |n| {
        emit(Progress { files_done: n, files_total: n, ..base.clone() }, false, progress)
    })?;

    // Kind filter. Live Photo partners of included files are kept too, so a
    // still is never judged without knowing about its motion component.
    let pairs_all = live_pairs(&all);
    let mut partner: HashMap<usize, usize> = HashMap::new();
    for &(s, m) in &pairs_all {
        partner.insert(s, m);
        partner.insert(m, s);
    }
    let wanted = |f: &FileRec| match &spec.kinds {
        None => true,
        Some(k) => k.contains(&f.kind),
    };
    let mut keep = vec![false; all.len()];
    for (i, f) in all.iter().enumerate() {
        if wanted(f) {
            keep[i] = true;
            if let Some(&p) = partner.get(&i) {
                keep[p] = true;
            }
        }
    }
    let mut remap = vec![usize::MAX; all.len()];
    let mut files = Vec::new();
    for (i, f) in all.into_iter().enumerate() {
        if keep[i] {
            remap[i] = files.len();
            files.push(f);
        }
    }
    let pairs: Vec<(usize, usize)> = pairs_all
        .into_iter()
        .filter(|(s, m)| remap[*s] != usize::MAX && remap[*m] != usize::MAX)
        .map(|(s, m)| (remap[s], remap[m]))
        .collect();
    stats.live_pairs = pairs.len() as u64;

    // 1. Size grouping.
    let total = files.len() as u64;
    emit(Progress { stage: Stage::ComparingSizes, files_total: total, ..base.clone() }, true, progress);
    let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if f.size == 0 {
            stats.empty_ignored += 1;
            continue;
        }
        by_size.entry(f.size).or_default().push(i);
    }
    let mut stage2: Vec<usize> = by_size.into_values().filter(|v| v.len() > 1).flatten().collect();
    stage2.sort_unstable();
    emit(
        Progress { stage: Stage::ComparingSizes, files_total: total, files_done: total, ..base.clone() },
        true,
        progress,
    );

    // 2. Partial fingerprints (small files get their full hash here).
    let roots: Vec<PathBuf> = spec.roots.iter().map(|r| r.canon.clone()).collect();
    let results: Vec<Mutex<HashSlot>> = (0..files.len()).map(|_| Mutex::new((None, None, false))).collect();
    let done = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let fp_bytes: u64 = stage2.iter().map(|&i| files[i].size.min(3 * PARTIAL_CHUNK)).sum();
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4);
    parallel(
        &stage2,
        workers,
        cancel,
        &done,
        || {
            emit(
                Progress {
                    stage: Stage::Fingerprinting,
                    files_total: stage2.len() as u64,
                    files_done: done.load(Ordering::Relaxed),
                    bytes_total: fp_bytes,
                    bytes_done: bytes.load(Ordering::Relaxed),
                    ..base.clone()
                },
                false,
                progress,
            )
        },
        |i| {
            let f = &files[i];
            let r = open_unchanged(&roots[f.root], f).ok().and_then(|mut file| {
                let fp = partial_fingerprint(&mut file, f.size).ok()?;
                bytes.fetch_add(f.size.min(3 * PARTIAL_CHUNK), Ordering::Relaxed);
                // Small files were read whole: confirm they didn't change meanwhile.
                if fp.1.is_some() {
                    let m = file.metadata().ok()?;
                    if m.len() != f.size || mtime_ns(&m) != f.mtime_ns {
                        return None;
                    }
                }
                Some(fp)
            });
            *results[i].lock().unwrap() = match r {
                Some((p, full)) => (Some(p), full, false),
                None => (None, None, true),
            };
            Ok(())
        },
    )?;
    for (i, r) in results.iter().enumerate() {
        let (p, full, bad) = *r.lock().unwrap();
        files[i].partial = p;
        files[i].full = full;
        if bad && stage2.binary_search(&i).is_ok() {
            stats.unreadable += 1;
        }
    }

    // 3. Full verification for files still colliding on (size, partial).
    let mut by_partial: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    for &i in &stage2 {
        if let Some(p) = files[i].partial {
            by_partial.entry((files[i].size, p)).or_default().push(i);
        }
    }
    let stage3: Vec<usize> =
        by_partial.into_values().filter(|v| v.len() > 1).flatten().filter(|&i| files[i].full.is_none()).collect();
    let full_bytes: u64 = stage3.iter().map(|&i| files[i].size).sum();
    let done = AtomicU64::new(0);
    let bytes = AtomicU64::new(0);
    let changed = AtomicU64::new(0);
    let fulls: Vec<Mutex<Option<[u8; 32]>>> = (0..files.len()).map(|_| Mutex::new(None)).collect();
    parallel(
        &stage3,
        workers.min(3),
        cancel,
        &done,
        || {
            emit(
                Progress {
                    stage: Stage::Verifying,
                    files_total: stage3.len() as u64,
                    files_done: done.load(Ordering::Relaxed),
                    bytes_total: full_bytes,
                    bytes_done: bytes.load(Ordering::Relaxed),
                    ..base.clone()
                },
                false,
                progress,
            )
        },
        |i| {
            let f = &files[i];
            let Ok(mut file) = open_unchanged(&roots[f.root], f) else {
                changed.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            };
            let h = full_hash(&mut file, f.size, cancel, &bytes)?;
            // Modified while hashing (size or mtime moved): not trustworthy.
            let still_same = file.metadata().is_ok_and(|m| m.len() == f.size && mtime_ns(&m) == f.mtime_ns);
            match h {
                Some(h) if still_same => *fulls[i].lock().unwrap() = Some(h),
                _ => {
                    changed.fetch_add(1, Ordering::Relaxed);
                }
            }
            Ok(())
        },
    )?;
    stats.changed += changed.load(Ordering::Relaxed);
    for &i in &stage3 {
        files[i].full = *fulls[i].lock().unwrap();
    }

    // Confirmed groups: identical size AND identical full BLAKE3.
    let mut by_full: HashMap<(u64, [u8; 32]), Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if let (Some(h), true) = (f.full, f.size > 0) {
            by_full.entry((f.size, h)).or_default().push(i);
        }
    }
    let file_groups: Vec<Vec<usize>> = by_full.into_values().filter(|v| v.len() > 1).collect();
    let groups = build_groups(&files, &spec.roots, &pairs, file_groups);

    let recoverable = groups.iter().map(Group::recoverable).sum();
    progress(&Progress {
        stage: Stage::Done,
        files_total: total,
        files_done: total,
        bytes_total: 0,
        bytes_done: 0,
        groups: groups.len() as u64,
        recoverable,
    });
    Ok(Analysis { roots: spec.roots, files, groups, stats })
}

/// Turn confirmed file groups into reviewable groups, handling Live Photos.
fn build_groups(
    files: &[FileRec],
    roots: &[Root],
    pairs: &[(usize, usize)],
    file_groups: Vec<Vec<usize>>,
) -> Vec<Group> {
    let in_pair: HashSet<usize> = pairs.iter().flat_map(|&(s, m)| [s, m]).collect();

    // Complete Live Photos whose still AND motion are both byte-identical.
    let mut live: HashMap<(Hash, Hash), Vec<(usize, usize)>> = HashMap::new();
    for &(s, m) in pairs {
        if let (Some(hs), Some(hm)) = (files[s].full, files[m].full) {
            live.entry((hs, hm)).or_default().push((s, m));
        }
    }
    let mut used: HashSet<usize> = HashSet::new();
    let mut groups = Vec::new();
    for members in live.into_values().filter(|v| v.len() > 1) {
        for &(s, m) in &members {
            used.insert(s);
            used.insert(m);
        }
        let unit = files[members[0].0].size + files[members[0].1].size;
        let members: Vec<Member> =
            members.into_iter().map(|(s, m)| Member { files: vec![s, m], locked: false }).collect();
        groups.push(Group { suggested: 0, members, live: true, unit_size: unit });
    }

    for g in file_groups {
        let members: Vec<Member> = g
            .into_iter()
            .filter(|i| !used.contains(i))
            // A component of a Live Photo that isn't duplicated as a whole
            // must stay, or that Live Photo would break.
            .map(|i| Member { files: vec![i], locked: in_pair.contains(&i) })
            .collect();
        if members.len() < 2 {
            continue;
        }
        let unit = files[members[0].files[0]].size;
        groups.push(Group { suggested: 0, members, live: false, unit_size: unit });
    }
    for g in &mut groups {
        g.suggested = suggest(files, roots, g);
    }
    // Biggest savings first.
    groups.sort_by(|a, b| {
        b.recoverable()
            .cmp(&a.recoverable())
            .then_with(|| files[a.members[0].files[0]].rel.cmp(&files[b.members[0].files[0]].rel))
    });
    groups
}

/// Which copy to keep: a conservative *suggestion* only. Prefers organised
/// media folders, avoids temporary/download/cache/backup-looking places and
/// "copy"-style names, then the oldest, then the shortest path.
pub fn suggest(files: &[FileRec], roots: &[Root], g: &Group) -> usize {
    if let Some(i) = g.members.iter().position(|m| m.locked) {
        return i;
    }
    let score = |m: &Member| {
        let f = &files[m.files[0]];
        let path = format!("{}/{}", roots[f.root].label, f.rel).to_lowercase();
        let segs: Vec<&str> = path.split('/').collect();
        let has = |words: &[&str]| segs.iter().any(|s| words.contains(s));
        let mut s = 0i32;
        if has(&["downloads", "download", "tmp", "temp", "cache", "caches", "trash", "recycle bin", "$recycle.bin"]) {
            s += 100;
        }
        if has(&["backup", "backups", "old", "copies", "duplicates"]) {
            s += 20;
        }
        let name = f.name.to_lowercase();
        if name.contains(" copy")
            || name.contains("copy of")
            || name.contains("-copy")
            || (1..10).any(|n| name.contains(&format!("({n})")))
        {
            s += 30;
        }
        if has(&["pictures", "photos", "videos", "movies", "dcim", "music", "documents"]) {
            s -= 10;
        }
        (s, f.created.unwrap_or(f.modified), f.rel.len(), f.rel.clone())
    };
    g.members.iter().enumerate().min_by_key(|(_, m)| score(m)).map_or(0, |(i, _)| i)
}

/// Plain-language reasons why `suggest` picked its copy (shown in the UI).
pub fn suggest_reasons(files: &[FileRec], roots: &[Root], g: &Group) -> Vec<String> {
    let k = g.suggested;
    let Some(keeper) = g.members.get(k) else { return Vec::new() };
    if keeper.locked {
        return vec!["Part of a Live Photo whose other half isn't duplicated, so it must stay".into()];
    }
    let segs = |m: &Member| {
        let f = &files[m.files[0]];
        format!("{}/{}", roots[f.root].label, f.rel).to_lowercase().split('/').map(str::to_owned).collect::<Vec<_>>()
    };
    let in_any = |m: &Member, words: &[&str]| segs(m).iter().any(|s| words.contains(&s.as_str()));
    let copy_name = |m: &Member| {
        let n = files[m.files[0]].name.to_lowercase();
        n.contains(" copy")
            || n.contains("copy of")
            || n.contains("-copy")
            || (1..10).any(|i| n.contains(&format!("({i})")))
    };
    let others: Vec<&Member> = g.members.iter().enumerate().filter(|(i, _)| *i != k).map(|(_, m)| m).collect();
    const TEMP: &[&str] =
        &["downloads", "download", "tmp", "temp", "cache", "caches", "trash", "recycle bin", "$recycle.bin"];
    const OLD: &[&str] = &["backup", "backups", "old", "copies", "duplicates"];
    const MEDIA: &[&str] = &["pictures", "photos", "videos", "movies", "dcim", "music", "documents"];
    let mut r = Vec::new();
    if !in_any(keeper, TEMP) && others.iter().any(|m| in_any(m, TEMP)) {
        r.push("Other copies are in Downloads, temporary or Trash folders".to_string());
    }
    if !in_any(keeper, OLD) && others.iter().any(|m| in_any(m, OLD)) {
        r.push("Other copies are in backup or “old” folders".into());
    }
    if !copy_name(keeper) && others.iter().any(|m| copy_name(m)) {
        r.push("Other copies have copy-style names (“copy”, “(1)”)".into());
    }
    if in_any(keeper, MEDIA) && others.iter().any(|m| !in_any(m, MEDIA)) {
        r.push("In an organised media folder".into());
    }
    let date = |m: &Member| files[m.files[0]].created.unwrap_or(files[m.files[0]].modified);
    if others.iter().all(|m| date(keeper) < date(m)) {
        r.push("Oldest copy".into());
    }
    if r.is_empty() {
        r.push("The copies are identical; the one with the shortest path is suggested".into());
    }
    r
}

// ----------------------------------------------------------------- cleanup

/// For one group: the member indexes to move to Trash; all others are kept.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct PlanItem {
    pub group: usize,
    pub trash: Vec<usize>,
}

/// The hard safety rules, enforced here regardless of what the UI sends.
pub fn validate(a: &Analysis, plan: &[PlanItem]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for item in plan {
        let g = a.groups.get(item.group).ok_or("The analysis changed; please review again.")?;
        if !seen.insert(item.group) {
            return Err("A group appears twice in the cleanup plan.".into());
        }
        let unique: HashSet<_> = item.trash.iter().collect();
        if unique.len() != item.trash.len() || item.trash.iter().any(|&m| m >= g.members.len()) {
            return Err("Invalid selection.".into());
        }
        // NEVER delete every copy.
        if item.trash.len() >= g.members.len() {
            return Err("At least one copy must be kept.".into());
        }
        if item.trash.iter().any(|&m| g.members[m].locked) {
            return Err("A copy that belongs to a Live Photo must be kept.".into());
        }
    }
    Ok(())
}

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub trashed_files: u64,
    pub trashed_bytes: u64,
    /// Files in kept copies of the groups that were cleaned.
    pub kept_files: u64,
    pub groups_cleaned: u64,
    pub groups_skipped: u64,
    pub failures: Vec<Failure>,
    pub cancelled: bool,
    #[serde(skip)]
    pub removed: Vec<usize>,
}

pub fn display_path(a: &Analysis, i: usize) -> String {
    let f = &a.files[i];
    format!("{}/{}", a.roots[f.root].label, f.rel)
}

/// Re-check a file right before acting: same size, mtime and fingerprint.
fn still_identical(a: &Analysis, i: usize) -> bool {
    let f = &a.files[i];
    let Ok(mut file) = open_unchanged(&a.roots[f.root].canon, f) else { return false };
    match (partial_fingerprint(&mut file, f.size), f.partial) {
        (Ok((p, _)), Some(expected)) => p == expected,
        _ => false,
    }
}

/// Execute a validated plan. `trash` performs the actual OS Trash operation.
pub fn execute(
    a: &Analysis,
    plan: &[PlanItem],
    trash: &mut dyn FnMut(&Path, &str) -> Result<(), String>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Outcome, String> {
    validate(a, plan)?;
    let mut out = Outcome::default();
    let total: u64 = plan
        .iter()
        .map(|p| p.trash.iter().map(|&m| a.groups[p.group].members[m].files.len() as u64).sum::<u64>())
        .sum();
    let mut done = 0u64;
    for item in plan {
        if item.trash.is_empty() {
            continue;
        }
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        let g = &a.groups[item.group];
        let kept: Vec<&Member> =
            g.members.iter().enumerate().filter(|(i, _)| !item.trash.contains(i)).map(|(_, m)| m).collect();
        // Revalidate every KEEP copy first (drive still connected, unchanged).
        if let Some(bad) = kept.iter().flat_map(|m| &m.files).find(|&&f| !still_identical(a, f)) {
            out.groups_skipped += 1;
            out.failures.push(Failure {
                path: display_path(a, *bad),
                reason: "The copy you chose to keep is unavailable or changed, so nothing in this group was moved."
                    .into(),
            });
            done += item.trash.iter().map(|&m| g.members[m].files.len() as u64).sum::<u64>();
            continue;
        }
        let mut any = false;
        for &m in &item.trash {
            let member = &g.members[m];
            if let Some(&bad) = member.files.iter().find(|&&f| !still_identical(a, f)) {
                out.failures.push(Failure {
                    path: display_path(a, bad),
                    reason: "Changed or unavailable since the analysis; left in place.".into(),
                });
                done += member.files.len() as u64;
                continue;
            }
            // Live Photos: still and motion move together.
            for (k, &f) in member.files.iter().enumerate() {
                let rec = &a.files[f];
                match trash(&a.roots[rec.root].canon, &rec.rel) {
                    Ok(()) => {
                        any = true;
                        out.trashed_files += 1;
                        out.trashed_bytes += rec.size;
                        out.removed.push(f);
                    }
                    Err(e) => {
                        // Both halves of a Live Photo move together; if the second fails, say so.
                        let reason = if member.files.len() > 1 && k > 0 {
                            format!("{e}. The Live Photo's still image was already moved to Trash; restore it from the Trash to keep the pair together.")
                        } else {
                            e
                        };
                        out.failures.push(Failure { path: display_path(a, f), reason });
                        break;
                    }
                }
                done += 1;
                progress(done, total);
            }
        }
        if any {
            out.groups_cleaned += 1;
            out.kept_files += kept.iter().map(|m| m.files.len() as u64).sum::<u64>();
        }
    }
    Ok(out)
}

/// Drop removed files from the in-memory results (groups left with fewer
/// than two copies disappear).
pub fn forget(a: &mut Analysis, removed: &HashSet<usize>) {
    for g in &mut a.groups {
        g.members.retain(|m| !m.files.iter().any(|f| removed.contains(f)));
    }
    a.groups.retain(|g| g.members.len() > 1);
    for g in &mut a.groups {
        g.suggested = g.suggested.min(g.members.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "mori-dupes-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&p).unwrap();
            Tmp(fs::canonicalize(p).unwrap())
        }
        fn put(&self, rel: &str, data: &[u8]) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, data).unwrap();
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn run(roots: &[&Path], kinds: Option<Vec<Kind>>) -> Analysis {
        let roots = roots
            .iter()
            .map(|p| Root {
                canon: p.to_path_buf(),
                label: p.file_name().unwrap().to_string_lossy().into(),
                ..Default::default()
            })
            .collect();
        analyze(Spec { roots, kinds, recursive: true }, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }

    fn names(a: &Analysis, g: &Group) -> Vec<String> {
        let mut v: Vec<String> =
            g.members.iter().flat_map(|m| m.files.iter().map(|&f| a.files[f].rel.clone())).collect();
        v.sort();
        v
    }

    /// Private folders (privacy.rs): an analysis started from a parent skips
    /// them entirely; one rooted at the private folder itself analyses it,
    /// with nested private folders still skipped.
    #[test]
    fn private_folders_are_skipped_unless_chosen_explicitly() {
        let t = Tmp::new("private");
        let photo = vec![3u8; 200_000];
        let other = vec![4u8; 150_000];
        t.put("Pictures/Family/a.jpg", &photo);
        t.put("Pictures/Private/a copy.jpg", &photo);
        t.put("Pictures/Private/b.jpg", &other);
        t.put("Pictures/Private/Inner/b copy.jpg", &other);
        t.put("Pictures/Private/Deeper/b again.jpg", &other);
        let root = |rel: &str, private: &[&str]| Root {
            canon: t.0.join(rel),
            label: rel.into(),
            private: private.iter().map(|s| s.to_string()).collect(),
        };
        let go = |r: Root| {
            analyze(Spec { roots: vec![r], kinds: None, recursive: true }, &AtomicBool::new(false), &mut |_| {})
                .unwrap()
        };
        // M/N) From the parent: nothing inside Private is even listed.
        let a = go(root("Pictures", &["Private", "Private/Deeper"]));
        assert!(a.groups.is_empty());
        assert!(
            a.files.iter().all(|f| !f.rel.starts_with("Private")),
            "{:?}",
            a.files.iter().map(|f| &f.rel).collect::<Vec<_>>()
        );
        assert_eq!(a.stats.scanned, 1);
        // O) Explicitly analysing the private folder works; the nested
        // private folder is still a boundary.
        let a = go(root("Pictures/Private", &["Deeper"]));
        assert_eq!(a.groups.len(), 1);
        assert_eq!(names(&a, &a.groups[0]), ["Inner/b copy.jpg", "b.jpg"]);
        assert!(a.files.iter().all(|f| !f.rel.starts_with("Deeper")));
    }

    #[test]
    fn content_not_names_decides() {
        let t = Tmp::new("names");
        let photo = vec![7u8; 300_000];
        t.put("Photos/IMG_4932.HEIC", &photo);
        t.put("Backup/photo-of-tora.HEIC", &photo);
        t.put("Other/IMG_4932.HEIC", &[8u8; 300_000]); // same name + size, different bytes
        let mut differs_at_end = photo.clone();
        *differs_at_end.last_mut().unwrap() = 1;
        t.put("Other/near.HEIC", &differs_at_end);
        let mut differs_in_middle = photo.clone();
        differs_in_middle[200_000] = 1; // outside all three fingerprint windows
        t.put("Other/sneaky.HEIC", &differs_in_middle);
        t.put("a/empty1.txt", b"");
        t.put("b/empty2.txt", b"");
        let a = run(&[&t.0], None);
        assert_eq!(a.groups.len(), 1, "{:?}", a.groups.iter().map(|g| names(&a, g)).collect::<Vec<_>>());
        assert_eq!(names(&a, &a.groups[0]), ["Backup/photo-of-tora.HEIC", "Photos/IMG_4932.HEIC"]);
        assert_eq!(a.stats.empty_ignored, 2);
        assert_eq!(a.groups[0].recoverable(), 300_000);
    }

    #[test]
    fn nested_and_cross_root_and_kind_filter() {
        let a1 = Tmp::new("r1");
        let a2 = Tmp::new("r2");
        let video = vec![3u8; 500_000];
        a1.put("deep/x/y/z/video.mov", &video);
        a2.put("birthday.mov", &video);
        a1.put("doc.pdf", b"%PDF same");
        a2.put("copy.pdf", b"%PDF same");
        let all = run(&[&a1.0, &a2.0], None);
        assert_eq!(all.groups.len(), 2);
        let only_video = run(&[&a1.0, &a2.0], Some(vec![Kind::Video]));
        assert_eq!(only_video.groups.len(), 1);
        let g = &only_video.groups[0];
        let roots: HashSet<usize> = g.members.iter().map(|m| only_video.files[m.files[0]].root).collect();
        assert_eq!(roots.len(), 2, "group spans both locations");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hardlinks_are_not_duplicates() {
        let t = Tmp::new("links");
        t.put("real.jpg", &[1u8; 1000]);
        std::os::unix::fs::symlink(t.0.join("real.jpg"), t.0.join("link.jpg")).unwrap();
        fs::hard_link(t.0.join("real.jpg"), t.0.join("hard.jpg")).unwrap();
        let outside = Tmp::new("outside");
        outside.put("secret.jpg", &[1u8; 1000]);
        std::os::unix::fs::symlink(&outside.0, t.0.join("escape")).unwrap();
        let a = run(&[&t.0], None);
        assert!(a.groups.is_empty(), "{:?}", a.groups.iter().map(|g| names(&a, g)).collect::<Vec<_>>());
        assert_eq!(a.stats.hardlinks_skipped, 1);
    }

    #[test]
    fn large_files_stream_with_bounded_memory() {
        let t = Tmp::new("large");
        // 300 MB sparse files: identical except the second one's last byte region.
        for (name, last) in [("a.bin", 0u8), ("b.bin", 0u8), ("c.bin", 9u8)] {
            let f = File::create(t.0.join(name)).unwrap();
            f.set_len(300 * 1024 * 1024).unwrap();
            if last != 0 {
                use std::io::Write;
                let mut f = f;
                f.seek(SeekFrom::Start(150 * 1024 * 1024 + 300_000)).unwrap(); // middle, outside windows
                f.write_all(&[last]).unwrap();
            }
        }
        let a = run(&[&t.0], None);
        assert_eq!(a.groups.len(), 1);
        assert_eq!(names(&a, &a.groups[0]), ["a.bin", "b.bin"]);
    }

    #[test]
    fn cancellation_stops() {
        let t = Tmp::new("cancel");
        for i in 0..50 {
            t.put(&format!("f{i}.bin"), &[0u8; 10]);
        }
        let cancel = AtomicBool::new(true);
        let r = analyze(
            Spec {
                roots: vec![Root { canon: t.0.clone(), label: "t".into(), ..Default::default() }],
                kinds: None,
                recursive: true,
            },
            &cancel,
            &mut |_| {},
        );
        assert!(r.is_err());
    }

    #[test]
    fn live_photos_are_kept_whole() {
        let t = Tmp::new("live");
        let still = vec![5u8; 400_000];
        let motion = vec![6u8; 900_000];
        // Two complete, identical Live Photos.
        t.put("A/IMG_1.HEIC", &still);
        t.put("A/IMG_1.MOV", &motion);
        t.put("B/IMG_1 copy.HEIC", &still);
        t.put("B/IMG_1 copy.MOV", &motion);
        // A third copy of the still whose motion differs: must be locked.
        t.put("C/IMG_9.HEIC", &still);
        t.put("C/IMG_9.MOV", &[7u8; 900_000]);
        // A standalone copy of the still (no motion) can be removed.
        t.put("D/standalone.heic", &still);
        let a = run(&[&t.0], Some(vec![Kind::Photo]));
        let live: Vec<_> = a.groups.iter().filter(|g| g.live).collect();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].members.len(), 2);
        assert!(live[0].members.iter().all(|m| m.files.len() == 2));
        let plain: Vec<_> = a.groups.iter().filter(|g| !g.live).collect();
        assert_eq!(plain.len(), 1);
        let locked: Vec<String> =
            plain[0].members.iter().filter(|m| m.locked).map(|m| a.files[m.files[0]].rel.clone()).collect();
        assert_eq!(locked, ["C/IMG_9.HEIC"]);
        // The suggested keep for a group with a locked member is that member.
        assert!(plain[0].members[plain[0].suggested].locked);
        // Backend refuses to trash a locked copy.
        let gi = a.groups.iter().position(|g| !g.live).unwrap();
        let li = plain[0].members.iter().position(|m| m.locked).unwrap();
        let other = 1 - li;
        assert!(validate(&a, &[PlanItem { group: gi, trash: vec![li] }]).is_err());
        assert!(validate(&a, &[PlanItem { group: gi, trash: vec![other] }]).is_ok());
    }

    #[test]
    fn suggestion_prefers_organised_originals() {
        let t = Tmp::new("suggest");
        let data = vec![9u8; 2000];
        t.put("Downloads/photo.jpg", &data);
        t.put("Pictures/Family/IMG_1450.jpg", &data);
        t.put("Backup/IMG_1450 copy.jpg", &data);
        let a = run(&[&t.0], None);
        let g = &a.groups[0];
        assert_eq!(a.files[g.members[g.suggested].files[0]].rel, "Pictures/Family/IMG_1450.jpg");
        let r = suggest_reasons(&a.files, &a.roots, g);
        assert!(r.iter().any(|x| x.contains("Downloads")), "{r:?}");
        assert!(r.iter().any(|x| x.contains("backup")));
        assert!(r.iter().any(|x| x.contains("copy-style")));
        assert!(r.iter().any(|x| x.contains("organised")));
    }

    #[test]
    fn never_trash_every_copy_and_revalidate_keepers() {
        let t = Tmp::new("plan");
        let data = vec![4u8; 5000];
        t.put("keep.jpg", &data);
        t.put("dup1.jpg", &data);
        t.put("dup2.jpg", &data);
        let a = run(&[&t.0], None);
        assert_eq!(a.groups.len(), 1);
        let all: Vec<usize> = (0..3).collect();
        // Every copy selected → refused by the backend.
        let err = validate(&a, &[PlanItem { group: 0, trash: all.clone() }]).unwrap_err();
        assert_eq!(err, "At least one copy must be kept.");
        let mut calls = Vec::new();
        let r = execute(
            &a,
            &[PlanItem { group: 0, trash: all }],
            &mut |_, rel| {
                calls.push(rel.to_owned());
                Ok(())
            },
            &AtomicBool::new(false),
            &mut |_, _| {},
        );
        assert!(r.is_err() && calls.is_empty(), "nothing touched");

        // Keep the suggested one, trash the others (fake trash: records calls).
        let keep = a.groups[0].suggested;
        let trash: Vec<usize> = (0..3).filter(|&m| m != keep).collect();
        // If the kept copy vanished (e.g. drive unplugged), nothing is trashed.
        let kept_rel = a.files[a.groups[0].members[keep].files[0]].rel.clone();
        let moved_away = t.0.join("elsewhere.tmp");
        fs::rename(t.0.join(&kept_rel), &moved_away).unwrap();
        let mut calls = Vec::new();
        let out = execute(
            &a,
            &[PlanItem { group: 0, trash: trash.clone() }],
            &mut |_, rel| {
                calls.push(rel.to_owned());
                Ok(())
            },
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert!(calls.is_empty());
        assert_eq!(out.groups_skipped, 1);
        fs::rename(&moved_away, t.0.join(&kept_rel)).unwrap();

        // A copy modified since the analysis is left in place; partial failure is reported.
        let victim = a.files[a.groups[0].members[trash[0]].files[0]].rel.clone();
        std::thread::sleep(Duration::from_millis(20));
        fs::write(t.0.join(&victim), vec![5u8; 5000]).unwrap();
        let mut calls = Vec::new();
        let out = execute(
            &a,
            &[PlanItem { group: 0, trash: trash.clone() }],
            &mut |_, rel| {
                calls.push(rel.to_owned());
                Err("simulated failure".into())
            },
            &AtomicBool::new(false),
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(calls.len(), 1, "only the unchanged duplicate was attempted");
        assert_eq!(out.trashed_files, 0, "a failed move is never reported as removed");
        assert_eq!(out.failures.len(), 2);
    }

    #[test]
    fn cleanup_cancellation() {
        let t = Tmp::new("ccancel");
        let data = vec![4u8; 5000];
        t.put("a.jpg", &data);
        t.put("b.jpg", &data);
        let a = run(&[&t.0], None);
        let cancel = AtomicBool::new(true);
        let mut calls = 0;
        let out = execute(
            &a,
            &[PlanItem { group: 0, trash: vec![1] }],
            &mut |_, _| {
                calls += 1;
                Ok(())
            },
            &cancel,
            &mut |_, _| {},
        )
        .unwrap();
        assert!(out.cancelled);
        assert_eq!(calls, 0);
    }

    /// Real files, real system Trash: only on request, against a scratch lab.
    /// `MORI_DUPLAB=/Volumes/Drive/DupLab cargo test -- --ignored --nocapture duplab`
    #[test]
    #[ignore]
    fn duplab_end_to_end() {
        let Some(lab) = std::env::var_os("MORI_DUPLAB").map(PathBuf::from) else { return };
        let canon = fs::canonicalize(&lab).unwrap();
        let spec = Spec {
            roots: vec![Root { canon: canon.clone(), label: "DupLab".into(), ..Default::default() }],
            kinds: None,
            recursive: true,
        };
        let t = std::time::Instant::now();
        let a = analyze(spec, &AtomicBool::new(false), &mut |_| {}).unwrap();
        println!("analyzed {} files in {:?}; stats {:?}", a.files.len(), t.elapsed(), a.stats);
        for g in a.groups.iter().filter(|g| !g.members[0].files.iter().any(|&f| a.files[f].rel.starts_with("Many/"))) {
            let names: Vec<String> = g
                .members
                .iter()
                .map(|m| {
                    format!(
                        "{}{}",
                        if m.locked { "[locked] " } else { "" },
                        m.files.iter().map(|&f| a.files[f].rel.as_str()).collect::<Vec<_>>().join(" + ")
                    )
                })
                .collect();
            println!("group live={} size={} suggested={} :: {:?}", g.live, g.unit_size, names[g.suggested], names);
        }
        // Clean up everything the suggestion would, through the real Trash.
        let plan: Vec<PlanItem> = a
            .groups
            .iter()
            .enumerate()
            .map(|(gi, g)| PlanItem {
                group: gi,
                trash: (0..g.members.len()).filter(|&i| i != g.suggested && !g.members[i].locked).collect(),
            })
            .filter(|p| !p.trash.is_empty())
            .collect();
        let store = crate::privacy::Store::load(std::env::temp_dir().join("mori-duplab-protected.json"));
        let policy = crate::policy::Policy { read_only: false, protected: &store };
        let mut trasher = |root: &Path, rel: &str| crate::fileops::move_to_trash(&policy, &root.join(rel));
        let out = execute(&a, &plan, &mut trasher, &AtomicBool::new(false), &mut |_, _| {}).unwrap();
        println!(
            "outcome: trashed {} ({} bytes), kept {}, cleaned {}, skipped {}, failures {:?}",
            out.trashed_files, out.trashed_bytes, out.kept_files, out.groups_cleaned, out.groups_skipped, out.failures
        );
        // Every group still has every kept copy on disk, byte for byte.
        for (gi, g) in a.groups.iter().enumerate() {
            let trashed: Vec<usize> = plan.iter().find(|p| p.group == gi).map(|p| p.trash.clone()).unwrap_or_default();
            for (mi, m) in g.members.iter().enumerate() {
                for &f in &m.files {
                    assert_eq!(canon.join(&a.files[f].rel).exists(), !trashed.contains(&mi), "{}", a.files[f].rel);
                }
            }
        }
    }
}
