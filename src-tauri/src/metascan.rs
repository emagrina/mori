//! Metadata in the app: a file's embedded metadata (inspector), the
//! Sensitive Metadata scan (with the places shown on the offline map), and
//! sanitized copies.
//!
//! All parsing happens in the sandboxed worker (`metadata.rs`,
//! `sanitize.rs`). This module only reads bounded byte ranges from confined
//! files, hands them over, validates what comes back, and — for sanitized
//! copies — writes a new file through the mutation policy. Scan results live
//! in memory only.

use crate::dupes::{self, Cancelled, FileRec};
use crate::index::{self, Kind};
use crate::metadata::{Category, Field, Meta};
use crate::worker::{self, Op, OutFormat};
use crate::{fileops, jobs, sanitize, secure, thumbs, AppState};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

/// Bytes of a file handed to the parser during a scan / for the inspector.
const SCAN_HEAD: u64 = 4 * 1024 * 1024;
const DETAIL_HEAD: u64 = 32 * 1024 * 1024;
/// Largest image Mori makes a sanitized copy of.
const MAX_SANITIZE: u64 = 60 * 1024 * 1024;
const BATCH_FILES: usize = 32;
const BATCH_BYTES: usize = 24 * 1024 * 1024;
/// Sensitive fields kept per scan hit (the inspector shows everything).
const HIT_FIELDS: usize = 24;

const HEIF_BRANDS: &[&[u8; 4]] = &[b"heic", b"heix", b"mif1", b"msf1", b"avif", b"heim", b"heis", b"hevc"];

/// Worker input for an open, confined file: the `moov` box of MP4/QuickTime
/// files (wherever it sits), otherwise the first `limit` bytes.
fn input_for(file: &mut File, len: u64, limit: u64) -> Option<Vec<u8>> {
    let mut head = [0u8; 12];
    file.seek(SeekFrom::Start(0)).ok()?;
    let n = file.read(&mut head).ok()?;
    let mp4 = n >= 12
        && ((&head[4..8] == b"ftyp" && !HEIF_BRANDS.iter().any(|b| *b == &head[8..12]))
            || matches!(&head[4..8], b"moov" | b"mdat" | b"wide" | b"free" | b"skip"));
    if mp4 {
        if let Some((at, size)) = crate::video::find_moov(file, len).filter(|(_, s)| *s <= crate::video::MAX_MOOV) {
            let mut v = b"MP4\0".to_vec();
            v.reserve(size as usize);
            file.seek(SeekFrom::Start(at)).ok()?;
            file.by_ref().take(size).read_to_end(&mut v).ok()?;
            return Some(v);
        }
    }
    let mut v = b"FILE".to_vec();
    file.seek(SeekFrom::Start(0)).ok()?;
    file.by_ref().take(limit.min(len)).read_to_end(&mut v).ok()?;
    Some(v)
}

/// Re-check what the worker sent: printable, bounded text, valid coordinates.
fn checked(mut m: Meta) -> Meta {
    m.container = secure::display_safe(&m.container).chars().take(32).collect();
    m.fields.truncate(2000);
    for f in &mut m.fields {
        f.group = secure::display_safe(&f.group).chars().take(40).collect();
        f.name = secure::display_safe(&f.name).chars().take(120).collect();
        f.value = secure::display_safe(&f.value).chars().take(600).collect();
    }
    if m.gps.is_some_and(|[a, b]| !crate::metadata::valid_position(a, b)) {
        m.gps = None;
    }
    if m.orientation.is_some_and(|o| !(1..=8).contains(&o)) {
        m.orientation = None;
    }
    m
}

fn parse_meta(out: worker::Output) -> Option<Meta> {
    (out.format == OutFormat::Json).then(|| serde_json::from_slice::<Meta>(&out.bytes).ok()).flatten().map(checked)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    #[serde(flatten)]
    meta: Meta,
    categories: Vec<Category>,
    /// A sanitized copy can be made (JPEG, PNG, WebP up to 60 MB).
    sanitizable: bool,
}

/// Everything the worker could read from one file's metadata.
#[tauri::command]
pub async fn file_metadata(app: AppHandle, id: String) -> Result<FileMeta, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let loc = state.locate(&id)?;
        if loc.is_dir || loc.is_link {
            return Err("Not a file".to_string());
        }
        let (mut file, meta, _) = secure::open_inside(&loc.root, &loc.rel).map_err(|_| "File unavailable")?;
        let input = input_for(&mut file, meta.len(), DETAIL_HEAD).ok_or("The file couldn't be read.")?;
        let sanitizable = meta.len() <= MAX_SANITIZE && sanitize::supported(input.get(4..).unwrap_or(&[]));
        let out = worker::run(Op::Meta, 64, input, Duration::from_secs(15))
            .map_err(|_| "The metadata couldn't be read safely.")?;
        let m = parse_meta(out).ok_or("The metadata couldn't be read safely.")?;
        Ok(FileMeta { categories: m.categories(), meta: m, sanitizable })
    })
    .await
    .map_err(|_| "The metadata couldn't be read.".to_string())?
}

// ------------------------------------------------------------------- scan

pub struct Hit {
    file: usize,
    categories: Vec<Category>,
    gps: Option<[f64; 2]>,
    fields: Vec<Field>,
}

#[derive(Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStats {
    scanned: u64,
    with_sensitive: u64,
    failed: u64,
    unreadable: u64,
    places: u64,
}

pub struct Store {
    roots: Vec<dupes::Root>,
    files: Vec<FileRec>,
    hits: Vec<Hit>,
    /// "m" + 16 hex → index into `hits`.
    ids: HashMap<String, usize>,
    stats: ScanStats,
}

pub fn scan_id(root: &Path, rel: &str) -> String {
    format!("m{:016x}", thumbs::fnv(format!("{}\u{0}{rel}", root.to_string_lossy()).as_bytes()))
}

impl Store {
    /// (root, rel, ext, name) of a scan result id.
    pub fn locate(&self, id: &str) -> Option<(PathBuf, String, String, String)> {
        let h = &self.hits[*self.ids.get(id)?];
        let f = &self.files[h.file];
        Some((self.roots[f.root].canon.clone(), f.rel.clone(), f.ext.clone(), f.name.clone()))
    }
    pub fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.roots.iter().map(|r| &r.canon)
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    stage: &'static str,
    done: u64,
    total: u64,
    found: u64,
    failed: u64,
    paused: bool,
}

/// Parse a batch in one worker; if the worker fails, each file gets its own
/// worker so one hostile file costs only its own result.
fn run_batch(inputs: Vec<Vec<u8>>) -> Vec<Option<Meta>> {
    let mut packed = Vec::with_capacity(inputs.iter().map(|i| i.len() + 4).sum());
    for i in &inputs {
        packed.extend((i.len() as u32).to_le_bytes());
        packed.extend(i);
    }
    if let Ok(out) = worker::run(Op::MetaBatch, 64, packed, Duration::from_secs(30)) {
        if out.format == OutFormat::Json {
            if let Ok(v) = serde_json::from_slice::<Vec<Option<Meta>>>(&out.bytes) {
                if v.len() == inputs.len() {
                    return v.into_iter().map(|m| m.map(checked)).collect();
                }
            }
        }
    }
    inputs
        .into_iter()
        .map(|i| worker::run(Op::Meta, 64, i, Duration::from_secs(10)).ok().and_then(parse_meta))
        .collect()
}

fn scan(
    roots: Vec<dupes::Root>,
    recursive: bool,
    job: &jobs::Control,
    emit: &(dyn Fn(ScanProgress) + Sync),
) -> Result<Store, Cancelled> {
    let spec = dupes::Spec { roots, kinds: Some(vec![Kind::Photo, Kind::Gif, Kind::Video, Kind::Audio]), recursive };
    let mut dstats = dupes::Stats::default();
    let files = dupes::collect(&spec, job.cancel_flag(), &mut dstats, &mut |n| {
        emit(ScanProgress { stage: "collecting", done: 0, total: n, found: 0, failed: 0, paused: job.is_paused() })
    })?;
    let total = files.len() as u64;
    let next = AtomicUsize::new(0);
    let done = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let hits: Mutex<Vec<Hit>> = Mutex::new(Vec::new());
    let finished = AtomicUsize::new(0);
    let threads = 3;
    let roots = &spec.roots;
    let work = || -> Result<(), Cancelled> {
        loop {
            job.checkpoint()?;
            let start = next.fetch_add(BATCH_FILES, Ordering::SeqCst);
            if start >= files.len() {
                return Ok(());
            }
            let end = (start + BATCH_FILES).min(files.len());
            let mut idx = Vec::new();
            let mut inputs = Vec::new();
            let mut bytes = 0;
            for (i, f) in files.iter().enumerate().take(end).skip(start) {
                // Confined, no symlinks, unchanged since the walk.
                let Ok(mut file) = dupes::open_unchanged(&roots[f.root].canon, f) else {
                    failed.fetch_add(1, Ordering::Relaxed);
                    done.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                match input_for(&mut file, f.size, SCAN_HEAD) {
                    Some(v) if bytes + v.len() <= BATCH_BYTES || inputs.is_empty() => {
                        bytes += v.len();
                        idx.push(i);
                        inputs.push(v);
                    }
                    Some(v) => {
                        // Too big to share a worker: alone.
                        let r = run_batch(vec![v]);
                        record(i, r.into_iter().next().flatten(), &hits, &failed);
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            if !inputs.is_empty() {
                job.checkpoint()?;
                for (i, m) in idx.iter().zip(run_batch(inputs)) {
                    record(*i, m, &hits, &failed);
                    done.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    };
    let result = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&work)).unwrap_or(Ok(()));
                    finished.fetch_add(1, Ordering::SeqCst);
                    r
                })
            })
            .collect();
        while finished.load(Ordering::SeqCst) < threads {
            emit(ScanProgress {
                stage: "reading",
                done: done.load(Ordering::Relaxed),
                total,
                found: hits.lock().unwrap_or_else(PoisonError::into_inner).len() as u64,
                failed: failed.load(Ordering::Relaxed),
                paused: job.is_paused(),
            });
            std::thread::sleep(Duration::from_millis(250));
        }
        handles.into_iter().map(|h| h.join().unwrap_or(Ok(()))).collect::<Result<Vec<()>, Cancelled>>()
    });
    result?;
    let mut hits = hits.into_inner().unwrap_or_else(PoisonError::into_inner);
    hits.sort_by_key(|h| h.file);
    let ids = hits
        .iter()
        .enumerate()
        .map(|(i, h)| (scan_id(&spec.roots[files[h.file].root].canon, &files[h.file].rel), i))
        .collect();
    let stats = ScanStats {
        scanned: total,
        with_sensitive: hits.len() as u64,
        failed: failed.load(Ordering::Relaxed),
        unreadable: dstats.unreadable,
        places: hits.iter().filter(|h| h.gps.is_some()).count() as u64,
    };
    Ok(Store { roots: spec.roots, files, hits, ids, stats })
}

fn record(file: usize, m: Option<Meta>, hits: &Mutex<Vec<Hit>>, failed: &AtomicU64) {
    let Some(m) = m else {
        failed.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let categories = m.categories();
    if categories.is_empty() {
        return;
    }
    let fields = m.fields.into_iter().filter(|f| f.sensitive.is_some()).take(HIT_FIELDS).collect();
    hits.lock().unwrap_or_else(PoisonError::into_inner).push(Hit { file, categories, gps: m.gps, fields });
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ScanDone {
    status: &'static str,
    message: Option<String>,
}

#[tauri::command]
pub fn meta_start(
    app: AppHandle,
    state: State<'_, AppState>,
    locations: Vec<String>,
    recursive: bool,
) -> Result<(), String> {
    if state.meta_running.swap(true, Ordering::SeqCst) {
        return Err("A metadata scan is already running.".into());
    }
    let roots = crate::analysis_roots(&app, &state, &locations);
    if roots.is_empty() {
        state.meta_running.store(false, Ordering::SeqCst);
        return Err("Choose at least one available location.".into());
    }
    *state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.meta_job.reset();
    let job = state.meta_job.clone();
    std::thread::spawn(move || {
        let emit = |p: ScanProgress| {
            let _ = app.emit("meta-progress", p);
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scan(roots, recursive, &job, &emit)));
        let state = app.state::<AppState>();
        let event = match res {
            Ok(Ok(store)) => {
                *state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
                ScanDone { status: "done", message: None }
            }
            Ok(Err(Cancelled)) => ScanDone { status: "cancelled", message: None },
            Err(_) => ScanDone { status: "failed", message: Some("The scan stopped unexpectedly.".into()) },
        };
        state.meta_running.store(false, Ordering::SeqCst);
        let _ = app.emit("meta-done", event);
    });
    Ok(())
}

#[tauri::command]
pub fn meta_pause(state: State<'_, AppState>, paused: bool) {
    state.meta_job.set_paused(paused);
}

#[tauri::command]
pub fn meta_cancel(state: State<'_, AppState>) {
    state.meta_job.cancel();
}

#[tauri::command]
pub fn meta_clear(state: State<'_, AppState>) {
    *state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HitView {
    id: String,
    name: String,
    path: String,
    location: String,
    drive: String,
    size: u64,
    modified: i64,
    created: Option<i64>,
    kind: Kind,
    ext: String,
    categories: Vec<Category>,
    gps: Option<[f64; 2]>,
    fields: Vec<Field>,
    sanitizable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetaView {
    locations: Vec<crate::LocationInfo>,
    stats: ScanStats,
    counts: BTreeMap<String, u64>,
    /// Matching the filter (before paging).
    total: usize,
    hits: Vec<HitView>,
}

fn category_key(c: Category) -> &'static str {
    match c {
        Category::Location => "location",
        Category::Person => "person",
        Category::Device => "device",
        Category::Software => "software",
        Category::Comment => "comment",
        Category::Identifier => "identifier",
    }
}

fn hit_view(s: &Store, h: &Hit, drives: &[String]) -> HitView {
    let f = &s.files[h.file];
    let root = &s.roots[f.root];
    HitView {
        id: scan_id(&root.canon, &f.rel),
        name: secure::display_safe(&f.name),
        path: secure::display_safe(&f.rel),
        location: root.label.clone(),
        drive: drives[f.root].clone(),
        size: f.size,
        modified: f.modified,
        created: f.created,
        kind: f.kind,
        ext: secure::display_safe(&f.ext),
        categories: h.categories.clone(),
        gps: h.gps,
        fields: h.fields.clone(),
        sanitizable: matches!(f.ext.as_str(), "jpg" | "jpeg" | "png" | "webp") && f.size <= MAX_SANITIZE,
    }
}

#[tauri::command]
pub fn meta_results(
    app: AppHandle,
    state: State<'_, AppState>,
    category: Option<String>,
    offset: usize,
    limit: usize,
) -> Option<MetaView> {
    let guard = state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner);
    let s = guard.as_ref()?;
    let drives: Vec<String> = s.roots.iter().map(|r| crate::drive_of(&r.canon)).collect();
    let mut counts = BTreeMap::new();
    for h in &s.hits {
        for c in &h.categories {
            *counts.entry(category_key(*c).to_string()).or_insert(0) += 1;
        }
    }
    let wanted = |h: &&Hit| category.as_deref().is_none_or(|c| h.categories.iter().any(|x| category_key(*x) == c));
    let total = s.hits.iter().filter(wanted).count();
    let hits =
        s.hits.iter().filter(wanted).skip(offset).take(limit.min(500)).map(|h| hit_view(s, h, &drives)).collect();
    let locations = s
        .roots
        .iter()
        .map(|r| crate::LocationInfo {
            key: String::new(),
            label: r.label.clone(),
            path: crate::pretty_path(&app, &r.canon),
            drive: crate::drive_of(&r.canon),
        })
        .collect();
    Some(MetaView { locations, stats: s.stats.clone(), counts, total, hits })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Places {
    ids: Vec<String>,
    names: Vec<String>,
    /// Flat [lat, lon, lat, lon, …].
    coords: Vec<f64>,
}

/// Every scanned file with a position, for the offline map.
#[tauri::command]
pub fn meta_places(state: State<'_, AppState>) -> Option<Places> {
    let guard = state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner);
    let s = guard.as_ref()?;
    let mut p = Places { ids: Vec::new(), names: Vec::new(), coords: Vec::new() };
    for h in s.hits.iter().filter(|h| h.gps.is_some()).take(500_000) {
        let f = &s.files[h.file];
        let [lat, lon] = h.gps.unwrap();
        p.ids.push(scan_id(&s.roots[f.root].canon, &f.rel));
        p.names.push(secure::display_safe(&f.name));
        p.coords.extend([(lat * 1e5).round() / 1e5, (lon * 1e5).round() / 1e5]);
    }
    Some(p)
}

// ---------------------------------------------------------- capture times

/// "2024:03:01 10:00:01" (EXIF) or "2024-03-01 10:00:01" → ms on a naive
/// clock (only differences between shots of one camera are used).
pub fn parse_exif_time(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| std::str::from_utf8(&b[r]).ok()?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil (proleptic Gregorian).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + mi) * 60_000 + se * 1000)
}

/// Capture time (with sub-seconds) and camera of a parsed file.
pub fn capture_of(m: &Meta) -> Option<(i64, String)> {
    let get = |n: &str| m.fields.iter().find(|f| f.group == "EXIF" && f.name == n).map(|f| f.value.as_str());
    let base = parse_exif_time(get("DateTimeOriginal").or(get("DateTime"))?)?;
    let sub = get("SubSecTimeOriginal").or(get("SubSecTime")).and_then(|s| {
        let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit()).take(3).collect();
        let n: i64 = digits.parse().ok()?;
        Some(n * 10i64.pow(3 - digits.len() as u32))
    });
    let camera = format!("{} {}", get("Make").unwrap_or(""), get("Model").unwrap_or("")).trim().to_owned();
    Some((base + sub.unwrap_or(0), camera))
}

/// Capture times of some files (e.g. the photos of Similar Media groups),
/// read by the worker. `files`: (id, root, record).
pub fn capture_times(files: &[(usize, PathBuf, FileRec)]) -> HashMap<usize, (i64, String)> {
    let mut out = HashMap::new();
    for chunk in files.chunks(BATCH_FILES) {
        let mut ids = Vec::new();
        let mut inputs = Vec::new();
        for (id, root, f) in chunk {
            let Ok(mut file) = dupes::open_unchanged(root, f) else { continue };
            if let Some(v) = input_for(&mut file, f.size, 1024 * 1024) {
                ids.push(*id);
                inputs.push(v);
            }
        }
        if inputs.is_empty() {
            continue;
        }
        for (id, m) in ids.into_iter().zip(run_batch(inputs)) {
            if let Some(t) = m.as_ref().and_then(capture_of) {
                out.insert(id, t);
            }
        }
    }
    out
}

// ------------------------------------------------------- sanitized copies

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizeOutcome {
    id: String,
    name: String,
    new_name: Option<String>,
    error: Option<String>,
}

/// `photo.jpg` → `photo-sanitized.jpg`, then `photo-sanitized-2.jpg`, …
fn copy_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..100).map(move |n| if n == 1 { format!("{stem}-sanitized{ext}") } else { format!("{stem}-sanitized-{n}{ext}") })
}

fn sanitize_one(app: &AppHandle, state: &AppState, id: &str) -> Result<String, String> {
    let loc = state.locate(id)?;
    if loc.is_dir || loc.is_link {
        return Err("Not a file".into());
    }
    let (file, meta, canon) = secure::open_inside(&loc.root, &loc.rel).map_err(|_| "file unavailable")?;
    if meta.len() > MAX_SANITIZE {
        return Err("too large for a sanitized copy (60 MB limit)".into());
    }
    let original = secure::read_limited(file, &meta, MAX_SANITIZE).map_err(|_| "the file couldn't be read")?;
    if !sanitize::supported(&original) {
        return Err("sanitized copies are available for JPEG, PNG and WebP images".into());
    }
    let out = worker::run(Op::Sanitize, 64, original.clone(), Duration::from_secs(30))
        .ok()
        .filter(|o| o.format == OutFormat::Bytes && sanitize::supported(&o.bytes))
        .ok_or("the image couldn't be processed safely")?
        .bytes;
    // Verify before writing anything: the copy decodes to the same dimensions…
    let dims = |b: Vec<u8>| worker::run(Op::Thumb, 64, b, Duration::from_secs(25)).ok().map(|o| (o.width, o.height));
    let (a, b) = (dims(original), dims(out.clone()));
    if a.is_none() || a != b {
        return Err("the copy didn't verify (image changed), so nothing was written".into());
    }
    // …and no sensitive field or position is left.
    let mut probe = b"FILE".to_vec();
    probe.extend(&out);
    let left = worker::run(Op::Meta, 64, probe, Duration::from_secs(15))
        .ok()
        .and_then(parse_meta)
        .ok_or("the copy couldn't be verified")?;
    if !left.categories().is_empty() || left.gps.is_some() {
        return Err("the copy still contained metadata, so nothing was written".into());
    }
    let dir = canon.parent().ok_or("invalid folder")?;
    let policy = state.policy();
    let mut created = None;
    for name in copy_names(&loc.name) {
        if fileops::create_new(&policy, dir, &name, &out)? {
            created = Some(name);
            break;
        }
    }
    let name = created.ok_or("no free name for the copy")?;
    state.history.record(
        format!("Created sanitized copy “{}”", secure::display_safe(&name)),
        vec![crate::history::Change::Created { path: dir.join(&name) }],
    );
    // Read back what was written.
    let rel = match loc.rel.rsplit_once('/') {
        Some((d, _)) => format!("{d}/{name}"),
        None => name.clone(),
    };
    let (written, wmeta, _) = secure::open_inside(&loc.root, &rel).map_err(|_| "the copy couldn't be read back")?;
    let back = secure::read_limited(written, &wmeta, MAX_SANITIZE).map_err(|_| "the copy couldn't be read back")?;
    if blake3::hash(&back) != blake3::hash(&out) {
        return Err(format!("“{name}” was written but didn't read back identically — check it before relying on it"));
    }
    // Show it in the browser right away when it's inside the browsed folder.
    if let Some(rel) = crate::browser_rel(state, &dir.join(&name)) {
        let entry = index::make_entry(rel.clone(), name.clone(), false, &wmeta);
        crate::apply_index_change(app, |idx| idx.add_file(entry));
    }
    Ok(name)
}

/// Create sanitized copies next to the originals. Originals are never modified.
#[tauri::command]
pub async fn sanitize_copies(app: AppHandle, ids: Vec<String>) -> Vec<SanitizeOutcome> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        ids.into_iter()
            .take(1000)
            .map(|id| {
                let name = state.locate(&id).map(|l| secure::display_safe(&l.name)).unwrap_or_default();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sanitize_one(&app, &state, &id))) {
                    Ok(Ok(n)) => SanitizeOutcome { id, name, new_name: Some(secure::display_safe(&n)), error: None },
                    Ok(Err(e)) => SanitizeOutcome { id, name, new_name: None, error: Some(e) },
                    Err(_) => SanitizeOutcome { id, name, new_name: None, error: Some("unexpected error".into()) },
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Debug builds only: `MORI_DEBUG_META=<folder>` scans that folder at launch
/// through the real worker and prints the results (private subfolders from
/// `MORI_DEBUG_META_PRIVATE=a,b`). With `MORI_DEBUG_META_SANITIZE=1` it also
/// makes sanitized copies of every eligible hit and rescans to check them.
pub fn debug_autorun(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_META").map(PathBuf::from) else { return };
    let Ok(canon) = std::fs::canonicalize(dir) else { return };
    let private: std::collections::HashSet<String> = std::env::var("MORI_DEBUG_META_PRIVATE")
        .unwrap_or_default()
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let state = app.state::<AppState>();
        let run = |label: &str| {
            let root = dupes::Root { canon: canon.clone(), label: "debug".into(), private: private.clone() };
            let t = std::time::Instant::now();
            let store = scan(vec![root], true, &jobs::Control::default(), &|_| {}).expect("scan");
            eprintln!(
                "mori: DEBUG meta {label}: {:?} in {:?}",
                serde_json::to_string(&store.stats).unwrap(),
                t.elapsed()
            );
            for h in &store.hits {
                let f = &store.files[h.file];
                eprintln!("mori: DEBUG meta   {} {:?} gps={:?} fields={}", f.rel, h.categories, h.gps, h.fields.len());
            }
            store
        };
        let store = run("scan");
        let ids: Vec<String> = store
            .hits
            .iter()
            .filter(|h| matches!(store.files[h.file].ext.as_str(), "jpg" | "jpeg" | "png" | "webp"))
            .map(|h| scan_id(&canon, &store.files[h.file].rel))
            .collect();
        *state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner) = Some(store);
        if std::env::var("MORI_DEBUG_META_SANITIZE").is_ok() {
            for id in ids {
                let r = sanitize_one(&app, &state, &id);
                eprintln!("mori: DEBUG sanitize {id}: {r:?}");
            }
            run("rescan");
        }
        eprintln!("mori: DEBUG meta done");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_times_from_exif() {
        assert_eq!(
            parse_exif_time("2024:03:01 10:00:01").unwrap() - parse_exif_time("2024:03:01 10:00:00").unwrap(),
            1000
        );
        assert_eq!(
            parse_exif_time("2024-03-02 00:00:00").unwrap() - parse_exif_time("2024:03:01 23:59:59").unwrap(),
            1000
        );
        assert!(parse_exif_time("0000:00:00 00:00:00").is_none() && parse_exif_time("garbage").is_none());
        let f = |n: &str, v: &str| Field { group: "EXIF".into(), name: n.into(), value: v.into(), sensitive: None };
        let m = Meta {
            fields: vec![
                f("DateTimeOriginal", "2024:03:01 10:00:01"),
                f("SubSecTimeOriginal", "25"),
                f("Make", "Apple"),
                f("Model", "iPhone 15"),
            ],
            ..Default::default()
        };
        let (t, cam) = capture_of(&m).unwrap();
        assert_eq!(t - parse_exif_time("2024:03:01 10:00:01").unwrap(), 250);
        assert_eq!(cam, "Apple iPhone 15");
    }

    #[test]
    fn copy_names_never_lose_the_extension() {
        let n: Vec<String> = copy_names("beach.jpg").take(3).collect();
        assert_eq!(n, ["beach-sanitized.jpg", "beach-sanitized-2.jpg", "beach-sanitized-3.jpg"]);
        assert_eq!(copy_names("noext").next().unwrap(), "noext-sanitized");
        assert_eq!(copy_names(".hidden.png").next().unwrap(), ".hidden-sanitized.png");
    }

    #[test]
    fn worker_output_is_rechecked() {
        let m = Meta {
            container: "JPEG\u{202E}".into(),
            fields: vec![Field {
                group: "EXIF".into(),
                name: "Make\u{0007}".into(),
                value: "x\u{202E}y".into(),
                sensitive: Some(Category::Device),
            }],
            gps: Some([200.0, 0.0]),
            orientation: Some(42),
            partial: false,
        };
        let c = checked(m);
        assert!(
            !c.container.contains('\u{202E}')
                && !c.fields[0].value.contains('\u{202E}')
                && !c.fields[0].name.contains('\u{7}')
        );
        assert!(c.gps.is_none() && c.orientation.is_none());
    }
}
