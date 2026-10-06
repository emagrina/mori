//! Media Health: which photos, videos and audio files on the browsed drive
//! Mori can't show, and why. Each file gets at most one result:
//!
//! - **risk**: a high-attention finding (disguised executable, extension that
//!   contradicts the content, deceptive name). Not decoded further.
//! - **broken**: the data is damaged (truncated, decoder rejected it, video
//!   container inconsistent, no video track).
//! - **unsupported**: a real media file in a format Mori has no safe decoder
//!   or player for (RAW, TIFF, AVIF, AVI/MKV, unsupported codecs…).
//! - **failed**: the decoder timed out, crashed or hit a safety limit, or the
//!   video previously stopped the system media engine.
//!
//! Images are test-decoded by the sandboxed worker (already-cached
//! thumbnails count as decoded); videos go through the sandboxed container
//! probe; audio files only get the type and risk checks. Contents of private
//! folders are skipped. Results live in memory only.

use crate::index::Kind;
use crate::inspect;
use crate::risk::Level;
use crate::secure;
use crate::worker::{self, Op, WorkerError};
use crate::{thumbs, video, AppState};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Risk,
    Broken,
    Unsupported,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Finding {
    /// Browser index path (relative to the root).
    pub rel: String,
    pub category: Category,
    pub reason: String,
}

pub struct Store {
    pub root: PathBuf,
    pub findings: Vec<Finding>,
    pub checked: u64,
}

struct Job {
    rel: String,
    kind: Kind,
}

/// One file → at most one finding.
fn check(state: &AppState, root: &std::path::Path, job: &Job) -> Option<(Category, String)> {
    let (mut file, meta, canon) = match secure::open_inside(root, &job.rel) {
        Ok(v) => v,
        Err(_) => return None, // gone or no longer a regular file: not a media problem
    };
    let display = secure::display_safe(&job.rel);
    // Type and risk first (head + tail only, nothing decoded).
    let report = inspect::report(&canon, &display, &meta, Some(&mut file), inspect::Context::default());
    if let Some(f) = report.findings.iter().find(|f| f.level == Level::High || f.code == "extension-mismatch") {
        return Some((Category::Risk, f.title.clone()));
    }
    let unknown = report.detected.as_ref().is_none_or(|d| d.family == crate::filetype::Family::Unknown);
    if meta.len() == 0 {
        return Some((Category::Broken, "Empty file".into()));
    }
    if let Some(f) = report.findings.iter().find(|f| matches!(f.code, "truncated" | "broken-container")) {
        return Some((Category::Broken, f.title.clone()));
    }
    let ext = canon.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let _ = std::io::Seek::rewind(&mut file);
    let head = secure::read_head(&mut file, secure::SNIFF_LEN);
    let detected = secure::sniff(&head, &ext);
    match job.kind {
        Kind::Photo | Kind::Gif => {
            if !detected.is_image() {
                let what = report.detected.as_ref().map_or("This format", |d| d.label);
                return if unknown {
                    Some((Category::Broken, "Not recognisable as an image".into()))
                } else {
                    Some((Category::Unsupported, format!("{what}: no safe decoder in Mori")))
                };
            }
            // A thumbnail already made (or failed) for this exact version answers it.
            let stem = thumbs::stem(&state.thumb_dir, &canon, &meta, crate::protocol::THUMB_SIZE);
            if thumbs::cached(&stem).is_some() {
                return None;
            }
            let _ = std::io::Seek::rewind(&mut file);
            let bytes = match secure::read_limited(file, &meta, worker::MAX_INPUT) {
                Ok(b) => b,
                Err(_) => return Some((Category::Failed, "Larger than the safe decoding limit".into())),
            };
            state.decoded.fetch_add(1, Ordering::Relaxed);
            match worker::run(Op::Thumb, 64, bytes, Duration::from_secs(12)) {
                Ok(_) => None,
                Err(WorkerError::Unsupported) => {
                    Some((Category::Unsupported, "Image variant the decoder doesn't support".into()))
                }
                Err(WorkerError::Decode) => Some((Category::Broken, "The image data is damaged".into())),
                Err(WorkerError::Limits) => Some((Category::Failed, "Exceeds Mori's safe decoding limits".into())),
                Err(WorkerError::Timeout) => Some((Category::Failed, "Decoding took too long and was stopped".into())),
                Err(WorkerError::Failed) => Some((Category::Failed, "The sandboxed decoder failed".into())),
            }
        }
        Kind::Video => {
            if !detected.is_video() {
                return if unknown {
                    Some((Category::Broken, "Not recognisable as a video".into()))
                } else {
                    Some((
                        Category::Unsupported,
                        format!(
                            "{} isn't previewed by Mori",
                            report.detected.as_ref().map_or("This container", |d| d.label)
                        ),
                    ))
                };
            }
            drop(file);
            let info = state.video_info(&canon, &meta, detected);
            match info.status {
                video::VideoStatus::Playable => None,
                video::VideoStatus::UnsupportedCodec => Some((
                    Category::Unsupported,
                    format!("Codec {} isn't supported for secure preview", info.video_codec.unwrap_or_default()),
                )),
                video::VideoStatus::NoVideo => Some((Category::Broken, "No video track".into())),
                video::VideoStatus::Damaged => {
                    Some((Category::Broken, "The video container is damaged or incomplete".into()))
                }
                video::VideoStatus::Blocked => {
                    Some((Category::Failed, "It stopped the system video engine before".into()))
                }
            }
        }
        Kind::Audio => match &report.detected {
            _ if unknown => Some((Category::Broken, "Not recognisable as an audio file".into())),
            Some(d) if d.family != crate::filetype::Family::Audio && d.family != crate::filetype::Family::Video => {
                Some((Category::Unsupported, format!("{}: not an audio format", d.label)))
            }
            _ => None,
        },
        _ => None,
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    stage: &'static str,
    done: u64,
    total: u64,
    found: u64,
    paused: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Done {
    status: &'static str,
    message: Option<String>,
}

#[tauri::command]
pub fn health_start(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let root = state.root_canon().ok_or("No folder selected")?;
    if state.health_running.swap(true, Ordering::SeqCst) {
        return Err("A health check is already running.".into());
    }
    let idx = state.index();
    let jobs: Vec<Job> = idx
        .files
        .iter()
        .filter(|e| e.visible_from("") && matches!(e.kind, Kind::Photo | Kind::Gif | Kind::Video | Kind::Audio))
        .map(|e| Job { rel: e.path.clone(), kind: e.kind })
        .collect();
    drop(idx);
    *state.health.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.health_job.reset();
    let control = state.health_job.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let total = jobs.len() as u64;
        let next = AtomicUsize::new(0);
        let done = AtomicU64::new(0);
        let found: Mutex<Vec<Finding>> = Mutex::new(Vec::new());
        let finished = AtomicUsize::new(0);
        let threads = 3;
        let work = || -> Result<(), crate::dupes::Cancelled> {
            loop {
                control.checkpoint()?;
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some(job) = jobs.get(i) else { return Ok(()) };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&state, &root, job)))
                    .unwrap_or_else(|_| Some((Category::Failed, "Checking this file failed unexpectedly".into())));
                if let Some((category, reason)) = r {
                    found.lock().unwrap_or_else(PoisonError::into_inner).push(Finding {
                        rel: job.rel.clone(),
                        category,
                        reason,
                    });
                }
                done.fetch_add(1, Ordering::Relaxed);
            }
        };
        let result = std::thread::scope(|s| {
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let r = work();
                        finished.fetch_add(1, Ordering::SeqCst);
                        r
                    })
                })
                .collect();
            while finished.load(Ordering::SeqCst) < threads {
                let _ = app.emit(
                    "health-progress",
                    Progress {
                        stage: "checking",
                        done: done.load(Ordering::Relaxed),
                        total,
                        found: found.lock().unwrap_or_else(PoisonError::into_inner).len() as u64,
                        paused: control.is_paused(),
                    },
                );
                std::thread::sleep(Duration::from_millis(250));
            }
            hs.into_iter().map(|h| h.join().unwrap_or(Ok(()))).collect::<Result<Vec<()>, _>>()
        });
        let event = match result {
            Ok(_) => {
                let mut findings = found.into_inner().unwrap_or_else(PoisonError::into_inner);
                findings.sort_by(|a, b| a.category.cmp(&b.category).then_with(|| a.rel.cmp(&b.rel)));
                *state.health.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(Store { root, findings, checked: total });
                Done { status: "done", message: None }
            }
            Err(_) => Done { status: "cancelled", message: None },
        };
        state.health_running.store(false, Ordering::SeqCst);
        let _ = app.emit("health-done", event);
    });
    Ok(())
}

#[tauri::command]
pub fn health_pause(state: State<'_, AppState>, paused: bool) {
    state.health_job.set_paused(paused);
}

#[tauri::command]
pub fn health_cancel(state: State<'_, AppState>) {
    state.health_job.cancel();
}

#[tauri::command]
pub fn health_clear(state: State<'_, AppState>) {
    *state.health.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthItem {
    #[serde(flatten)]
    item: crate::index::Item,
    category: Category,
    reason: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthView {
    checked: u64,
    counts: BTreeMap<String, u64>,
    total: usize,
    items: Vec<HealthItem>,
}

fn key(c: Category) -> &'static str {
    match c {
        Category::Risk => "risk",
        Category::Broken => "broken",
        Category::Unsupported => "unsupported",
        Category::Failed => "failed",
    }
}

#[tauri::command]
pub fn health_results(
    state: State<'_, AppState>,
    category: Option<String>,
    offset: usize,
    limit: usize,
) -> Option<HealthView> {
    let guard = state.health.lock().unwrap_or_else(PoisonError::into_inner);
    let s = guard.as_ref()?;
    // Results belong to the drive they were made for.
    if state.root_canon().as_ref() != Some(&s.root) {
        return None;
    }
    let idx = state.index();
    let mut counts = BTreeMap::new();
    for f in &s.findings {
        *counts.entry(key(f.category).to_string()).or_insert(0) += 1;
    }
    let wanted = |f: &&Finding| category.as_deref().is_none_or(|c| key(f.category) == c);
    let total = s.findings.iter().filter(wanted).count();
    let items = s
        .findings
        .iter()
        .filter(wanted)
        .skip(offset)
        .take(limit.min(500))
        .filter_map(|f| {
            let e = idx.get(&crate::index::id_str(crate::index::id_for(&f.rel)))?;
            Some(HealthItem { item: e.into(), category: f.category, reason: f.reason.clone() })
        })
        .collect();
    Some(HealthView { checked: s.checked, counts, total, items })
}

/// Debug builds only: `MORI_DEBUG_HEALTH=<folder>` switches the browsed
/// folder (in memory only, settings untouched) to that folder, runs Media
/// Health once it is indexed and prints every finding.
pub fn debug_autorun(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_HEALTH").map(PathBuf::from) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        if let Err(e) = crate::open_root(&app, &dir) {
            eprintln!("mori: DEBUG health can't open {dir:?}: {e}");
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
        while state.scanning.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let t = std::time::Instant::now();
        if let Err(e) = health_start(app.clone(), app.state()) {
            eprintln!("mori: DEBUG health error {e}");
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
        while state.health_running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let g = state.health.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(s) = g.as_ref() else { return };
        eprintln!("mori: DEBUG health checked={} findings={} in {:?}", s.checked, s.findings.len(), t.elapsed());
        for f in &s.findings {
            eprintln!("mori: DEBUG health   {:?} {} — {}", f.category, f.rel, f.reason);
        }
        eprintln!("mori: DEBUG health done");
    });
}
