// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod archive;
mod capture;
mod cleanup;
mod diagnostics;
mod drives;
mod dupes;
mod fileops;
mod filetype;
mod health;
#[cfg(target_os = "macos")]
mod heif;
mod history;
mod index;
mod inspect;
mod integrity;
mod jobs;
mod localdata;
mod metadata;
mod metascan;
mod overwrite;
#[cfg(target_os = "macos")]
mod pdf;
mod policy;
mod privacy;
mod probe;
mod protocol;
mod risk;
mod sanitize;
mod secure;
mod similar;
mod storage;
mod tags;
mod thumbs;
mod video;
mod worker;

use index::{Index, Item, Query};
use protocol::PreviewCache;
use secure::Detected;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::PoisonError;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

/// Debug-only diagnostics. Release builds log nothing (and never paths).
macro_rules! debug_log {
    ($($t:tt)*) => { if cfg!(debug_assertions) { eprintln!($($t)*) } };
}

struct RootInfo {
    /// Canonical root; every file access is confined beneath it.
    canon: PathBuf,
    name: String,
}

pub struct AppState {
    /// Mori's own directories (see `localdata.rs`).
    dirs: localdata::Dirs,
    /// Stale temporary files removed at startup.
    stale_cleaned: u64,
    data_dir: PathBuf,
    pub thumb_dir: PathBuf,
    /// In-memory thumbnails for analyzer files outside the browsed folder.
    pub volatile_thumbs: thumbs::Volatile,
    root: RwLock<Option<RootInfo>>,
    index: RwLock<Arc<Index>>,
    scan_gen: AtomicU64,
    scanning: AtomicBool,
    scan_count: AtomicUsize,
    settings_lock: Mutex<()>,
    pub previews: Mutex<PreviewCache>,
    pub video: video::VideoGuard,
    /// Results of the last duplicate analysis (memory only, never written to disk).
    analysis: Mutex<Option<AnalysisStore>>,
    analysis_running: AtomicBool,
    analysis_cancel: Arc<AtomicBool>,
    cleanup_cancel: Arc<AtomicBool>,
    /// Folders the user picked for analysis in this session (explicit consent).
    custom_locations: Mutex<Vec<PathBuf>>,
    /// Similar-media analysis (memory only; fingerprints are cached separately).
    similar: Mutex<Option<SimilarStore>>,
    similar_running: AtomicBool,
    similar_cancel: Arc<AtomicBool>,
    /// The one pending video-frame request to the webview: (token, reply).
    similar_capture: Mutex<Option<SimilarReply>>,
    similar_token: AtomicU64,
    /// Private folders (visibility boundaries), persisted in app data.
    privacy: privacy::Store,
    /// Protected folders ("Never Modify"), persisted in app data.
    protected: privacy::Store,
    /// Read-only Mode: every filesystem mutation is refused (policy.rs).
    read_only: AtomicBool,
    /// Drives Mori knows about (Safe Inspection Mode settings).
    drives: drives::Store,
    /// The browsed root is in Safe Inspection Mode: no automatic decoding.
    pub safe_mode: AtomicBool,
    /// Media decoded since the current root was opened.
    pub decoded: AtomicU64,
    /// Drives connected while Mori runs, announced to the UI: key → (mount, label).
    connected: Mutex<HashMap<String, (PathBuf, String)>>,
    /// Favorites (files and folders), per volume like private folders.
    favorites: privacy::Store,
    /// Local tags (names in Mori's app data, never in the files).
    tags: tags::Store,
    /// Temporary session: nothing about the browsed folder is written to disk.
    pub temp: AtomicBool,
    /// Private Inspection: a temporary session that also forces read-only
    /// access and Safe Inspection Mode for as long as it lasts.
    private_inspection: AtomicBool,
    session_read_only: AtomicBool,
    session_safe: AtomicBool,
    /// Screenshot corrections: never one / always one (per volume, like private folders).
    capture_not: privacy::Store,
    capture_yes: privacy::Store,
    /// SHA-256 results of this session (memory only, never written).
    checksums: integrity::Cache,
    checksum_cancel: AtomicBool,
    /// Integrity snapshots (opt-in local state in app data).
    integrity: integrity::Store,
    integrity_job: Arc<jobs::Control>,
    integrity_running: AtomicBool,
    /// Undo history of file operations (this session, memory only).
    history: history::History,
    /// Move/Copy destinations picked this session outside the browsed folder.
    transfer_dests: Mutex<Vec<PathBuf>>,
    transfer_cancel: AtomicBool,
    /// Unfinished Quick Cleanup sessions (opaque ids only; never in a temporary session).
    cleanup: cleanup::Store,
    /// Media Health results (memory only).
    health: Mutex<Option<health::Store>>,
    health_running: AtomicBool,
    health_job: Arc<jobs::Control>,
    /// Sensitive Metadata scan results (memory only).
    meta_scan: Mutex<Option<metascan::Store>>,
    meta_running: AtomicBool,
    meta_job: Arc<jobs::Control>,
}

type SimilarReply = (u64, std::sync::mpsc::Sender<Result<similar::Capture, similar::CaptureError>>);

struct SimilarStore {
    /// Registered as soon as the file list is known, so the webview can load
    /// videos by id while they are being sampled.
    roots: Vec<PathBuf>,
    files: Vec<dupes::FileRec>,
    /// Opaque ids ("y" + 16 hex) → index into `files`.
    ids: HashMap<String, usize>,
    result: Option<similar::SimilarAnalysis>,
}

struct AnalysisStore {
    analysis: dupes::Analysis,
    /// Opaque analyzer file ids ("x" + 16 hex) → index into `analysis.files`.
    ids: HashMap<String, usize>,
}

/// Where an id points: the authorised root it is confined to and its path
/// relative to that root.
pub struct Located {
    pub root: PathBuf,
    pub rel: String,
    pub ext: String,
    pub name: String,
    pub is_dir: bool,
    /// A symbolic link: described and listed, never followed or opened.
    pub is_link: bool,
}

fn analysis_id(root: &Path, rel: &str) -> String {
    format!("x{:016x}", thumbs::fnv(format!("{}\u{0}{rel}", root.to_string_lossy()).as_bytes()))
}

impl AppState {
    /// Stable key for one version of a file (path + size + mtime).
    pub fn file_key(&self, canon: &Path, meta: &fs::Metadata) -> String {
        // Just the opaque hash: nothing about the path is stored in the blocklist.
        let stem = thumbs::stem(&self.thumb_dir, canon, meta, 0);
        stem.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Probe (cached) a confined video file.
    pub fn video_info(&self, canon: &Path, meta: &fs::Metadata, detected: Detected) -> video::VideoInfo {
        let key = self.file_key(canon, meta);
        self.video.info(&key, || {
            // Re-open through the confinement check; never trust `canon` alone.
            let root = self.authorised_roots().into_iter().find(|r| canon.starts_with(r));
            let reopened = root.and_then(|r| canon.strip_prefix(&r).ok().map(|rel| (r.clone(), rel.to_path_buf())));
            match reopened.and_then(|(r, rel)| secure::open_inside(&r, rel.to_str()?).ok()) {
                Some((f, m, _)) => video::probe_file(f, m.len(), detected),
                None => video::probe_file_unavailable(),
            }
        })
    }

    pub fn video_playable(&self, canon: &Path, meta: &fs::Metadata, detected: Detected) -> bool {
        self.video_info(canon, meta, detected).status == video::VideoStatus::Playable
    }

    /// The gate for every change to the user's files.
    pub fn policy(&self) -> policy::Policy<'_> {
        policy::Policy {
            read_only: self.read_only.load(Ordering::SeqCst) || self.session_read_only.load(Ordering::SeqCst),
            protected: &self.protected,
        }
    }

    pub fn root_canon(&self) -> Option<PathBuf> {
        self.root.read().unwrap_or_else(PoisonError::into_inner).as_ref().map(|r| r.canon.clone())
    }

    pub fn index(&self) -> Arc<Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Thumbnails are kept per volume, so "Forget this drive" can remove
    /// exactly that drive's thumbnails.
    pub fn thumb_dir_for(&self, canon: &Path) -> PathBuf {
        let key = drives::key_of(&privacy::volume_of(canon));
        self.thumb_dir.join(format!("v{:016x}", thumbs::fnv(key.as_bytes())))
    }

    /// Changes to Mori's own records are refused in a temporary session.
    fn persistent(&self) -> Result<(), String> {
        if self.temp.load(Ordering::SeqCst) {
            Err("Not available in a temporary session: Mori isn't saving anything about this folder.".into())
        } else {
            Ok(())
        }
    }

    fn index_dir(&self) -> PathBuf {
        self.data_dir.join("index-v2")
    }

    fn index_file(&self, root: &Path) -> PathBuf {
        let h = thumbs::fnv(root.to_string_lossy().as_bytes());
        self.index_dir().join(format!("{h:016x}.json"))
    }

    /// Resolve a browser id (16 hex), an exact-duplicate id ("x" + 16 hex) or
    /// a similar-media id ("y" + 16 hex).
    pub fn locate(&self, id: &str) -> Result<Located, String> {
        if id.starts_with('y') {
            let store = self.similar.lock().unwrap_or_else(PoisonError::into_inner);
            let store = store.as_ref().ok_or("The analysis was cleared.")?;
            let &i = store.ids.get(id).ok_or("Unknown file")?;
            let f = &store.files[i];
            return Ok(Located {
                root: store.roots[f.root].clone(),
                rel: f.rel.clone(),
                ext: f.ext.clone(),
                name: f.name.clone(),
                is_dir: false,
                is_link: false,
            });
        }
        if id.starts_with('m') {
            let store = self.meta_scan.lock().unwrap_or_else(PoisonError::into_inner);
            let (root, rel, ext, name) =
                store.as_ref().ok_or("The scan was cleared.")?.locate(id).ok_or("Unknown file")?;
            return Ok(Located { root, rel, ext, name, is_dir: false, is_link: false });
        }
        if id.starts_with('x') {
            let store = self.analysis.lock().unwrap_or_else(PoisonError::into_inner);
            let store = store.as_ref().ok_or("The analysis was cleared.")?;
            let &i = store.ids.get(id).ok_or("Unknown file")?;
            let f = &store.analysis.files[i];
            return Ok(Located {
                root: store.analysis.roots[f.root].canon.clone(),
                rel: f.rel.clone(),
                ext: f.ext.clone(),
                name: f.name.clone(),
                is_dir: false,
                is_link: false,
            });
        }
        let root = self.root_canon().ok_or("No folder selected")?;
        let idx = self.index();
        let e = idx.get(id).ok_or("Unknown item")?;
        Ok(Located {
            root,
            rel: e.path.clone(),
            ext: e.ext.clone(),
            name: e.name.clone(),
            is_dir: e.kind == index::Kind::Folder,
            is_link: e.kind == index::Kind::Link,
        })
    }

    /// Roots files may currently be read from: the browsed folder plus the
    /// locations of the current analysis.
    pub fn authorised_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.root_canon().into_iter().collect();
        if let Some(s) = self.analysis.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            v.extend(s.analysis.roots.iter().map(|r| r.canon.clone()));
        }
        if let Some(s) = self.similar.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            v.extend(s.roots.iter().cloned());
        }
        if let Some(s) = self.meta_scan.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            v.extend(s.roots().cloned());
        }
        v
    }

    /// Open a file by id, confined to its root.
    fn open_by_id(&self, id: &str) -> Result<(fs::File, fs::Metadata, PathBuf, String), String> {
        let loc = self.locate(id)?;
        if loc.is_dir || loc.is_link {
            // Links are never followed: opening one would read its target.
            return Err("Not a file".into());
        }
        let (f, m, canon) = secure::open_inside(&loc.root, &loc.rel).map_err(|_| "File unavailable")?;
        Ok((f, m, canon, loc.ext))
    }

    /// Canonical path of an indexed file or folder ("" = root), confined to the root.
    fn path_by_id(&self, id: &str) -> Result<PathBuf, String> {
        let root = self.root_canon().ok_or("No folder selected")?;
        if id.is_empty() {
            return Ok(root);
        }
        let loc = self.locate(id)?;
        if Path::new(&loc.rel).components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err("Invalid path".into());
        }
        // A link is shown as itself: resolve its folder, never its target.
        let (dir, last) = match loc.rel.rsplit_once('/') {
            Some((d, n)) if loc.is_link => (d.to_owned(), Some(n.to_owned())),
            None if loc.is_link => (String::new(), Some(loc.rel.clone())),
            _ => (loc.rel.clone(), None),
        };
        let canon = fs::canonicalize(loc.root.join(&dir)).map_err(|_| "Item unavailable")?;
        if !canon.starts_with(&loc.root) {
            return Err("Item unavailable".into());
        }
        Ok(match last {
            Some(name) => canon.join(name),
            None => canon,
        })
    }
}

// ------------------------------------------------------------------ settings

/// The complete, typed settings file. Unknown keys are dropped.
#[derive(Serialize, Deserialize, Default, Clone)]
struct Settings {
    #[serde(default)]
    root: Option<String>,
    #[serde(default)]
    view: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    desc: Option<bool>,
    #[serde(default)]
    recursive: Option<bool>,
    #[serde(default)]
    search_global: Option<bool>,
    #[serde(default)]
    read_only: Option<bool>,
}

const VIEWS: &[&str] = &["gallery", "grid", "list"];
const SORTS: &[&str] = &["name", "modified", "created", "size", "type"];

fn read_settings(state: &AppState) -> Settings {
    let s: Settings = fs::read(state.data_dir.join("settings.json"))
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default();
    Settings {
        view: s.view.filter(|v| VIEWS.contains(&v.as_str())),
        sort: s.sort.filter(|v| SORTS.contains(&v.as_str())),
        ..s
    }
}

fn write_settings(state: &AppState, f: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let _guard = state.settings_lock.lock().unwrap_or_else(PoisonError::into_inner);
    let mut s = read_settings(state);
    f(&mut s);
    fs::create_dir_all(&state.data_dir).map_err(|_| "Could not save settings")?;
    fs::write(state.data_dir.join("settings.json"), serde_json::to_vec_pretty(&s).unwrap())
        .map_err(|_| "Could not save settings".into())
}

// ------------------------------------------------------------ drive detection

/// If Mori is running from an external drive, return that drive's root.
fn detect_portable_root() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        if let Some(p) = std::env::var_os("MORI_ROOT").map(PathBuf::from).filter(|p| p.is_dir()) {
            return Some(p);
        }
    }
    let exe = std::env::current_exe().ok()?;
    portable_root_for(&exe)
}

#[cfg(target_os = "macos")]
fn portable_root_for(exe: &Path) -> Option<PathBuf> {
    // /Volumes/<Drive>/Mori/Mori.app/Contents/MacOS/mori -> /Volumes/<Drive>
    let mut c = exe.components();
    match (c.next(), c.next(), c.next()) {
        (Some(Component::RootDir), Some(Component::Normal(v)), Some(Component::Normal(name))) if v == "Volumes" => {
            Some(Path::new("/Volumes").join(name))
        }
        _ => None,
    }
}

#[cfg(windows)]
fn portable_root_for(exe: &Path) -> Option<PathBuf> {
    // E:\Mori\Mori.exe -> E:\ (but never the system drive).
    let Some(Component::Prefix(prefix)) = exe.components().next() else { return None };
    let drive = prefix.as_os_str().to_string_lossy().to_string();
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    if drive.eq_ignore_ascii_case(&system) {
        return None;
    }
    Some(PathBuf::from(format!("{drive}\\")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn portable_root_for(exe: &Path) -> Option<PathBuf> {
    // /media/<user>/<drive>, /run/media/<user>/<drive>, /mnt/<drive>
    let parts: Vec<_> = exe.components().collect();
    let names: Vec<String> = parts.iter().map(|c| c.as_os_str().to_string_lossy().into()).collect();
    let depth = match names.get(1).map(String::as_str) {
        Some("media") => 4,
        Some("run") if names.get(2).map(String::as_str) == Some("media") => 5,
        Some("mnt") => 3,
        _ => return None,
    };
    (names.len() > depth).then(|| parts[..depth].iter().collect())
}

// ------------------------------------------------------------------ scanning

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Status {
    has_root: bool,
    root_name: String,
    scanning: bool,
    scan_count: usize,
    file_count: usize,
    scanned_at: i64,
    /// Safe Inspection Mode for this drive (no automatic decoding).
    safe_mode: bool,
    /// Media decoded since this root was opened.
    decoded: u64,
    /// A temporary session: nothing about this folder is saved.
    temporary: bool,
    /// Private Inspection (temporary + read-only + Safe Inspection Mode).
    private_inspection: bool,
    /// Read-only in effect (the setting, or forced by Private Inspection).
    read_only: bool,
}

fn status(state: &AppState) -> Status {
    let root = state.root.read().unwrap_or_else(PoisonError::into_inner);
    let idx = state.index();
    Status {
        has_root: root.is_some(),
        root_name: root.as_ref().map(|r| secure::display_safe(&r.name)).unwrap_or_default(),
        scanning: state.scanning.load(Ordering::SeqCst),
        scan_count: state.scan_count.load(Ordering::SeqCst),
        file_count: idx.files.len(),
        scanned_at: idx.scanned_at,
        safe_mode: state.safe_mode.load(Ordering::SeqCst),
        decoded: state.decoded.load(Ordering::Relaxed),
        temporary: state.temp.load(Ordering::SeqCst),
        private_inspection: state.private_inspection.load(Ordering::SeqCst),
        read_only: state.policy().read_only,
    }
}

fn emit_status(app: &AppHandle) {
    let _ = app.emit("status", status(&app.state::<AppState>()));
}

/// The one place a new index becomes visible: private-folder boundaries are
/// applied here, so every query, search and count respects them.
fn publish_index(app: &AppHandle, mut idx: Index) {
    let state = app.state::<AppState>();
    if let Some(root) = state.root_canon() {
        idx.apply_boundaries(&state.privacy.boundaries(&root));
        idx.apply_protection(&state.protected.boundaries(&root), state.protected.covering(&root).is_some());
        idx.apply_capture(&state.capture_not.boundaries(&root), &state.capture_yes.boundaries(&root));
        idx.apply_org(&state.favorites.boundaries(&root), &state.tags.for_root(&root));
    }
    *state.index.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(idx);
    let _ = app.emit("index-changed", ());
}

fn start_scan(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(root) = state.root_canon() else { return };
    let gen = state.scan_gen.fetch_add(1, Ordering::SeqCst) + 1;
    state.scanning.store(true, Ordering::SeqCst);
    state.scan_count.store(0, Ordering::SeqCst);
    emit_status(app);

    let app = app.clone();
    std::thread::spawn(move || {
        let _ = catch_unwind(AssertUnwindSafe(|| scan_thread(&app, root, gen)));
        let state = app.state::<AppState>();
        // Whatever happened, never leave the UI showing "Scanning…" forever.
        if state.scan_gen.load(Ordering::SeqCst) == gen && state.scanning.swap(false, Ordering::SeqCst) {
            emit_status(&app);
        }
    });
}

fn scan_thread(app: &AppHandle, root: PathBuf, gen: u64) {
    {
        let app = app.clone();
        let state = app.state::<AppState>();
        let cancelled = || state.scan_gen.load(Ordering::SeqCst) != gen;
        // With no previous index, show results progressively while scanning.
        let progressive = state.index().files.is_empty();
        let mut last_emit = std::time::Instant::now();
        let mut progress = |count: usize, snapshot: index::Snapshot<'_>| {
            state.scan_count.store(count, Ordering::SeqCst);
            if let Some((files, dirs)) = snapshot {
                // Follow moved private folders before anything is shown.
                let found: Vec<(&str, u64)> = dirs.iter().map(|d| (d.path.as_str(), d.ino)).collect();
                state.privacy.reconcile(&root, &found);
                state.protected.reconcile(&root, &found);
                let partial = Index::new(root.to_string_lossy().into_owned(), 0, files.to_vec(), dirs.to_vec());
                publish_index(&app, partial);
            }
            if last_emit.elapsed() > Duration::from_millis(150) {
                last_emit = std::time::Instant::now();
                emit_status(&app);
            }
        };
        let every = progressive.then(|| Duration::from_millis(1500));
        let Some(idx) = index::scan(&root, &cancelled, &mut progress, every) else { return };
        if cancelled() {
            return;
        }
        if !state.temp.load(Ordering::SeqCst) {
            if index::save(&state.index_file(&root), &idx).is_err() {
                debug_log!("mori: could not save index");
            }
            // Private folders moved or renamed outside Mori: follow them by inode.
            let dirs: Vec<(&str, u64)> = idx.dirs.iter().map(|d| (d.path.as_str(), d.ino)).collect();
            state.privacy.reconcile(&root, &dirs);
            state.protected.reconcile(&root, &dirs);
        }
        publish_index(&app, idx);
        state.scanning.store(false, Ordering::SeqCst);
        emit_status(&app);
    }
}

fn open_root(app: &AppHandle, root: &Path) -> Result<(), String> {
    let canon = fs::canonicalize(root).map_err(|_| "That folder can't be opened")?;
    if !canon.is_dir() {
        return Err("That isn't a folder".into());
    }
    let state = app.state::<AppState>();
    let name =
        canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| secure::plain_path(&canon));
    *state.root.write().unwrap_or_else(PoisonError::into_inner) = Some(RootInfo { canon: canon.clone(), name });
    state.safe_mode.store(state.drives.safe_for(&canon) || state.session_safe.load(Ordering::SeqCst), Ordering::SeqCst);
    state.decoded.store(0, Ordering::SeqCst);
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let cached = index::load(&state.index_file(&canon), &canon).unwrap_or_default();
    publish_index(app, cached);
    start_scan(app);
    Ok(())
}

// ------------------------------------------------------------------ commands

fn launch_id() -> u64 {
    static ID: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *ID.get_or_init(|| {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        thumbs::fnv(format!("{t}|{}", std::process::id()).as_bytes()) >> 11 // fits a JS number exactly
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitInfo {
    status: Status,
    view: Option<String>,
    sort: Option<String>,
    desc: Option<bool>,
    recursive: Option<bool>,
    search_global: Option<bool>,
    read_only: bool,
    platform: &'static str,
    /// Differs on every app launch; lets the UI tell an in-process reload
    /// (restore where the user was) from a fresh start.
    launch_id: u64,
    /// macOS moved the app to a random read-only location (quarantine).
    translocated: bool,
}

#[tauri::command]
fn init(app: AppHandle, state: State<'_, AppState>) -> InitInfo {
    debug_log!("mori: UI init");
    let settings = read_settings(&state);
    if state.root.read().unwrap_or_else(PoisonError::into_inner).is_none() {
        let saved = settings.root.as_ref().map(PathBuf::from).filter(|p| p.is_dir());
        let detected = detect_portable_root();
        // Prefer the drive Mori lives on, unless the user picked a folder inside it.
        let chosen = match (detected, saved) {
            (Some(d), Some(s)) if s.starts_with(&d) => Some(s),
            (Some(d), _) => Some(d),
            (None, s) => s,
        };
        if let Some(root) = chosen {
            if open_root(&app, &root).is_err() {
                debug_log!("mori: could not open saved root");
            }
        }
    }
    InitInfo {
        status: status(&state),
        view: settings.view,
        sort: settings.sort,
        desc: settings.desc,
        recursive: settings.recursive,
        search_global: settings.search_global,
        read_only: state.read_only.load(Ordering::SeqCst),
        platform: std::env::consts::OS,
        launch_id: launch_id(),
        translocated: std::env::current_exe()
            .map(|p| p.to_string_lossy().contains("AppTranslocation"))
            .unwrap_or(false),
    }
}

/// Only these preferences can be written by the UI, and only with known values.
#[tauri::command]
fn update_settings(
    state: State<'_, AppState>,
    view: String,
    sort: String,
    desc: bool,
    recursive: bool,
    search_global: bool,
) -> Result<(), String> {
    if !VIEWS.contains(&view.as_str()) || !SORTS.contains(&sort.as_str()) {
        return Err("Invalid setting".into());
    }
    write_settings(&state, |s| {
        s.view = Some(view);
        s.sort = Some(sort);
        s.desc = Some(desc);
        s.recursive = Some(recursive);
        s.search_global = Some(search_global);
    })
}

/// The folder picker runs on the Rust side, so the UI never supplies a path.
#[tauri::command]
async fn choose_root(app: AppHandle) -> Result<Status, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title("Choose a drive or folder for Mori").blocking_pick_folder();
    forget_open_panel_location();
    let Some(picked) = picked else {
        return Err("cancelled".into());
    };
    let path = picked.into_path().map_err(|_| "That folder can't be opened")?;
    app.state::<AppState>().temp.store(false, Ordering::SeqCst);
    open_root(&app, &path)?;
    let state = app.state::<AppState>();
    let canon = state.root_canon().unwrap();
    write_settings(&state, |s| s.root = Some(canon.to_string_lossy().into_owned()))?;
    Ok(status(&state))
}

#[tauri::command]
fn rescan(app: AppHandle) {
    start_scan(&app);
}

/// Remove generated thumbnails, previews and indexes. Originals are untouched.
#[tauri::command]
fn clear_cache(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    state.scan_gen.fetch_add(1, Ordering::SeqCst); // cancel any running scan
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.volatile_thumbs.clear();
    for dir in [&state.thumb_dir, &state.index_dir(), &state.similar_dir()] {
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|_| "Could not clear the cache")?;
        }
    }
    fs::create_dir_all(&state.thumb_dir).map_err(|_| "Could not clear the cache")?;
    publish_index(&app, Index::default());
    start_scan(&app);
    Ok(())
}

#[tauri::command]
fn get_status(state: State<'_, AppState>) -> Status {
    status(&state)
}

#[derive(Serialize)]
struct Crumb {
    id: String,
    name: String,
}

#[derive(Serialize)]
struct QueryResponse {
    items: Vec<Item>,
    total: usize,
    truncated: bool,
    crumbs: Vec<Crumb>,
    /// The current folder is private or inside a private folder.
    #[serde(rename = "privateScope")]
    private_scope: bool,
    /// The current folder is protected or inside a protected folder.
    #[serde(rename = "protectedScope")]
    protected_scope: bool,
}

#[tauri::command]
async fn query(state: State<'_, AppState>, q: Query) -> Result<QueryResponse, ()> {
    let idx = state.index();
    let mut q = q;
    q.search.truncate(256);
    let r = index::query(&idx, &q);
    let base = r.base;
    Ok(QueryResponse {
        items: r
            .items
            .into_iter()
            .map(|e| Item { location: Some(secure::display_safe(index::location(e, base))), ..Item::from(e) })
            .collect(),
        total: r.total,
        truncated: r.truncated,
        crumbs: index::crumbs(&idx, &q.folder).into_iter().map(|(id, name)| Crumb { id, name }).collect(),
        private_scope: r.private_scope,
        protected_scope: r.protected_scope,
    })
}

#[tauri::command]
fn stats(state: State<'_, AppState>) -> index::Stats {
    index::stats(&state.index())
}

#[tauri::command]
fn subfolders(state: State<'_, AppState>, parent: String) -> Vec<Item> {
    index::subfolders(&state.index(), &parent).into_iter().map(Item::from).collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Inspection {
    detected: Detected,
    /// "image" | "video" | "text" | "none"
    preview: &'static str,
    can_open: bool,
    /// Extension and actual content disagree.
    mismatch: bool,
    /// Container/codec details for videos.
    video: Option<video::VideoInfo>,
    /// Automatic previews are off (Safe Inspection Mode).
    previews_off: bool,
}

/// Decide how (and whether) a file may be previewed, from its magic bytes.
#[tauri::command]
async fn inspect(state: State<'_, AppState>, id: String) -> Result<Inspection, String> {
    let (mut file, meta, canon, ext) = state.open_by_id(&id)?;
    let head = secure::read_head(&mut file, secure::SNIFF_LEN);
    let detected = secure::sniff(&head, &ext);
    drop(file);
    let video = detected.is_video().then(|| state.video_info(&canon, &meta, detected));
    let mismatch = match secure::expected_for_ext(&ext) {
        Some(expected) => !expected.contains(&detected),
        None => detected.is_image() || detected.is_video() || detected == Detected::Executable,
    };
    let ft = filetype::detect(&head, meta.len());
    let preview = if detected.is_image() {
        "image"
    } else if video.as_ref().is_some_and(|v| v.status == video::VideoStatus::Playable) {
        "video"
    } else if detected == Detected::Text {
        "text"
    } else if detected == Detected::Pdf && cfg!(target_os = "macos") {
        "pdf"
    } else if matches!(ft.id, "zip" | "tar" | "gzip" | "jar" | "ooxml" | "odf" | "epub") {
        "archive"
    } else if protocol::audio_mime(&head, meta.len()).is_some() {
        "audio"
    } else {
        "none"
    };
    let previews_off =
        state.safe_mode.load(Ordering::SeqCst) && state.root_canon().is_some_and(|r| canon.starts_with(r));
    // A file whose content contradicts its name is suspicious: never hand it to another app.
    // Never offered in a temporary session: other apps keep their own history.
    let can_open = !mismatch && secure::may_open_externally(&ext, detected) && !state.temp.load(Ordering::SeqCst);
    Ok(Inspection { detected, preview, can_open, mismatch, video, previews_off })
}

/// Begin the (single) active media session for a video about to be loaded by
/// the webview. Any previous session is superseded.
#[tauri::command]
async fn video_session_start(state: State<'_, AppState>, id: String) -> Result<u64, String> {
    let (_, meta, canon, _) = state.open_by_id(&id)?;
    let name = state.locate(&id).map(|l| secure::display_safe(&l.name)).unwrap_or_default();
    Ok(state.video.start(state.file_key(&canon, &meta), name))
}

/// UI liveness ping, sent every second while the page is alive.
#[tauri::command]
fn ui_alive(state: State<'_, AppState>) {
    if cfg!(debug_assertions) && std::env::var_os("MORI_DEBUG_PINGS").is_some() {
        eprintln!("mori: ping");
    }
    state.video.ui_beat();
}

#[tauri::command]
fn video_session_end(state: State<'_, AppState>, token: u64) {
    state.video.end(token);
}

/// Names of videos that crashed/hung the player since the UI last asked.
#[tauri::command]
fn take_recovered(state: State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.video.recovered.lock().unwrap_or_else(PoisonError::into_inner))
}

#[tauri::command]
fn preview_info(state: State<'_, AppState>, id: String) -> Option<(u32, u32)> {
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).dims(&id)
}

/// Plain-text preview (first 256 KB), only for files whose content is text.
#[tauri::command]
async fn read_text(state: State<'_, AppState>, id: String) -> Result<String, String> {
    use std::io::Read;
    let (mut file, _, _, ext) = state.open_by_id(&id)?;
    let mut buf = Vec::new();
    file.by_ref().take(256 * 1024).read_to_end(&mut buf).map_err(|_| "File unavailable")?;
    if secure::sniff(&buf[..buf.len().min(secure::SNIFF_LEN)], &ext) != Detected::Text {
        return Err("This file type cannot be safely previewed.".into());
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Open a *passive* file in its default app — only when the user asks, and
/// never for anything executable or active (see `secure::may_open_externally`).
#[tauri::command]
async fn open_file(state: State<'_, AppState>, id: String) -> Result<(), String> {
    if state.temp.load(Ordering::SeqCst) {
        return Err(
            "Other apps aren't used during a temporary session: they could keep their own history of the file.".into(),
        );
    }
    let (mut file, _, canon, ext) = state.open_by_id(&id)?;
    let head = secure::read_head(&mut file, secure::SNIFF_LEN);
    let detected = secure::sniff(&head, &ext);
    let mismatch = secure::expected_for_ext(&ext).is_some_and(|e| !e.contains(&detected));
    if mismatch || !secure::may_open_externally(&ext, detected) {
        return Err("For safety, Mori won't open this kind of file.".into());
    }
    drop(file);
    system_open(&canon, false)
}

/// Show an item in Finder / Explorer (selects it; never launches it).
#[tauri::command]
fn reveal_file(state: State<'_, AppState>, id: String) -> Result<(), String> {
    if state.temp.load(Ordering::SeqCst) {
        return Err("Finder isn't used during a temporary session.".into());
    }
    system_open(&state.path_by_id(&id)?, true)
}

/// Absolute path as text, for "Copy path".
#[tauri::command]
fn copy_path(state: State<'_, AppState>, id: String) -> Result<String, String> {
    Ok(secure::plain_path(&state.path_by_id(&id)?))
}

/// Receives a PNG video frame captured by the webview (raw IPC body).
#[tauri::command]
async fn store_frame(app: AppHandle, request: tauri::ipc::Request<'_>) -> Result<bool, ()> {
    let Some(id) = request.headers().get("mori-id").and_then(|v| v.to_str().ok()).map(str::to_owned) else {
        return Ok(false);
    };
    let frame = request.headers().get("mori-frame").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u32>().ok());
    let explicit = request.headers().get("mori-explicit").is_some();
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else { return Ok(false) };
    let body = body.clone();
    // The worker can take seconds; never block the async runtime or the UI thread.
    tauri::async_runtime::spawn_blocking(move || {
        protocol::store_frame(&app.state::<AppState>(), &id, body, frame, explicit)
    })
    .await
    .map_err(|_| ())
}

// ------------------------------------------------------- file management

/// Remove a file's cached thumbnails (it is about to disappear or change name).
fn invalidate_thumbs(state: &AppState, canon: &Path, meta: &fs::Metadata) {
    let stem = thumbs::stem(&state.thumb_dir_for(canon), canon, meta, protocol::THUMB_SIZE);
    for ext in ["jpg", "png", "none"] {
        let _ = fs::remove_file(stem.with_extension(ext));
    }
}

/// Apply an in-place change to the browser index, persist it and refresh the UI.
fn apply_index_change(app: &AppHandle, change: impl FnOnce(&mut Index)) {
    let state = app.state::<AppState>();
    let mut idx = (*state.index()).clone();
    change(&mut idx);
    if let Some(root) = state.root_canon().filter(|_| !state.temp.load(Ordering::SeqCst)) {
        if index::save(&state.index_file(&root), &idx).is_err() {
            debug_log!("mori: could not save index");
        }
    }
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
    publish_index(app, idx);
    // A walk in progress may already have seen the old state: restart it.
    if state.scanning.load(Ordering::SeqCst) {
        start_scan(app);
    }
}

/// Paths (relative to the browsed root) of removed items that live inside it.
fn browser_rel(state: &AppState, abs: &Path) -> Option<String> {
    let root = state.root_canon()?;
    let rel = abs.strip_prefix(&root).ok()?.to_str()?;
    (!rel.is_empty()).then(|| if cfg!(windows) { rel.replace('\\', "/") } else { rel.to_owned() })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashSummary {
    files: u64,
    folders: u64,
    bytes: u64,
}

/// What a Trash operation would affect (for the confirmation dialog).
#[tauri::command]
fn trash_summary(state: State<'_, AppState>, ids: Vec<String>) -> TrashSummary {
    let idx = state.index();
    let mut s = TrashSummary { files: 0, folders: 0, bytes: 0 };
    for id in ids.iter().take(100_000) {
        if let Ok(loc) = state.locate(id) {
            if loc.is_dir {
                s.folders += 1;
                let prefix = format!("{}/", loc.rel);
                for f in idx.files.iter().filter(|f| f.path.starts_with(&prefix)) {
                    s.files += 1;
                    s.bytes += f.size;
                }
            } else {
                s.files += 1;
                s.bytes += fs::metadata(loc.root.join(&loc.rel)).map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    s
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrashResult {
    trashed: Vec<String>,
    bytes: u64,
    failed: Vec<dupes::Failure>,
}

/// Move items to the OS Trash / Recycle Bin. Never deletes permanently.
#[tauri::command]
async fn trash_items(app: AppHandle, ids: Vec<String>) -> Result<TrashResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut out = TrashResult { trashed: Vec::new(), bytes: 0, failed: Vec::new() };
        let mut removed_rels = Vec::new();
        let mut removed_abs = Vec::new();
        let mut changes = Vec::new();
        for id in ids.iter().take(100_000) {
            let loc = match state.locate(id) {
                Ok(l) => l,
                Err(e) => {
                    out.failed.push(dupes::Failure { path: id.clone(), reason: e });
                    continue;
                }
            };
            let shown = secure::display_safe(&loc.rel);
            let (path, meta) = match fileops::confined_item(&loc.root, &loc.rel) {
                Ok(v) => v,
                Err(e) => {
                    out.failed.push(dupes::Failure { path: shown, reason: e });
                    continue;
                }
            };
            if id.starts_with('x') && !meta.is_file() {
                out.failed.push(dupes::Failure { path: shown, reason: "not a regular file".into() });
                continue;
            }
            let bytes = if meta.is_dir() { trash_summary(state.clone(), vec![id.clone()]).bytes } else { meta.len() };
            if meta.is_file() {
                invalidate_thumbs(&state, &path, &meta);
            }
            match fileops::move_to_trash(&state.policy(), &path) {
                Ok(trashed) => {
                    out.trashed.push(id.clone());
                    out.bytes += bytes;
                    if let Some(rel) = browser_rel(&state, &path) {
                        removed_rels.push(rel);
                    }
                    changes.push(history::Change::Trashed { original: path.clone(), trashed });
                    removed_abs.push(path);
                }
                Err(e) => out.failed.push(dupes::Failure { path: shown, reason: e }),
            }
        }
        if !removed_rels.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&removed_rels));
        }
        forget_removed(&state, &removed_abs);
        state.history.record(trash_label(&changes), changes);
        out
    })
    .await
    .map_err(|_| "The operation failed.".to_string())
}

fn trash_label(changes: &[history::Change]) -> String {
    match changes {
        [history::Change::Trashed { original, .. }] => {
            format!(
                "Moved “{}” to Trash",
                secure::display_safe(
                    &original.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                )
            )
        }
        c => format!("Moved {} items to Trash", c.len()),
    }
}

/// Drop removed files (or files inside removed folders) from both analyses.
fn forget_removed(state: &AppState, removed: &[PathBuf]) {
    if removed.is_empty() {
        return;
    }
    let hit = |abs: PathBuf| removed.iter().any(|r| abs == *r || abs.starts_with(r));
    if let Some(store) = state.analysis.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        let a = &mut store.analysis;
        let gone: HashSet<usize> =
            (0..a.files.len()).filter(|&i| hit(a.roots[a.files[i].root].canon.join(&a.files[i].rel))).collect();
        dupes::forget(a, &gone);
    }
    if let Some(r) =
        state.similar.lock().unwrap_or_else(PoisonError::into_inner).as_mut().and_then(|s| s.result.as_mut())
    {
        let a = &r.analysis;
        let gone: HashSet<usize> =
            (0..a.files.len()).filter(|&i| hit(a.roots[a.files[i].root].canon.join(&a.files[i].rel))).collect();
        similar::forget(r, &gone);
    }
}

/// Rename a file or folder in the browser. Never overwrites an existing item.
#[tauri::command]
fn rename_item(app: AppHandle, state: State<'_, AppState>, id: String, name: String) -> Result<String, String> {
    if id.starts_with('x') || id.starts_with('y') {
        return Err("Rename items from the browser.".into());
    }
    fileops::validate_name(&name)?;
    let loc = state.locate(&id)?;
    let (path, meta) = fileops::confined_item(&loc.root, &loc.rel)?;
    let parent = path.parent().ok_or("invalid path")?;
    let target = parent.join(&name);
    if target == path {
        return Ok(id);
    }
    if meta.is_file() {
        invalidate_thumbs(&state, &path, &meta);
    }
    fileops::rename_no_replace(&state.policy(), &path, &target)?;
    state.history.record(
        format!(
            "Renamed “{}” to “{}”",
            secure::display_safe(&path.file_name().unwrap_or_default().to_string_lossy()),
            secure::display_safe(&target.file_name().unwrap_or_default().to_string_lossy())
        ),
        vec![history::Change::Renamed { from: path.clone(), to: target.clone() }],
    );
    // Mori's records follow the item: a private folder keeps its privacy, a
    // favorite or tagged file or folder keeps its favorite and tags.
    if meta.is_dir() {
        state.privacy.renamed(&path, &target);
    }
    state.capture_not.renamed(&path, &target);
    state.capture_yes.renamed(&path, &target);
    state.favorites.renamed(&path, &target);
    state.tags.renamed(&path, &target);
    let old_rel = loc.rel.clone();
    let new_rel = match old_rel.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{name}"),
        None => name.clone(),
    };
    apply_index_change(&app, |idx| idx.rename_path(&old_rel, &new_rel));
    Ok(index::id_str(index::id_for(&new_rel)))
}

// ------------------------------------------------------------ move & copy

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DestInfo {
    /// Opaque key for `plan_transfer` / `transfer_items` ("d0", "d1"…).
    key: String,
    label: String,
    path: String,
    drive: String,
}

/// Native folder picker (runs in Rust) for a Move/Copy destination outside
/// the browsed folder. Remembered for this session only, as an opaque key.
#[tauri::command]
async fn transfer_choose_folder(app: AppHandle) -> Result<DestInfo, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title("Choose a destination folder").blocking_pick_folder();
    forget_open_panel_location();
    let path = picked.ok_or("cancelled")?.into_path().map_err(|_| "That folder can't be opened")?;
    let canon = fs::canonicalize(path).ok().filter(|c| c.is_dir()).ok_or("That folder can't be opened")?;
    let state = app.state::<AppState>();
    let key = {
        let mut list = state.transfer_dests.lock().unwrap_or_else(PoisonError::into_inner);
        let n = list.iter().position(|p| *p == canon).unwrap_or_else(|| {
            list.push(canon.clone());
            list.len() - 1
        });
        format!("d{n}")
    };
    let label =
        canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| secure::plain_path(&canon));
    Ok(DestInfo { key, label: secure::display_safe(&label), path: pretty_path(&app, &canon), drive: drive_of(&canon) })
}

/// Resolve a destination: a browser folder id ("" = the root) or a folder
/// picked this session ("d0"…). Always canonical and a real folder (never a
/// link to one); browser folders stay inside the root.
fn transfer_dest(state: &AppState, dest: &str) -> Result<(PathBuf, String), String> {
    let gone = "The destination folder is no longer available.";
    // Browser ids are 16 hex digits (and may start with "d" themselves).
    let (path, inside_root) = if dest.is_empty() || index::parse_id(dest).is_some() {
        if !dest.is_empty() && state.index().folder_path(dest).is_none() {
            return Err("Unknown destination.".into());
        }
        (state.path_by_id(dest)?, true)
    } else {
        let i: usize = dest.strip_prefix('d').and_then(|n| n.parse().ok()).ok_or("Unknown destination.")?;
        let p = state.transfer_dests.lock().unwrap_or_else(PoisonError::into_inner).get(i).cloned();
        (p.ok_or("Unknown destination.")?, false)
    };
    let canon = fs::canonicalize(&path).map_err(|_| gone)?;
    if !fs::symlink_metadata(&canon).is_ok_and(|m| m.is_dir()) {
        return Err(gone.into());
    }
    if inside_root && !state.root_canon().is_some_and(|r| canon.starts_with(r)) {
        return Err(gone.into());
    }
    let name =
        canon.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| secure::plain_path(&canon));
    Ok((canon, secure::display_safe(&name)))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TransferEntry {
    id: String,
    name: String,
    path: String,
    kind: &'static str,
    bytes: u64,
    files: u64,
    /// Something with this name is already in the destination: "file",
    /// "folder" or "link" (or another selected item has the same name: "batch").
    conflict: Option<&'static str>,
    /// Replace is offered: both are regular files, the existing one may go
    /// to the Trash, and it isn't the item itself.
    replaceable: bool,
    /// Refused (policy, folder into itself, already there…), with the reason.
    blocked: Option<String>,
    note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransferPlan {
    op: String,
    dest_name: String,
    /// Another volume: a move copies, verifies, then moves the original to the Trash.
    cross_volume: bool,
    entries: Vec<TransferEntry>,
    total_bytes: u64,
    total_files: u64,
    blocked: usize,
    conflicts: usize,
}

const ALREADY_HERE: &str = "Already in this folder.";

/// A dry run of Move/Copy: per item, what would happen and what's refused.
/// Nothing changes. Returns the plan, each item's source path (None when
/// unavailable) and the destination folder.
fn transfer_plan(
    state: &AppState,
    op: &str,
    ids: &[String],
    dest: &str,
) -> Result<(TransferPlan, Vec<Option<PathBuf>>, PathBuf), String> {
    let moving = match op {
        "move" => true,
        "copy" => false,
        _ => return Err("Unknown operation".into()),
    };
    let (dest_dir, dest_name) = transfer_dest(state, dest)?;
    let root = state.root_canon().ok_or("No folder selected")?;
    let policy = state.policy();
    let idx = state.index();
    let dest_vol = drives::key_of(&privacy::volume_of(&dest_dir));
    let mut entries = Vec::new();
    let mut paths = Vec::new();
    let mut names: HashSet<String> = HashSet::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cross_volume = false;
    for id in ids.iter().take(100_000) {
        // Browser items only, each once.
        if index::parse_id(id).is_none() || !seen.insert(id.as_str()) {
            continue;
        }
        let Some(e) = idx.get(id) else { continue };
        let shown = secure::display_safe(&e.path);
        let mut entry = TransferEntry {
            id: id.clone(),
            name: secure::display_safe(&e.name),
            path: shown,
            kind: "file",
            bytes: 0,
            files: 0,
            conflict: None,
            replaceable: false,
            blocked: None,
            note: None,
        };
        let (path, meta) = match fileops::confined_item(&root, &e.path) {
            Ok(v) => v,
            Err(err) => {
                entry.blocked = Some(err);
                entries.push(entry);
                paths.push(None);
                continue;
            }
        };
        let ft = meta.file_type();
        (entry.kind, entry.bytes, entry.files) = if ft.is_symlink() {
            ("link", 0, 0)
        } else if ft.is_dir() {
            let prefix = format!("{}/", e.path);
            let (b, n) =
                idx.files.iter().filter(|f| f.path.starts_with(&prefix)).fold((0, 0), |(b, n), f| (b + f.size, n + 1));
            ("folder", b, n)
        } else {
            ("file", meta.len(), 1)
        };
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let target = dest_dir.join(&file_name);
        let same_folder = path.parent() == Some(dest_dir.as_path());
        entry.blocked = if moving && same_folder {
            Some(ALREADY_HERE.into())
        } else if fileops::is_within(&dest_dir, &path) {
            Some(format!("A folder can't be {} into itself.", if moving { "moved" } else { "copied" }))
        } else {
            moving
                .then(|| policy.check(policy::Op::Move, &path).err())
                .flatten()
                .or_else(|| policy.check(policy::Op::Create, &target).err())
        };
        let other_volume = drives::key_of(&privacy::volume_of(&path)) != dest_vol;
        if entry.blocked.is_none() {
            if let Ok(existing) = fs::symlink_metadata(&target) {
                let et = existing.file_type();
                entry.conflict = Some(if et.is_symlink() {
                    "link"
                } else if et.is_dir() {
                    "folder"
                } else {
                    "file"
                });
                entry.replaceable =
                    !same_folder && ft.is_file() && et.is_file() && policy.check(policy::Op::Trash, &target).is_ok();
            } else if !names.insert(index::fold(&file_name)) {
                entry.conflict = Some("batch");
            }
            if moving && other_volume {
                cross_volume = true;
            }
            entry.note = if ft.is_symlink() {
                Some("Only the link itself is transferred; what it points to is never touched.".into())
            } else if moving && other_volume {
                Some("Another drive: copied and checked, then the original goes to the Trash.".into())
            } else {
                None
            };
        }
        entries.push(entry);
        paths.push(Some(path));
    }
    let ok = || entries.iter().filter(|e| e.blocked.is_none());
    let plan = TransferPlan {
        op: op.to_owned(),
        dest_name,
        cross_volume,
        total_bytes: ok().map(|e| e.bytes).sum(),
        total_files: ok().map(|e| e.files).sum(),
        blocked: entries.iter().filter(|e| e.blocked.is_some()).count(),
        conflicts: ok().filter(|e| e.conflict.is_some()).count(),
        entries,
    };
    Ok((plan, paths, dest_dir))
}

#[tauri::command]
async fn plan_transfer(app: AppHandle, op: String, ids: Vec<String>, dest: String) -> Result<TransferPlan, String> {
    tauri::async_runtime::spawn_blocking(move || transfer_plan(&app.state::<AppState>(), &op, &ids, &dest).map(|p| p.0))
        .await
        .map_err(|_| "The plan couldn't be made.".to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Transferred {
    id: String,
    /// The item's id at its new place, when that's inside the browsed folder.
    new_id: Option<String>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct TransferResult {
    done: Vec<Transferred>,
    bytes: u64,
    skipped: usize,
    failed: Vec<dupes::Failure>,
    /// Copied but the original couldn't be moved to the Trash (both exist).
    originals_kept: Vec<dupes::Failure>,
    cancelled: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TransferProgress {
    done: usize,
    total: usize,
}

/// Move or copy items into a folder. Re-plans and re-checks everything
/// here. Conflicts are resolved per item id: "keepBoth" (a free name),
/// "replace" (only when the plan offers it: the existing file goes to the
/// Trash first) or "skip"; an unresolved conflict is skipped. Nothing is
/// ever overwritten in place.
#[tauri::command]
async fn transfer_items(
    app: AppHandle,
    op: String,
    ids: Vec<String>,
    dest: String,
    resolutions: HashMap<String, String>,
) -> Result<TransferResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.transfer_cancel.store(false, Ordering::SeqCst);
        let (plan, paths, dest_dir) = transfer_plan(&state, &op, &ids, &dest)?;
        let moving = op == "move";
        let policy = state.policy();
        let cancel = || state.transfer_cancel.load(Ordering::SeqCst);
        let mut out = TransferResult::default();
        let mut changes = Vec::new();
        let (mut removed, mut renamed, mut added) = (Vec::new(), Vec::new(), Vec::new());
        let mut gone = Vec::new();
        let mut rescan = false;
        // Names this batch created, never replaced by a later item of the same batch.
        let mut created: HashSet<PathBuf> = HashSet::new();
        let total = plan.entries.len();
        let mut last_emit = std::time::Instant::now();
        for (k, (e, src)) in plan.entries.iter().zip(paths).enumerate() {
            if last_emit.elapsed() > Duration::from_millis(150) {
                last_emit = std::time::Instant::now();
                let _ = app.emit("transfer-progress", TransferProgress { done: k, total });
            }
            let fail = |reason: String| dupes::Failure { path: e.path.clone(), reason };
            if let Some(b) = &e.blocked {
                if b == ALREADY_HERE {
                    out.skipped += 1;
                } else {
                    out.failed.push(fail(b.clone()));
                }
                continue;
            }
            let Some(src) = src else { continue };
            if cancel() {
                out.cancelled = true;
                out.skipped += 1;
                continue;
            }
            let file_name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut target = dest_dir.join(&file_name);
            let mut replaced: Option<(PathBuf, Option<PathBuf>)> = None;
            let is_dir = fs::symlink_metadata(&src).is_ok_and(|m| m.is_dir());
            if fs::symlink_metadata(&target).is_ok() {
                match resolutions.get(&e.id).map(String::as_str) {
                    Some("keepBoth") => {
                        let free =
                            fileops::free_name(&file_name, is_dir, &|c| fs::symlink_metadata(dest_dir.join(c)).is_ok());
                        match free {
                            Some(n) => target = dest_dir.join(n),
                            None => {
                                out.failed.push(fail("no free name was found".into()));
                                continue;
                            }
                        }
                    }
                    Some("replace") if e.replaceable && !created.contains(&target) => {
                        if !fs::symlink_metadata(&target).is_ok_and(|m| m.is_file()) {
                            out.failed.push(fail("what's there now isn't a file, so it wasn't replaced".into()));
                            continue;
                        }
                        if let Ok(m) = fs::symlink_metadata(&target) {
                            invalidate_thumbs(&state, &target, &m);
                        }
                        match fileops::move_to_trash(&policy, &target) {
                            Ok(t) => replaced = Some((target.clone(), t)),
                            Err(r) => {
                                out.failed.push(fail(format!("the existing item couldn't be moved to the Trash: {r}")));
                                continue;
                            }
                        }
                    }
                    Some("replace") => {
                        out.failed.push(fail("Replace isn't available for this item.".into()));
                        continue;
                    }
                    _ => {
                        out.skipped += 1;
                        continue;
                    }
                }
            }
            let src_meta = fs::symlink_metadata(&src).ok();
            if moving {
                if let Some(m) = src_meta.as_ref().filter(|m| m.is_file()) {
                    invalidate_thumbs(&state, &src, m);
                }
            }
            let r = if moving {
                fileops::move_item(&policy, &src, &target, &cancel)
            } else {
                fileops::copy_item(&policy, &src, &target, &cancel)
                    .map(|bytes| fileops::Moved::Copied { bytes, trashed: None })
            };
            let outcome = match r {
                Ok(m) => m,
                Err(reason) => {
                    // Put a replaced item back where it was (when the platform says where it went).
                    if let Some((orig, Some(t))) = &replaced {
                        let _ = fileops::restore_from_trash(&policy, t, orig);
                    } else if replaced.is_some() {
                        out.originals_kept.push(fail("the item it was to replace is in the Trash".into()));
                    }
                    if reason == "cancelled" {
                        out.cancelled = true;
                        out.skipped += 1;
                    } else {
                        out.failed.push(fail(reason));
                    }
                    continue;
                }
            };
            created.insert(target.clone());
            if let Some((orig, t)) = replaced {
                if let Some(rel) = browser_rel(&state, &orig) {
                    removed.push(rel);
                }
                gone.push(orig.clone());
                changes.push(history::Change::Trashed { original: orig, trashed: t });
            }
            let src_rel = browser_rel(&state, &src);
            let dest_rel = browser_rel(&state, &target);
            match (&outcome, moving) {
                (fileops::Moved::Renamed, _) => {
                    changes.push(history::Change::Renamed { from: src.clone(), to: target.clone() });
                    // Mori's own records follow the item (same volume).
                    for s in [&state.privacy, &state.favorites, &state.capture_not, &state.capture_yes] {
                        s.renamed(&src, &target);
                    }
                    state.tags.renamed(&src, &target);
                    match (&src_rel, &dest_rel) {
                        (Some(a), Some(b)) => renamed.push((a.clone(), b.clone())),
                        (Some(a), None) => removed.push(a.clone()),
                        (None, Some(_)) => rescan = true,
                        (None, None) => {}
                    }
                    gone.push(src.clone());
                }
                (fileops::Moved::Copied { bytes, trashed }, true) => {
                    out.bytes += bytes;
                    changes.push(history::Change::CrossMoved {
                        from: src.clone(),
                        to: target.clone(),
                        trashed: trashed.clone(),
                    });
                    if let Some(a) = &src_rel {
                        removed.push(a.clone());
                    }
                    gone.push(src.clone());
                }
                (fileops::Moved::Copied { bytes, .. }, false) => {
                    out.bytes += bytes;
                    changes.push(history::Change::Created { path: target.clone() });
                }
                (fileops::Moved::CopiedOriginalKept { bytes, reason }, _) => {
                    out.bytes += bytes;
                    changes.push(history::Change::Created { path: target.clone() });
                    out.originals_kept.push(fail(format!("copied, but the original stayed: {reason}")));
                }
            }
            let copied_in = !matches!(outcome, fileops::Moved::Renamed);
            if let (true, Some(rel)) = (copied_in, &dest_rel) {
                match fs::symlink_metadata(&target) {
                    Ok(m) if m.is_file() => added.push(index::make_entry(
                        rel.clone(),
                        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        false,
                        &m,
                    )),
                    _ => rescan = true,
                }
            }
            out.done.push(Transferred { id: e.id.clone(), new_id: dest_rel.map(|r| index::id_str(index::id_for(&r))) });
        }
        if !(removed.is_empty() && renamed.is_empty() && added.is_empty()) {
            apply_index_change(&app, |idx| {
                idx.remove_paths(&removed);
                for (a, b) in &renamed {
                    idx.rename_path(a, b);
                }
                for e in added {
                    idx.add_file(e);
                }
            });
        }
        forget_removed(&state, &gone);
        if rescan {
            start_scan(&app);
        }
        let n = out.done.len();
        let what = match plan.entries.iter().find(|e| out.done.first().is_some_and(|d| d.id == e.id)) {
            Some(e) if n == 1 => format!("“{}”", e.name),
            _ => format!("{n} items"),
        };
        state
            .history
            .record(format!("{} {what} to “{}”", if moving { "Moved" } else { "Copied" }, plan.dest_name), changes);
        let _ = app.emit("transfer-progress", TransferProgress { done: total, total });
        Ok(out)
    })
    .await
    .map_err(|_| "The operation stopped unexpectedly.".to_string())?
}

/// Stop a running Move/Copy after the current item (a partly copied item
/// is removed again).
#[tauri::command]
fn transfer_cancel(state: State<'_, AppState>) {
    state.transfer_cancel.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------- quick cleanup

/// The hashes a saved cleanup session is filed under (the root and its volume).
fn cleanup_keys(state: &AppState) -> Result<(String, String), String> {
    state.persistent()?;
    let root = state.root_canon().ok_or("No folder selected")?;
    let vol = drives::key_of(&privacy::volume_of(&root));
    Ok((
        format!("{:016x}", thumbs::fnv(root.to_string_lossy().as_bytes())),
        format!("{:016x}", thumbs::fnv(vol.as_bytes())),
    ))
}

/// An unfinished Quick Cleanup of `folder`, if one was saved. Never in a
/// temporary session (nothing is saved there).
#[tauri::command]
fn cleanup_saved(state: State<'_, AppState>, folder: String) -> Option<cleanup::Session> {
    let (root, _) = cleanup_keys(&state).ok()?;
    state.cleanup.get(&root, &folder)
}

/// Save Quick Cleanup decisions so the session can be resumed. Only opaque
/// ids and Keep / Mark decisions; refused in a temporary session. This never
/// touches any file.
#[tauri::command]
fn cleanup_save(state: State<'_, AppState>, session: cleanup::Session) -> Result<(), String> {
    let (root, vol) = cleanup_keys(&state)?;
    state.cleanup.put(&root, &vol, session)
}

#[tauri::command]
fn cleanup_discard(state: State<'_, AppState>, folder: String) {
    if let Ok((root, _)) = cleanup_keys(&state) {
        state.cleanup.discard(&root, &folder);
    }
}

// ----------------------------------------------------------- inspection

/// Factual report on a file, folder or link (type, risk indicators,
/// permissions, marks). Bounded reads only; nothing is decoded or followed.
#[tauri::command]
async fn file_report(app: AppHandle, id: String) -> Result<inspect::FileReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let loc = state.locate(&id)?;
        let abs = state.path_by_id(&id)?;
        let meta = inspect::lstat(&abs).map_err(|_| "Item unavailable")?;
        let mut ctx = inspect::Context::default();
        if loc.is_link {
            let idx = state.index();
            if let Some(e) = idx.get(&id) {
                ctx.link_target = e.link.clone();
                ctx.link_outside = e.link_outside;
            }
        }
        let mut file = None;
        if meta.is_file() {
            if let Ok((mut f, m, canon)) = secure::open_inside(&loc.root, &loc.rel) {
                let head = secure::read_head(&mut f, secure::SNIFF_LEN);
                let detected = secure::sniff(&head, &loc.ext);
                let _ = std::io::Seek::rewind(&mut f);
                ctx.decode_failed =
                    thumbs::is_miss(&thumbs::stem(&state.thumb_dir_for(&canon), &canon, &m, protocol::THUMB_SIZE));
                if detected.is_video() {
                    let status = state.video_info(&canon, &m, detected).status;
                    ctx.media_blocked = status == video::VideoStatus::Blocked;
                    ctx.broken_container = status == video::VideoStatus::Damaged;
                }
                file = Some(f);
            }
        }
        let rel_display = loc.rel.clone();
        ctx.marks = inspect::Marks {
            private: meta.is_dir() && state.privacy.covering(&abs).is_some_and(|p| abs.ends_with(&p)),
            inside_private: state.privacy.covering(&abs).is_some(),
            protected: meta.is_dir() && state.protected.covering(&abs).is_some_and(|p| abs.ends_with(&p)),
            inside_protected: state.protected.covering(&abs).is_some(),
        };
        Ok(inspect::report(&abs, &rel_display, &meta, file.as_mut(), ctx))
    })
    .await
    .map_err(|_| "The report failed.".to_string())?
}

// ----------------------------------------------------------- diagnostics

/// Run Mori's self-test: what this installation can actually do right now.
#[tauri::command]
async fn run_diagnostics(app: AppHandle) -> Vec<diagnostics::Check> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let csp = app.config().app.security.csp.as_ref().map(|c| c.to_string()).unwrap_or_default();
        let index_bytes = localdata::size_of(&state.index_dir()).0;
        diagnostics::run(
            &csp,
            state.temp.load(Ordering::SeqCst),
            state.policy().read_only,
            state.stale_cleaned,
            index_bytes,
            &state.data_dir,
        )
    })
    .await
    .unwrap_or_default()
}

// ------------------------------------------------------------ local data

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionData {
    temporary: bool,
    checksums: usize,
    history: usize,
    analyses: usize,
    thumbnails_in_memory: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalData {
    groups: Vec<localdata::Group>,
    /// Held in memory only; gone when Mori quits or the session ends.
    session: SessionData,
    data_dir: String,
    cache_dir: String,
}

/// Everything Mori keeps on this computer, with sizes.
#[tauri::command]
async fn local_data(app: AppHandle) -> LocalData {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let analyses = [
            state.analysis.lock().unwrap_or_else(PoisonError::into_inner).is_some(),
            state.similar.lock().unwrap_or_else(PoisonError::into_inner).is_some(),
            state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner).is_some(),
            state.health.lock().unwrap_or_else(PoisonError::into_inner).is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        LocalData {
            groups: localdata::inventory(&state.dirs),
            session: SessionData {
                temporary: state.temp.load(Ordering::SeqCst),
                checksums: state.checksums.len(),
                history: state.history.list().len(),
                analyses,
                thumbnails_in_memory: state.temp.load(Ordering::SeqCst),
            },
            data_dir: secure::plain_path(&state.dirs.data),
            cache_dir: secure::plain_path(&state.dirs.cache),
        }
    })
    .await
    .expect("inventory")
}

fn clear_categories(app: &AppHandle, state: &AppState, cats: &[localdata::Category]) -> u64 {
    use localdata::Category as C;
    if cats.contains(&C::Cache) {
        state.scan_gen.fetch_add(1, Ordering::SeqCst);
        state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
        state.volatile_thumbs.clear();
    }
    if cats.contains(&C::Analysis) {
        state.video.clear();
        state.cleanup.clear();
    }
    if cats.contains(&C::History) {
        state.drives.clear();
        state.history.clear();
        state.custom_locations.lock().unwrap_or_else(PoisonError::into_inner).clear();
        let _ = write_settings(state, |s| s.root = None);
        forget_open_panel_location();
    }
    if cats.contains(&C::Organization) {
        state.tags.clear();
        state.favorites.clear();
        state.capture_not.clear();
        state.capture_yes.clear();
    }
    if cats.contains(&C::Rules) {
        state.privacy.clear();
        state.protected.clear();
    }
    let freed = localdata::clear(&state.dirs, cats);
    let _ = fs::create_dir_all(&state.thumb_dir);
    if cats.iter().any(|c| matches!(c, C::Organization | C::Rules)) {
        publish_index(app, (*state.index()).clone());
    }
    freed
}

/// Remove the chosen kinds of Mori's own data. Never touches user files.
#[tauri::command]
async fn clear_mori_data(app: AppHandle, categories: Vec<String>) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let cats: Vec<localdata::Category> = categories
            .iter()
            .filter_map(|c| localdata::Category::parse(c))
            .filter(|c| *c != localdata::Category::Settings)
            .collect();
        if cats.is_empty() {
            return Err("Nothing selected.".to_string());
        }
        Ok(clear_categories(&app, &app.state::<AppState>(), &cats))
    })
    .await
    .map_err(|_| "Clearing failed.".to_string())?
}

/// Reset Mori: remove all of Mori's own local data and close the folder.
/// Files on browsed drives are never touched.
#[tauri::command]
async fn reset_mori(app: AppHandle) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use localdata::Category as C;
        let state = app.state::<AppState>();
        clear_session(&state);
        state.temp.store(false, Ordering::SeqCst);
        state.private_inspection.store(false, Ordering::SeqCst);
        state.session_read_only.store(false, Ordering::SeqCst);
        state.session_safe.store(false, Ordering::SeqCst);
        state.read_only.store(false, Ordering::SeqCst);
        *state.root.write().unwrap_or_else(PoisonError::into_inner) = None;
        publish_index(&app, Index::default());
        let freed = clear_categories(
            &app,
            &state,
            &[C::Cache, C::Analysis, C::History, C::Organization, C::Rules, C::Integrity, C::Settings],
        );
        reset_user_defaults();
        emit_status(&app);
        Ok(freed)
    })
    .await
    .map_err(|_| "Reset failed.".to_string())?
}

/// Remove Mori's macOS preferences domain (window and folder-picker state).
fn reset_user_defaults() {
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| unsafe {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let d: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let id = std::ffi::CString::new(localdata::IDENTIFIER).unwrap();
        let k: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: id.as_ptr()];
        if !d.is_null() && !k.is_null() {
            let _: () = msg_send![d, removePersistentDomainForName: k];
        }
    });
}

// ------------------------------------------------------------- integrity

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Checksum {
    id: String,
    name: String,
    path: String,
    size: u64,
    modified: i64,
    detected: Option<String>,
    sha256: String,
    /// Reused from this session's memory (same file version).
    cached: bool,
}

fn checksum_of(state: &AppState, id: &str) -> Result<Checksum, String> {
    let loc = state.locate(id)?;
    if loc.is_dir || loc.is_link {
        return Err("Checksums are calculated for files.".into());
    }
    let (mut file, meta, canon) = secure::open_inside(&loc.root, &loc.rel).map_err(|_| "File unavailable")?;
    let head = secure::read_head(&mut file, filetype::HEAD_LEN);
    let detected = Some(filetype::detect(&head, meta.len()).label.to_string());
    let _ = std::io::Seek::rewind(&mut file);
    let v = integrity::version_of(&meta);
    let base = Checksum {
        id: id.to_owned(),
        name: secure::display_safe(&loc.name),
        path: secure::display_safe(&loc.rel),
        size: meta.len(),
        modified: index::make_entry(String::new(), String::new(), false, &meta).modified,
        detected,
        sha256: String::new(),
        cached: false,
    };
    if let Some(sum) = state.checksums.get(&canon, &v) {
        return Ok(Checksum { sha256: sum, cached: true, ..base });
    }
    let sum = integrity::sha256(&mut file, meta.len(), &state.checksum_cancel, &AtomicU64::new(0))?;
    // Re-check after reading: the cached value must describe this exact version.
    if fs::metadata(&canon).map(|m| integrity::version_of(&m)).ok().as_ref() == Some(&v) {
        state.checksums.put(&canon, v, sum.clone());
    }
    Ok(Checksum { sha256: sum, ..base })
}

/// SHA-256 of a file, calculated locally. Cached in memory for this session only.
#[tauri::command]
async fn checksum_file(app: AppHandle, id: String) -> Result<Checksum, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.checksum_cancel.store(false, Ordering::SeqCst);
        checksum_of(&state, &id)
    })
    .await
    .map_err(|_| "The checksum couldn't be calculated.".to_string())?
}

#[tauri::command]
fn checksum_cancel(state: State<'_, AppState>) {
    state.checksum_cancel.store(true, Ordering::SeqCst);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Comparison {
    a: Checksum,
    b: Checksum,
    /// Same SHA-256 (and size): byte-for-byte identical content.
    same: bool,
}

/// Exact comparison of two files by SHA-256 (not visual similarity).
#[tauri::command]
async fn compare_files(app: AppHandle, a: String, b: String) -> Result<Comparison, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.checksum_cancel.store(false, Ordering::SeqCst);
        let ca = checksum_of(&state, &a)?;
        let cb = checksum_of(&state, &b)?;
        Ok(Comparison { same: ca.sha256 == cb.sha256 && ca.size == cb.size, a: ca, b: cb })
    })
    .await
    .map_err(|_| "The comparison couldn't be made.".to_string())?
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IntegrityProgress {
    stage: &'static str,
    files: u64,
    bytes_done: u64,
    bytes_total: u64,
    paused: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct IntegrityDone {
    status: &'static str,
    kind: &'static str,
    message: Option<String>,
    snapshot: Option<SnapshotInfo>,
    result: Option<VerifyView>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct SnapshotInfo {
    id: String,
    label: String,
    kind: String,
    created: i64,
    files: usize,
    bytes: u64,
    skipped: integrity::Skipped,
    /// The snapshot's file or folder is reachable now.
    available: bool,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct VerifyView {
    snapshot: SnapshotInfo,
    unchanged: u64,
    changed: Vec<String>,
    missing: Vec<String>,
    added: Vec<String>,
    unreadable: Vec<String>,
}

fn snapshot_info(s: &integrity::Snapshot) -> SnapshotInfo {
    SnapshotInfo {
        id: s.id.clone(),
        label: secure::display_safe(&s.label),
        kind: s.kind.clone(),
        created: s.created,
        files: s.items.len(),
        bytes: s.items.iter().map(|i| i.size).sum(),
        skipped: s.skipped.clone(),
        available: integrity::locate(s).is_some(),
    }
}

/// Run an integrity job (save or verify) in the background with progress,
/// pause and cancel.
fn integrity_job(
    app: AppHandle,
    kind: &'static str,
    work: impl FnOnce(&AppState, &jobs::Control, &AtomicU64, &dyn Fn(&'static str, u64, u64)) -> Result<IntegrityDone, String>
        + Send
        + 'static,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    if state.integrity_running.swap(true, Ordering::SeqCst) {
        return Err("An integrity check is already running.".into());
    }
    state.integrity_job.reset();
    let job = state.integrity_job.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let bytes = AtomicU64::new(0);
        let emit = |stage: &'static str, files: u64, total: u64| {
            let _ = app.emit(
                "integrity-progress",
                IntegrityProgress {
                    stage,
                    files,
                    bytes_done: bytes.load(Ordering::Relaxed),
                    bytes_total: total,
                    paused: job.is_paused(),
                },
            );
        };
        let done = match catch_unwind(AssertUnwindSafe(|| work(&state, &job, &bytes, &emit))) {
            Ok(Ok(d)) => d,
            Ok(Err(e)) if e == "cancelled" => {
                IntegrityDone { status: "cancelled", kind, message: None, snapshot: None, result: None }
            }
            Ok(Err(e)) => IntegrityDone { status: "failed", kind, message: Some(e), snapshot: None, result: None },
            Err(_) => IntegrityDone {
                status: "failed",
                kind,
                message: Some("The check stopped unexpectedly.".into()),
                snapshot: None,
                result: None,
            },
        };
        state.integrity_running.store(false, Ordering::SeqCst);
        let _ = app.emit("integrity-done", done);
    });
    Ok(())
}

/// Hash with a progress ticker; honours pause and cancel between files.
fn hash_with_progress(
    root: &Path,
    single: bool,
    list: &[(String, u64)],
    job: &jobs::Control,
    bytes: &AtomicU64,
    emit: &dyn Fn(&'static str, u64, u64),
) -> Result<(Vec<integrity::Item>, Vec<String>), String> {
    let total: u64 = list.iter().map(|(_, s)| s).sum();
    let mut items = Vec::new();
    let mut bad = Vec::new();
    for (i, chunk) in list.chunks(64).enumerate() {
        job.checkpoint().map_err(|_| "cancelled".to_string())?;
        emit("hashing", (i * 64) as u64, total);
        let (it, b) =
            integrity::hash_all(root, single, chunk, job.cancel_flag(), bytes).map_err(|_| "cancelled".to_string())?;
        items.extend(it);
        bad.extend(b);
    }
    Ok((items, bad))
}

/// Record the current SHA-256 of a file, or of every file in a folder, as
/// an integrity snapshot in Mori's local data. Refused in a temporary session.
#[tauri::command]
fn integrity_save(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.persistent()?;
    let loc = state.locate(&id)?;
    if loc.is_link {
        return Err("Links aren't followed, so they can't be snapshotted.".into());
    }
    let (canon, _) = fileops::confined_item(&loc.root, &loc.rel).or_else(|_| {
        // The browsed root itself.
        if loc.rel.is_empty() {
            Ok((loc.root.clone(), fs::symlink_metadata(&loc.root).map_err(|_| "unavailable")?))
        } else {
            Err("That item is no longer available.".to_string())
        }
    })?;
    let (vol, root_rel) = integrity::volume_parts(&canon).ok_or("This location can't be identified.")?;
    let single = !loc.is_dir;
    let private = if single { HashSet::new() } else { state.privacy.boundaries(&canon) };
    let label = secure::display_safe(&loc.name);
    integrity_job(app, "save", move |state, job, bytes, emit| {
        emit("listing", 0, 0);
        let (list, skipped) =
            integrity::collect(&canon, &private, job.cancel_flag()).map_err(|_| "cancelled".to_string())?;
        let (items, bad) = hash_with_progress(&canon, single, &list, job, bytes, emit)?;
        let snap = integrity::Snapshot {
            version: 1,
            id: integrity::new_id(),
            created: index::now_millis(),
            kind: if single { "file" } else { "folder" }.into(),
            label,
            volume: vol.uuid.clone(),
            mount: vol.mount.to_string_lossy().into_owned(),
            root: root_rel,
            items,
            skipped: integrity::Skipped { unreadable: skipped.unreadable + bad.len() as u64, ..skipped },
        };
        state.integrity.save(&snap)?;
        Ok(IntegrityDone {
            status: "done",
            kind: "save",
            message: None,
            snapshot: Some(snapshot_info(&snap)),
            result: None,
        })
    })
}

/// Hash the snapshot's file or folder again and report what changed.
#[tauri::command]
fn integrity_verify(app: AppHandle, state: State<'_, AppState>, snapshot: String) -> Result<(), String> {
    let snap = state.integrity.load(&snapshot).ok_or("That snapshot no longer exists.")?;
    let root = integrity::locate(&snap).ok_or("The snapshot's drive or folder isn't available right now.")?;
    let single = snap.kind == "file";
    let private = if single { HashSet::new() } else { state.privacy.boundaries(&root) };
    integrity_job(app, "verify", move |_state, job, bytes, emit| {
        emit("listing", 0, 0);
        let (list, _) = integrity::collect(&root, &private, job.cancel_flag()).map_err(|_| "cancelled".to_string())?;
        let (now, bad) = hash_with_progress(&root, single, &list, job, bytes, emit)?;
        let present: HashSet<String> = list.iter().map(|(r, _)| r.clone()).collect();
        let v = integrity::compare(&snap.items, &now, bad, &present);
        let cap = |mut l: Vec<String>| {
            l.truncate(1000);
            l.into_iter().map(|p| secure::display_safe(&p)).collect::<Vec<_>>()
        };
        let result = VerifyView {
            snapshot: snapshot_info(&snap),
            unchanged: v.unchanged,
            changed: cap(v.changed),
            missing: cap(v.missing),
            added: cap(v.added),
            unreadable: cap(v.unreadable),
        };
        Ok(IntegrityDone { status: "done", kind: "verify", message: None, snapshot: None, result: Some(result) })
    })
}

#[tauri::command]
fn integrity_pause(state: State<'_, AppState>, paused: bool) {
    state.integrity_job.set_paused(paused);
}

#[tauri::command]
fn integrity_cancel(state: State<'_, AppState>) {
    state.integrity_job.cancel();
}

#[tauri::command]
async fn integrity_list(app: AppHandle) -> Vec<SnapshotInfo> {
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().integrity.list().iter().map(snapshot_info).collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
fn integrity_delete(state: State<'_, AppState>, snapshot: String) -> Result<(), String> {
    state.integrity.delete(&snapshot)
}

// ---------------------------------------------------- operations, undo

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanEntry {
    id: String,
    name: String,
    path: String,
    kind: &'static str,
    bytes: u64,
    files: u64,
    /// Refused (by the mutation policy or because it's gone), with the reason.
    blocked: Option<String>,
    note: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationPlan {
    op: String,
    entries: Vec<PlanEntry>,
    total_bytes: u64,
    total_files: u64,
    blocked: usize,
    /// Large or folder deletions must be confirmed by typing DELETE.
    needs_typed_confirm: bool,
    /// `None`: Secure Overwrite is meaningful here; otherwise why it isn't.
    overwrite_unavailable: Option<String>,
}

const TYPED_CONFIRM_ITEMS: usize = 25;
const TYPED_CONFIRM_FILES: u64 = 100;
const TYPED_CONFIRM_BYTES: u64 = 1024 * 1024 * 1024;

fn plan(state: &AppState, op: &str, ids: &[String]) -> Result<(OperationPlan, Vec<PathBuf>), String> {
    let pop = match op {
        "trash" => policy::Op::Trash,
        "delete" | "overwrite" => policy::Op::Delete,
        _ => return Err("Unknown operation".into()),
    };
    let policy = state.policy();
    let idx = state.index();
    let mut entries = Vec::new();
    let mut paths = Vec::new();
    let mut vols: HashMap<String, Option<String>> = HashMap::new();
    for id in ids.iter().take(100_000) {
        let Ok(loc) = state.locate(id) else { continue };
        let shown = secure::display_safe(&loc.rel);
        let name = secure::display_safe(&loc.name);
        let (path, meta) = match fileops::confined_item(&loc.root, &loc.rel) {
            Ok(v) => v,
            Err(e) => {
                entries.push(PlanEntry {
                    id: id.clone(),
                    name,
                    path: shown,
                    kind: "file",
                    bytes: 0,
                    files: 0,
                    blocked: Some(e),
                    note: None,
                });
                paths.push(PathBuf::new());
                continue;
            }
        };
        let ft = meta.file_type();
        let (kind, bytes, files) = if ft.is_symlink() {
            ("link", 0, 0)
        } else if ft.is_dir() {
            let prefix = format!("{}/", loc.rel);
            let (b, n) =
                idx.files.iter().filter(|e| e.path.starts_with(&prefix)).fold((0, 0), |(b, n), e| (b + e.size, n + 1));
            ("folder", b, n)
        } else {
            ("file", meta.len(), 1)
        };
        let mut blocked = policy.check(pop, &path).err();
        if blocked.is_none() && op == "overwrite" {
            blocked = policy.check(policy::Op::Overwrite, &path).err();
        }
        let note = (kind == "link")
            .then(|| "Only the link itself is removed; what it points to is never touched.".to_string());
        let vol = drives::key_of(&privacy::volume_of(&path));
        vols.entry(vol).or_insert_with(|| overwrite::eligibility(&path).err());
        entries.push(PlanEntry { id: id.clone(), name, path: shown, kind, bytes, files, blocked, note });
        paths.push(path);
    }
    let ok = || entries.iter().filter(|e| e.blocked.is_none());
    let total_bytes = ok().map(|e| e.bytes).sum();
    let total_files = ok().map(|e| e.files).sum();
    let blocked = entries.iter().filter(|e| e.blocked.is_some()).count();
    let needs_typed_confirm = op != "trash"
        && (entries.len() > TYPED_CONFIRM_ITEMS
            || total_files > TYPED_CONFIRM_FILES
            || total_bytes > TYPED_CONFIRM_BYTES
            || ok().any(|e| e.kind == "folder"));
    let overwrite_unavailable =
        if vols.is_empty() { Some("Nothing to overwrite.".to_string()) } else { vols.into_values().flatten().next() };
    Ok((
        OperationPlan {
            op: op.to_owned(),
            entries,
            total_bytes,
            total_files,
            blocked,
            needs_typed_confirm,
            overwrite_unavailable,
        },
        paths,
    ))
}

/// A dry run: what an operation would do to each item, and what the
/// mutation policy refuses — nothing is changed.
#[tauri::command]
async fn plan_operation(app: AppHandle, op: String, ids: Vec<String>) -> Result<OperationPlan, String> {
    tauri::async_runtime::spawn_blocking(move || plan(&app.state::<AppState>(), &op, &ids).map(|p| p.0))
        .await
        .map_err(|_| "The plan couldn't be made.".to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteResult {
    deleted: Vec<String>,
    bytes: u64,
    failed: Vec<dupes::Failure>,
}

/// Delete permanently (no Trash), optionally overwriting file contents first
/// where that is meaningful. Re-plans and re-checks everything here; large
/// batches and folders require the typed confirmation "DELETE".
#[tauri::command]
async fn delete_items(
    app: AppHandle,
    ids: Vec<String>,
    overwrite: bool,
    confirm: String,
) -> Result<DeleteResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (plan, paths) = plan(&state, if overwrite { "overwrite" } else { "delete" }, &ids)?;
        if plan.needs_typed_confirm && confirm != "DELETE" {
            return Err("Type DELETE to confirm this permanent deletion.".into());
        }
        if overwrite {
            if let Some(why) = &plan.overwrite_unavailable {
                return Err(format!("Secure Overwrite isn't available here: {why}"));
            }
        }
        let policy = state.policy();
        let mut out = DeleteResult { deleted: Vec::new(), bytes: 0, failed: Vec::new() };
        let mut rels = Vec::new();
        let mut gone = Vec::new();
        let mut changes = Vec::new();
        for (e, path) in plan.entries.iter().zip(paths) {
            if let Some(b) = &e.blocked {
                out.failed.push(dupes::Failure { path: e.path.clone(), reason: b.clone() });
                continue;
            }
            if let Ok(m) = fs::symlink_metadata(&path) {
                if m.is_file() {
                    invalidate_thumbs(&state, &path, &m);
                }
            }
            let r = if overwrite && e.kind != "link" {
                overwrite_tree(&policy, &path)
            } else {
                fileops::delete_permanently(&policy, &path)
            };
            match r {
                Ok(b) => {
                    out.deleted.push(e.id.clone());
                    out.bytes += b;
                    if let Some(rel) = browser_rel(&state, &path) {
                        rels.push(rel);
                    }
                    changes.push(history::Change::Deleted { path: path.clone() });
                    gone.push(path);
                }
                Err(reason) => out.failed.push(dupes::Failure { path: e.path.clone(), reason }),
            }
        }
        if !rels.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&rels));
        }
        forget_removed(&state, &gone);
        let n = changes.len();
        state.history.record(
            format!(
                "{} {n} item{} permanently",
                if overwrite { "Overwrote and deleted" } else { "Deleted" },
                if n == 1 { "" } else { "s" }
            ),
            changes,
        );
        Ok(out)
    })
    .await
    .map_err(|_| "The deletion stopped unexpectedly.".to_string())?
}

/// Overwrite every regular file under `path` (never through a link), then
/// remove what remains (folders, links).
fn overwrite_tree(policy: &policy::Policy, path: &Path) -> Result<u64, String> {
    let meta = fs::symlink_metadata(path).map_err(|_| "file no longer exists".to_string())?;
    if meta.is_file() {
        return overwrite::overwrite_and_delete(policy, path);
    }
    let mut bytes = 0;
    for e in walkdir::WalkDir::new(path).follow_links(false).into_iter() {
        let e = e.map_err(|_| "a folder couldn't be read".to_string())?;
        if e.file_type().is_file() {
            bytes += overwrite::overwrite_and_delete(policy, e.path())?;
        }
    }
    fileops::delete_permanently(policy, path)?;
    Ok(bytes)
}

#[tauri::command]
fn history_list(state: State<'_, AppState>) -> Vec<history::RecordView> {
    state.history.list()
}

/// Undo one operation (`None` = the most recent one that can be undone).
#[tauri::command]
async fn history_undo(app: AppHandle, id: Option<u64>) -> Result<history::UndoOutcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let id = id.or_else(|| state.history.last_undoable()).ok_or("Nothing to undo.")?;
        let out = state.history.undo(id, &state.policy())?;
        // Renamed or moved back: Mori's own records follow the item again.
        for (now, back) in &out.moved_back {
            for s in [&state.privacy, &state.favorites, &state.capture_not, &state.capture_yes] {
                s.renamed(now, back);
            }
            state.tags.renamed(now, back);
        }
        // Restored or renamed items: let the index pick them up.
        if out.touched.iter().any(|p| state.root_canon().is_some_and(|r| p.starts_with(r))) {
            start_scan(&app);
        }
        Ok(out)
    })
    .await
    .map_err(|_| "Undo failed.".to_string())?
}

// ------------------------------------------------------- favorites, tags

/// Canonical paths of browser items (files and folders, never through a link).
fn browser_paths(state: &AppState, ids: &[String]) -> Result<Vec<PathBuf>, String> {
    let root = state.root_canon().ok_or("No folder selected")?;
    let idx = state.index();
    ids.iter()
        .take(10_000)
        .map(|id| {
            let e = idx.get(id).ok_or("Unknown item")?;
            if e.kind == index::Kind::Link
                || Path::new(&e.path).components().any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err("Links can't be tagged".to_string());
            }
            Ok(root.join(&e.path))
        })
        .collect()
}

/// Add or remove items from Favorites. Only Mori's records change.
#[tauri::command]
fn set_favorite(app: AppHandle, state: State<'_, AppState>, ids: Vec<String>, on: bool) -> Result<(), String> {
    state.persistent()?;
    for p in browser_paths(&state, &ids)? {
        state.favorites.set(&p, 0, on)?;
    }
    publish_index(&app, (*state.index()).clone());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TagInfo {
    id: u32,
    name: String,
    /// Items with this tag on the browsed drive (outside private folders).
    count: usize,
}

#[tauri::command]
fn tags_list(state: State<'_, AppState>) -> Vec<TagInfo> {
    let idx = state.index();
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for e in idx.files.iter().chain(idx.dirs.iter()).filter(|e| e.visible_from("")) {
        for t in &e.tags {
            *counts.entry(*t).or_default() += 1;
        }
    }
    state
        .tags
        .list()
        .into_iter()
        .map(|t| TagInfo {
            count: counts.get(&t.id).copied().unwrap_or(0),
            id: t.id,
            name: secure::display_safe(&t.name),
        })
        .collect()
}

/// Add (`on`) or remove the tag called `name` on items. Never touches the files.
#[tauri::command]
fn tag_items(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Vec<String>,
    name: String,
    on: bool,
) -> Result<u32, String> {
    state.persistent()?;
    let paths = browser_paths(&state, &ids)?;
    let id = state.tags.ensure(&name)?;
    state.tags.assign(&paths, id, on)?;
    publish_index(&app, (*state.index()).clone());
    Ok(id)
}

#[tauri::command]
fn tag_rename(app: AppHandle, state: State<'_, AppState>, id: u32, name: String) -> Result<(), String> {
    state.persistent()?;
    state.tags.rename(id, &name)?;
    publish_index(&app, (*state.index()).clone());
    Ok(())
}

/// Delete a tag everywhere (the tagged files are not touched).
#[tauri::command]
fn tag_delete(app: AppHandle, state: State<'_, AppState>, id: u32) -> Result<(), String> {
    state.persistent()?;
    state.tags.delete(id)?;
    publish_index(&app, (*state.index()).clone());
    Ok(())
}

// --------------------------------------------- sessions and forgetting

/// Browse a folder without leaving anything behind: no index cache, no
/// thumbnails on disk, no remembered folder, no records. Everything stays
/// in memory and is gone when another folder is opened or Mori quits.
#[tauri::command]
async fn open_temporary(app: AppHandle) -> Result<Status, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title("Temporary Session").blocking_pick_folder();
    // The system folder picker remembers the last folder in Mori's preferences.
    forget_open_panel_location();
    let path = picked.ok_or("cancelled")?.into_path().map_err(|_| "That folder can't be opened")?;
    let state = app.state::<AppState>();
    begin_session(&state, false);
    open_root(&app, &path)?;
    Ok(status(&state))
}

fn begin_session(state: &AppState, private: bool) {
    clear_session(state);
    state.temp.store(true, Ordering::SeqCst);
    state.private_inspection.store(private, Ordering::SeqCst);
    state.session_read_only.store(private, Ordering::SeqCst);
    state.session_safe.store(private, Ordering::SeqCst);
}

/// Private Inspection: a temporary session with read-only access and Safe
/// Inspection Mode forced on — for a newly connected drive (`key`) or a
/// folder chosen in the picker. The drive is not remembered.
#[tauri::command]
async fn start_private_inspection(app: AppHandle, key: Option<String>) -> Result<Status, String> {
    use tauri_plugin_dialog::DialogExt;
    let state = app.state::<AppState>();
    let path = match key {
        Some(k) => state
            .connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&k)
            .map(|(p, _)| p.clone())
            .ok_or("That drive is no longer connected.")?,
        None => {
            let picked = app.dialog().file().set_title("Private Inspection").blocking_pick_folder();
            forget_open_panel_location();
            picked.ok_or("cancelled")?.into_path().map_err(|_| "That folder can't be opened")?
        }
    };
    begin_session(&state, true);
    open_root(&app, &path)?;
    Ok(status(&state))
}

/// Remove the "last folder" macOS's folder picker stores in Mori's own
/// preferences, so the picked location isn't remembered.
fn forget_open_panel_location() {
    #[cfg(target_os = "macos")]
    objc2::rc::autoreleasepool(|_| unsafe {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let d: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        for key in [c"NSOSPLastRootDirectory", c"NSNavLastRootDirectory"] {
            let k: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: key.as_ptr()];
            if !d.is_null() && !k.is_null() {
                let _: () = msg_send![d, removeObjectForKey: k];
            }
        }
    });
}

/// Leave a temporary session and go back to the last saved folder.
#[tauri::command]
fn end_temporary(app: AppHandle, state: State<'_, AppState>) -> Result<Status, String> {
    if !state.temp.load(Ordering::SeqCst) {
        return Ok(status(&state));
    }
    // Cancel work, drop every in-memory result of the session, then leave it.
    clear_session(&state);
    state.scan_gen.fetch_add(1, Ordering::SeqCst);
    state.temp.store(false, Ordering::SeqCst);
    state.private_inspection.store(false, Ordering::SeqCst);
    state.session_read_only.store(false, Ordering::SeqCst);
    state.session_safe.store(false, Ordering::SeqCst);
    *state.root.write().unwrap_or_else(PoisonError::into_inner) = None;
    publish_index(&app, Index::default());
    forget_open_panel_location();
    let saved = read_settings(&state).root.map(PathBuf::from);
    match saved {
        Some(p) if open_root(&app, &p).is_ok() => {}
        _ => {
            *state.root.write().unwrap_or_else(PoisonError::into_inner) = None;
            publish_index(&app, Index::default());
        }
    }
    Ok(status(&state))
}

/// Remove a file or folder Mori created in its own app directories. Refuses
/// anything else, so nothing on a user's drive can ever be deleted here.
fn remove_app_path(state: &AppState, p: &Path) {
    let cache = state.thumb_dir.parent().unwrap_or(&state.thumb_dir);
    if !inside_app_dirs(p, &state.data_dir, cache) {
        return;
    }
    if p.is_dir() {
        let _ = fs::remove_dir_all(p);
    } else {
        let _ = fs::remove_file(p);
    }
}

/// Strictly inside one of Mori's own directories (never the directory
/// itself, never through `..`).
fn inside_app_dirs(p: &Path, data: &Path, cache: &Path) -> bool {
    !p.components().any(|c| matches!(c, Component::ParentDir))
        && [data, cache].iter().any(|d| p.starts_with(d) && p != *d && d.components().count() > 2)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Forgotten {
    drive: String,
    indexes: usize,
}

/// "Forget this drive": remove everything Mori knows about the current
/// drive — its index caches, thumbnails, and its private, protected,
/// favorite, tag and screenshot records — then close it. Nothing on the
/// drive itself is read for this or changed.
#[tauri::command]
fn forget_drive(app: AppHandle, state: State<'_, AppState>) -> Result<Forgotten, String> {
    let root = state.root_canon().ok_or("No folder selected")?;
    let forgotten = forget_data(&state, &root);
    clear_session(&state);
    *state.root.write().unwrap_or_else(PoisonError::into_inner) = None;
    write_settings(&state, |s| s.root = None)?;
    publish_index(&app, Index::default());
    emit_status(&app);
    Ok(forgotten)
}

/// Remove Mori's own data about the volume holding `root`.
fn forget_data(state: &AppState, root: &Path) -> Forgotten {
    let vol = privacy::volume_of(root);
    let drive = drive_of(root);
    state.scan_gen.fetch_add(1, Ordering::SeqCst);
    // Index caches of any folder on this volume (the cache records its root).
    let mut indexes = 0;
    if let Ok(rd) = fs::read_dir(state.index_dir()) {
        for f in rd.flatten() {
            let p = f.path();
            let on_volume = fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v.get("root").and_then(|r| r.as_str()).map(PathBuf::from))
                .is_some_and(|r| {
                    r.starts_with(&vol.mount) && drives::key_of(&privacy::volume_of(&r)) == drives::key_of(&vol)
                });
            if on_volume {
                remove_app_path(state, &p);
                indexes += 1;
            }
        }
    }
    remove_app_path(state, &state.thumb_dir_for(root));
    for store in [&state.privacy, &state.protected, &state.favorites, &state.capture_not, &state.capture_yes] {
        store.forget_volume(&vol);
    }
    state.tags.forget_volume(&vol);
    state.cleanup.forget_volume(&format!("{:016x}", thumbs::fnv(drives::key_of(&vol).as_bytes())));
    state.drives.forget(&drives::key_of(&vol));
    Forgotten { drive, indexes }
}

/// Debug builds only: `MORI_DEBUG_OPS=<scratch folder>` builds a small tree
/// in that folder and exercises the operation commands end to end (plan,
/// typed confirmation, overwrite refusal, Trash and Undo, permanent delete).
fn debug_ops(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_OPS").map(PathBuf::from) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Album/Keep")).unwrap();
        fs::write(dir.join("Album/a.jpg"), vec![1u8; 2000]).unwrap();
        fs::write(dir.join("Album/Keep/b.jpg"), vec![2u8; 3000]).unwrap();
        fs::write(dir.join("loose.txt"), b"loose").unwrap();
        std::thread::sleep(Duration::from_secs(2));
        open_root(&app, &dir).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        while state.scanning.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let id = |rel: &str| index::id_str(index::id_for(rel));
        state.protected.set(&dir.join("Album/Keep"), 0, true).unwrap();
        let (p, _) = plan(&state, "delete", &[id("Album"), id("loose.txt")]).unwrap();
        eprintln!(
            "mori: DEBUG ops plan: entries={} blocked={:?} typed={} overwrite={:?}",
            p.entries.len(),
            p.entries.iter().map(|e| (e.path.clone(), e.blocked.clone())).collect::<Vec<_>>(),
            p.needs_typed_confirm,
            p.overwrite_unavailable
        );
        state.protected.set(&dir.join("Album/Keep"), 0, false).unwrap();
        let no_confirm =
            tauri::async_runtime::block_on(delete_items(app.clone(), vec![id("Album")], false, String::new()));
        let overwrite =
            tauri::async_runtime::block_on(delete_items(app.clone(), vec![id("loose.txt")], true, String::new()));
        eprintln!(
            "mori: DEBUG ops refusals: no-confirm={:?} overwrite={:?} album-still-there={}",
            no_confirm.err(),
            overwrite.err(),
            dir.join("Album/a.jpg").exists()
        );
        let t = tauri::async_runtime::block_on(trash_items(app.clone(), vec![id("loose.txt")])).unwrap();
        let gone = !dir.join("loose.txt").exists();
        let u = tauri::async_runtime::block_on(history_undo(app.clone(), None));
        eprintln!(
            "mori: DEBUG ops trash+undo: trashed={} gone={gone} undo={:?} back={}",
            t.trashed.len(),
            u.map(|o| (o.restored, o.failed)),
            fs::read(dir.join("loose.txt")).is_ok_and(|b| b == b"loose")
        );
        let d = tauri::async_runtime::block_on(delete_items(app.clone(), vec![id("Album")], false, "DELETE".into()))
            .unwrap();
        let hist = state.history.list();
        eprintln!(
            "mori: DEBUG ops delete: deleted={} bytes={} album-gone={} history={:?}",
            d.deleted.len(),
            d.bytes,
            !dir.join("Album").exists(),
            hist.iter().map(|h| (h.label.clone(), h.undoable, h.note.clone())).collect::<Vec<_>>()
        );
        eprintln!("mori: DEBUG ops done");
    });
}

/// Debug builds only, for `tests/transfer.rs`: `MORI_DEBUG_TRANSFER=<scratch
/// folder>` (and `MORI_DEBUG_TRANSFER_OUT=<folder outside it>`) builds a
/// small tree and runs Move / Copy, conflicts, Replace + Undo, refusals,
/// partial failure and the Quick Cleanup store through the real commands,
/// printing one line per step, then quits.
fn debug_transfer(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_TRANSFER").map(PathBuf::from) else { return };
    let out_dir = std::env::var_os("MORI_DEBUG_TRANSFER_OUT").map(PathBuf::from);
    let app = app.clone();
    std::thread::spawn(move || {
        if catch_unwind(AssertUnwindSafe(|| debug_transfer_steps(&app, &dir, out_dir.as_deref()))).is_err() {
            eprintln!("mori: DEBUG transfer failed");
            app.exit(1);
        }
    });
}

fn debug_transfer_steps(app: &AppHandle, dir: &Path, out_dir: Option<&Path>) {
    {
        let app = app.clone();
        let state = app.state::<AppState>();
        let _ = fs::remove_dir_all(dir);
        for d in ["Album/Sub", "Other", "Guarded", "Private"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        fs::write(dir.join("Album/a.jpg"), b"album-a").unwrap();
        fs::write(dir.join("Album/b.jpg"), b"album-b").unwrap();
        fs::write(dir.join("Album/Sub/c.jpg"), b"sub-c").unwrap();
        fs::write(dir.join("Other/a.jpg"), b"other-a").unwrap();
        fs::write(dir.join("Other/x.txt"), b"x").unwrap();
        fs::write(dir.join("Guarded/g.jpg"), b"g").unwrap();
        fs::write(dir.join("Private/p.jpg"), b"p").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("Album/a.jpg"), dir.join("link.jpg")).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let dir = fs::canonicalize(dir).unwrap();
        open_root(&app, &dir).unwrap();
        let settle = || {
            std::thread::sleep(Duration::from_millis(300));
            while state.scanning.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        settle();
        state.protected.set(&dir.join("Guarded"), 0, true).unwrap();
        state.privacy.set(&dir.join("Private"), 0, true).unwrap();
        publish_index(&app, (*state.index()).clone());
        let id = |rel: &str| index::id_str(index::id_for(rel));
        let has = |rel: &str| state.index().get(&id(rel)).is_some();
        let read = |rel: &str| fs::read_to_string(dir.join(rel)).unwrap_or_else(|_| "-".into());
        let run = |op: &str, ids: &[&str], dest: &str, res: &[(&str, &str)]| {
            let ids = ids.iter().map(|r| id(r)).collect();
            let res = res.iter().map(|(r, c)| (id(r), c.to_string())).collect();
            tauri::async_runtime::block_on(transfer_items(app.clone(), op.into(), ids, dest.into(), res))
        };

        // 1. Plans: conflict + Replace offered, protected source, into itself, already there.
        let (p, _, _) =
            transfer_plan(&state, "move", &[id("Album/a.jpg"), id("Album/b.jpg"), id("Guarded/g.jpg")], &id("Other"))
                .unwrap();
        let e = |n: usize| &p.entries[n];
        eprintln!(
            "mori: DEBUG transfer plan: conflict={:?} replaceable={} free={:?} guarded-blocked={}",
            e(0).conflict,
            e(0).replaceable,
            e(1).conflict,
            e(2).blocked.as_deref().is_some_and(|b| b.contains("protected"))
        );
        let (p, _, _) = transfer_plan(&state, "move", &[id("Album")], &id("Album/Sub")).unwrap();
        let (q, _, _) = transfer_plan(&state, "move", &[id("Album/b.jpg")], &id("Album")).unwrap();
        eprintln!(
            "mori: DEBUG transfer refusals: into-itself={} already-here={}",
            p.entries[0].blocked.as_deref().is_some_and(|b| b.contains("into itself")),
            q.entries[0].blocked.as_deref() == Some(ALREADY_HERE)
        );

        // 2. Move without resolving the conflict: the conflicting item is skipped, never overwritten.
        let r = run("move", &["Album/a.jpg", "Album/b.jpg"], &id("Other"), &[]).unwrap();
        eprintln!(
            "mori: DEBUG transfer move: done={} skipped={} other-a={} moved-b={} index-old={} index-new={}",
            r.done.len(),
            r.skipped,
            read("Other/a.jpg"),
            read("Other/b.jpg"),
            has("Album/b.jpg"),
            has("Other/b.jpg")
        );

        // 3. Favorites follow a move.
        state.favorites.set(&dir.join("Other/b.jpg"), 0, true).unwrap();
        publish_index(&app, (*state.index()).clone());
        run("move", &["Other/b.jpg"], &id("Album"), &[]).unwrap();
        eprintln!(
            "mori: DEBUG transfer favorite-follows={}",
            state.index().get(&id("Album/b.jpg")).is_some_and(|e| e.favorite)
        );

        // 4. Copy with Keep Both.
        let r = run("copy", &["Album/a.jpg"], &id("Other"), &[("Album/a.jpg", "keepBoth")]).unwrap();
        eprintln!(
            "mori: DEBUG transfer keep-both: done={} copy={} original={} indexed={}",
            r.done.len(),
            read("Other/a 2.jpg"),
            read("Album/a.jpg"),
            has("Other/a 2.jpg")
        );

        // 5. Replace (the existing file goes to the Trash), then Undo puts both back.
        let r = run("move", &["Album/a.jpg"], &id("Other"), &[("Album/a.jpg", "replace")]).unwrap();
        let replaced = read("Other/a.jpg");
        let u = tauri::async_runtime::block_on(history_undo(app.clone(), None)).unwrap();
        eprintln!(
            "mori: DEBUG transfer replace: done={} now={replaced} undo={} back-album={} back-other={}",
            r.done.len(),
            u.restored,
            read("Album/a.jpg"),
            read("Other/a.jpg")
        );
        settle();

        // 6. A folder copy, and a link copied as a link.
        let r = run("copy", &["Album"], &id("Other"), &[]).unwrap();
        #[cfg(unix)]
        let link = {
            run("copy", &["link.jpg"], &id("Other"), &[]).unwrap();
            fs::symlink_metadata(dir.join("Other/link.jpg")).is_ok_and(|m| m.file_type().is_symlink())
        };
        #[cfg(not(unix))]
        let link = true;
        settle();
        eprintln!(
            "mori: DEBUG transfer folder-copy: done={} nested={} source-kept={} indexed={} link-is-link={link}",
            r.done.len(),
            read("Other/Album/Sub/c.jpg"),
            read("Album/Sub/c.jpg"),
            has("Other/Album/Sub/c.jpg")
        );

        // 7. Read-only Mode and a protected destination refuse; nothing is created.
        state.read_only.store(true, Ordering::SeqCst);
        let ro = run("copy", &["Album/b.jpg"], &id("Other"), &[]).unwrap();
        state.read_only.store(false, Ordering::SeqCst);
        let pr = run("copy", &["Album/b.jpg"], &id("Guarded"), &[]).unwrap();
        eprintln!(
            "mori: DEBUG transfer policy: read-only={} protected={} created={}",
            ro.failed.first().is_some_and(|f| f.reason == policy::READ_ONLY),
            pr.failed.first().is_some_and(|f| f.reason.contains("protected")),
            dir.join("Guarded/b.jpg").exists() || dir.join("Other/b.jpg").exists()
        );

        // 8. Partial failure: one item vanished from disk after indexing.
        fs::remove_file(dir.join("Other/x.txt")).unwrap();
        let r = run("move", &["Other/x.txt", "Album/b.jpg"], &id("Album/Sub"), &[]).unwrap();
        eprintln!(
            "mori: DEBUG transfer partial: done={} failed={} moved={}",
            r.done.len(),
            r.failed.len(),
            read("Album/Sub/b.jpg")
        );

        // 9. A destination outside the browsed folder.
        if let Some(out) = out_dir {
            fs::create_dir_all(out).unwrap();
            state.transfer_dests.lock().unwrap_or_else(PoisonError::into_inner).push(fs::canonicalize(out).unwrap());
            let r = run("move", &["Album/Sub/c.jpg"], "d0", &[]).unwrap();
            eprintln!(
                "mori: DEBUG transfer outside: done={} new-id={} arrived={} left-index={}",
                r.done.len(),
                r.done.first().is_some_and(|d| d.new_id.is_some()),
                fs::read_to_string(out.join("c.jpg")).unwrap_or_default(),
                !has("Album/Sub/c.jpg")
            );
        }

        // 10. Quick Cleanup sessions: saved in normal mode, never in a temporary session.
        let session = cleanup::Session {
            folder: id("Album"),
            options: cleanup::Options { recursive: false, kind: "all".into(), order: "browser".into() },
            kept: vec![id("Album/a.jpg")],
            marked: vec![id("Album/Sub/b.jpg")],
            cursor: 2,
            saved_at: 0,
        };
        let saved = cleanup_save(app.state::<AppState>(), session.clone()).is_ok();
        let resumed = cleanup_saved(app.state::<AppState>(), id("Album")).is_some_and(|s| s.marked == session.marked);
        cleanup_discard(app.state::<AppState>(), id("Album"));
        let discarded = cleanup_saved(app.state::<AppState>(), id("Album")).is_none();
        begin_session(&state, false);
        open_root(&app, &dir).unwrap();
        settle();
        let temp_save = cleanup_save(app.state::<AppState>(), session.clone());
        let temp_read = cleanup_saved(app.state::<AppState>(), id("Album"));
        eprintln!(
            "mori: DEBUG transfer cleanup-store: saved={saved} resumed={resumed} discarded={discarded} temp-refused={} temp-none={}",
            temp_save.is_err(),
            temp_read.is_none()
        );
        let _ = end_temporary(app.clone(), app.state::<AppState>());
        let files_left = walkdir::WalkDir::new(&dir).into_iter().flatten().filter(|e| e.file_type().is_file()).count();
        eprintln!("mori: DEBUG transfer done files={files_left}");
        app.exit(0);
    }
}

/// Debug builds only, for `tests/ephemeral.rs`:
/// - `MORI_DEBUG_QUIT=1` quits right after startup (baseline run);
/// - `MORI_DEBUG_EPHEMERAL=<folder>` runs a temporary session over that
///   folder exercising every read path (browse, search, thumbnails,
///   previews, PDF pages, metadata, checksums, archives), tries the writes
///   a session must refuse, ends the session and quits.
fn debug_ephemeral(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    if std::env::var_os("MORI_DEBUG_FORGET_PANEL").is_some() {
        forget_open_panel_location();
        eprintln!("mori: DEBUG panel location forgotten");
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            app.exit(0);
        });
        return;
    }
    if std::env::var_os("MORI_DEBUG_QUIT").is_some() {
        let app = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(3));
            eprintln!("mori: DEBUG ephemeral baseline done");
            app.exit(0);
        });
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_EPHEMERAL").map(PathBuf::from) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let state = app.state::<AppState>();
        let mut log: Vec<String> = Vec::new();
        begin_session(&state, true);
        open_root(&app, &dir).expect("open");
        std::thread::sleep(Duration::from_millis(300));
        while state.scanning.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let st = status(&state);
        log.push(format!("private start: safe={} read_only={} temporary={}", st.safe_mode, st.read_only, st.temporary));
        // Explicit previews for the session (as the banner's button would).
        state.session_safe.store(false, Ordering::SeqCst);
        state.safe_mode.store(false, Ordering::SeqCst);
        let idx = state.index();
        let found = index::query(
            &idx,
            &Query { scope: "library".into(), kind: "all".into(), search: "passport".into(), ..Default::default() },
        )
        .items
        .len();
        log.push(format!("indexed={} search-hits={found}", idx.files.len()));
        let files: Vec<(String, String)> = idx.files.iter().map(|e| (index::id_str(e.id), e.ext.clone())).collect();
        drop(idx);
        let get = |route: &str| {
            let req =
                tauri::http::Request::builder().uri(format!("mori://localhost/{route}")).body(Vec::new()).unwrap();
            protocol::handle(&app, req).status().as_u16()
        };
        for (id, ext) in &files {
            match ext.as_str() {
                "jpg" | "png" => {
                    log.push(format!(
                        "thumb={} preview={} iso={}",
                        get(&format!("thumb/{id}")),
                        get(&format!("preview/{id}")),
                        get(&format!("iso-preview/{id}"))
                    ));
                    log.push(format!(
                        "metadata={}",
                        tauri::async_runtime::block_on(metascan::file_metadata(app.clone(), id.clone())).is_ok()
                    ));
                }
                "pdf" => {
                    log.push(format!(
                        "pdf-info={} pdf-page={}",
                        tauri::async_runtime::block_on(pdf_info(app.clone(), id.clone(), true)).is_ok(),
                        get(&format!("iso-pdf/{id}/1/400"))
                    ));
                }
                "zip" => log.push(format!(
                    "archive={}",
                    tauri::async_runtime::block_on(archive_listing(app.clone(), id.clone())).is_ok()
                )),
                _ => {}
            }
            log.push(format!(
                "checksum={}",
                tauri::async_runtime::block_on(checksum_file(app.clone(), id.clone())).is_ok()
            ));
        }
        let first = files.first().map(|f| f.0.clone()).unwrap_or_default();
        log.push(format!(
            "refused: favorite={} snapshot={} tag={} open={}",
            set_favorite(app.clone(), app.state(), vec![first.clone()], true).is_err(),
            integrity_save(app.clone(), app.state(), first.clone()).is_err(),
            tag_items(app.clone(), app.state(), vec![first.clone()], "secret-tag".into(), true).is_err(),
            tauri::async_runtime::block_on(open_file(app.state(), first.clone())).is_err(),
        ));
        let rename_refused = rename_item(app.clone(), app.state(), first.clone(), "renamed.jpg".into()).is_err();
        log.push(format!("read-only rename refused={rename_refused}"));
        let _ = end_temporary(app.clone(), app.state());
        log.push(format!(
            "after end: temporary={} root={}",
            state.temp.load(Ordering::SeqCst),
            state.root_canon().is_some()
        ));
        for l in log {
            eprintln!("mori: DEBUG ephemeral {l}");
        }
        eprintln!("mori: DEBUG ephemeral done");
        std::thread::sleep(Duration::from_millis(500));
        app.exit(0);
    });
}

/// Debug builds only: `MORI_DEBUG_ORG=<folder>` checks on the real app that a
/// temporary session writes nothing, and that forgetting the folder's drive
/// removes only Mori's data (the folder itself is compared before and after).
/// Settings are never written.
fn debug_org(app: &AppHandle) {
    if !cfg!(debug_assertions) {
        return;
    }
    let Some(dir) = std::env::var_os("MORI_DEBUG_ORG").map(PathBuf::from) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<AppState>();
        let snap = |base: &Path| -> std::collections::BTreeMap<String, (u64, u128)> {
            walkdir::WalkDir::new(base)
                .follow_links(false)
                .into_iter()
                .flatten()
                .filter(|e| e.file_type().is_file())
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    Some((e.path().to_string_lossy().into_owned(), (m.len(), dupes::mtime_ns(&m))))
                })
                .collect()
        };
        let cache = state.thumb_dir.parent().unwrap_or(&state.thumb_dir).to_path_buf();
        let wait_scan = || {
            std::thread::sleep(Duration::from_millis(300));
            while state.scanning.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        std::thread::sleep(Duration::from_secs(2));
        wait_scan();
        let canon = fs::canonicalize(&dir).unwrap();
        let _ = fs::remove_file(state.index_file(&canon));
        let (data0, cache0, drive0) = (snap(&state.data_dir), snap(&cache), snap(&canon));
        state.temp.store(true, Ordering::SeqCst);
        open_root(&app, &canon).unwrap();
        wait_scan();
        let files = state.index().files.len();
        let refused = state.persistent().is_err();
        let (data1, cache1) = (snap(&state.data_dir), snap(&cache));
        let diff = |a: &std::collections::BTreeMap<String, (u64, u128)>,
                    b: &std::collections::BTreeMap<String, (u64, u128)>| {
            b.iter().filter(|(k, v)| a.get(*k) != Some(v)).map(|(k, _)| k.clone()).collect::<Vec<_>>()
        };
        eprintln!("mori: DEBUG org temp: indexed {files} files; app-data changes={:?} cache changes={:?} records-refused={refused}", diff(&data0, &data1), diff(&cache0, &cache1));
        state.temp.store(false, Ordering::SeqCst);
        open_root(&app, &canon).unwrap();
        wait_scan();
        let saved = state.index_file(&canon).exists();
        let first = state.index().files.first().map(|e| canon.join(&e.path));
        if let Some(f) = &first {
            state.favorites.set(f, 0, true).unwrap();
            let t = state.tags.ensure("debug-forget").unwrap();
            state.tags.assign(std::slice::from_ref(f), t, true).unwrap();
        }
        let before_forget = (state.favorites.boundaries(&canon).len(), state.tags.for_root(&canon).len());
        let r = forget_data(&state, &canon);
        let after = (
            state.favorites.boundaries(&canon).len(),
            state.tags.for_root(&canon).len(),
            state.index_file(&canon).exists(),
            state.thumb_dir_for(&canon).exists(),
        );
        let drive1 = snap(&canon);
        if let Some(t) = state.tags.list().iter().find(|t| t.name == "debug-forget") {
            let _ = state.tags.delete(t.id);
        }
        eprintln!(
            "mori: DEBUG org normal: index saved={saved}; before forget favorites/tags={before_forget:?}; forgot {} indexes; after (favorites, tags, index exists, thumbs exist)={after:?}; drive unchanged={}",
            r.indexes,
            drive0 == drive1
        );
        eprintln!("mori: DEBUG org done");
    });
}

fn clear_session(state: &AppState) {
    state.analysis_cancel.store(true, Ordering::SeqCst);
    state.similar_cancel.store(true, Ordering::SeqCst);
    state.meta_job.cancel();
    state.health_job.cancel();
    *state.analysis.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *state.similar.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *state.meta_scan.lock().unwrap_or_else(PoisonError::into_inner) = None;
    *state.health.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.custom_locations.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.transfer_dests.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.connected.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.history.clear();
    state.checksums.clear();
    state.integrity_job.cancel();
    state.video.clear_session_probes();
    state.volatile_thumbs.clear();
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
}

/// "Clear Session Data": forget what this session holds in memory — analysis
/// results, folders picked for analysis, in-memory previews. Unlike Clear
/// Cache it keeps thumbnails and indexes; unlike Forget Drive it keeps every
/// record; it never touches files.
#[tauri::command]
fn clear_session_data(state: State<'_, AppState>) {
    clear_session(&state);
}

// ---------------------------------------------------------------- storage

/// Where the space goes on the browsed drive (from the index; nothing is read).
#[tauri::command]
fn storage_report(state: State<'_, AppState>, folder: String) -> Result<storage::Report, String> {
    let idx = state.index();
    let rel = idx.folder_path(&folder).ok_or("Unknown folder")?.to_owned();
    Ok(storage::report(&idx, &rel))
}

/// Folders without files, each re-checked on disk (hidden items count).
#[tauri::command]
async fn empty_folders(app: AppHandle) -> Result<Vec<storage::EmptyFolder>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let root = state.root_canon().ok_or("No folder selected")?;
        Ok(storage::empty_folders(&state.index(), &root))
    })
    .await
    .map_err(|_| "The check failed.".to_string())?
}

/// Move the chosen empty folders to the Trash, re-verifying each on disk
/// right before (never a permanent delete; protected folders are refused by
/// the mutation policy).
#[tauri::command]
async fn trash_empty_folders(app: AppHandle, ids: Vec<String>) -> Result<TrashResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let mut out = TrashResult { trashed: Vec::new(), bytes: 0, failed: Vec::new() };
        let mut removed = Vec::new();
        let mut changes = Vec::new();
        for id in ids.iter().take(10_000) {
            let Ok(loc) = state.locate(id) else { continue };
            let shown = secure::display_safe(&loc.rel);
            if !loc.is_dir {
                out.failed.push(dupes::Failure { path: shown, reason: "not a folder".into() });
                continue;
            }
            let path = match fileops::confined_item(&loc.root, &loc.rel) {
                Ok((p, m)) if m.is_dir() => p,
                Ok(_) => {
                    out.failed.push(dupes::Failure { path: shown, reason: "not a folder".into() });
                    continue;
                }
                Err(e) => {
                    out.failed.push(dupes::Failure { path: shown, reason: e });
                    continue;
                }
            };
            if let Err(e) = storage::verify_empty(&path) {
                out.failed.push(dupes::Failure { path: shown, reason: format!("no longer empty: {e}") });
                continue;
            }
            match fileops::move_to_trash(&state.policy(), &path) {
                Ok(trashed) => {
                    out.trashed.push(id.clone());
                    removed.push(loc.rel.clone());
                    changes.push(history::Change::Trashed { original: path.clone(), trashed });
                }
                Err(e) => out.failed.push(dupes::Failure { path: shown, reason: e }),
            }
        }
        if !removed.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&removed));
        }
        state.history.record(
            format!("Moved {} empty folder{} to Trash", changes.len(), if changes.len() == 1 { "" } else { "s" }),
            changes,
        );
        Ok(out)
    })
    .await
    .map_err(|_| "The operation failed.".to_string())?
}

// ------------------------------------------- screenshots and recordings

/// Correct the screenshot / screen-recording guess for one file:
/// "auto" (Mori's guess), "not" or "yes". Only Mori's view changes.
#[tauri::command]
fn set_capture_override(app: AppHandle, state: State<'_, AppState>, id: String, mode: String) -> Result<(), String> {
    state.persistent()?;
    let loc = state.locate(&id)?;
    if loc.is_dir || loc.is_link || !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Only files in the browsed folder can be corrected.".into());
    }
    let path = loc.root.join(&loc.rel);
    let (not, yes) = match mode.as_str() {
        "auto" => (false, false),
        "not" => (true, false),
        "yes" => (false, true),
        _ => return Err("Unknown choice".into()),
    };
    state.capture_not.set(&path, 0, not)?;
    state.capture_yes.set(&path, 0, yes)?;
    let idx = (*state.index()).clone();
    publish_index(&app, idx);
    Ok(())
}

// ------------------------------------------------- PDFs and archives

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct PdfInfo {
    pages: u32,
    encrypted: bool,
    locked: bool,
    title: Option<String>,
    author: Option<String>,
    creator: Option<String>,
    producer: Option<String>,
    subject: Option<String>,
    /// Present in the document (never run by Mori).
    javascript: bool,
    open_action: bool,
    embedded_files: bool,
    forms: bool,
}

/// Facts about a PDF, read by the sandboxed worker (macOS CoreGraphics).
#[tauri::command]
async fn pdf_info(app: AppHandle, id: String, explicit: bool) -> Result<PdfInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (mut file, meta, canon, ext) = state.open_by_id(&id)?;
        if !explicit
            && state.safe_mode.load(Ordering::SeqCst)
            && state.root_canon().is_some_and(|r| canon.starts_with(r))
        {
            return Err("Previews are off for this drive (Safe Inspection Mode).".to_string());
        }
        let head = secure::read_head(&mut file, secure::SNIFF_LEN);
        if secure::sniff(&head, &ext) != Detected::Pdf {
            return Err("Not a PDF".into());
        }
        let _ = std::io::Seek::rewind(&mut file);
        let bytes =
            secure::read_limited(file, &meta, worker::MAX_INPUT).map_err(|_| "This PDF is too large to inspect.")?;
        let out = worker::run(worker::Op::PdfInfo, 64, bytes, Duration::from_secs(15))
            .map_err(|_| "This PDF could not be read safely.")?;
        let text = String::from_utf8_lossy(&out.bytes);
        let mut info = PdfInfo::default();
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = secure::display_safe(v);
            match k {
                "pages" => info.pages = v.parse().unwrap_or(0),
                "encrypted" => info.encrypted = v == "true",
                "locked" => info.locked = v == "true",
                "title" => info.title = Some(v),
                "author" => info.author = Some(v),
                "creator" => info.creator = Some(v),
                "producer" => info.producer = Some(v),
                "subject" => info.subject = Some(v),
                "javascript" => info.javascript = v == "true",
                "openaction" => info.open_action = v == "true",
                "embedded" => info.embedded_files = v == "true",
                "forms" => info.forms = v == "true",
                _ => {}
            }
        }
        Ok(info)
    })
    .await
    .map_err(|_| "The PDF could not be read.".to_string())?
}

/// List an archive's contents without extracting anything (archive.rs).
#[tauri::command]
async fn archive_listing(app: AppHandle, id: String) -> Result<archive::Listing, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let (mut file, meta, _, _) = state.open_by_id(&id)?;
        let head = secure::read_head(&mut file, filetype::HEAD_LEN);
        let kind = filetype::detect(&head, meta.len()).id;
        catch_unwind(AssertUnwindSafe(|| archive::inspect(&mut file, meta.len(), kind)))
            .unwrap_or_else(|_| Err("The archive could not be read safely.".into()))
    })
    .await
    .map_err(|_| "The archive could not be read.".to_string())?
}

// ------------------------------------------------- safe inspection mode

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ConnectedDrive {
    key: String,
    label: String,
    path: String,
}

/// Watch for volumes mounted while Mori runs. Drives present at launch and
/// drives Mori already knows are never announced.
fn watch_drives(app: AppHandle) {
    std::thread::spawn(move || {
        let mut seen: HashSet<PathBuf> = drives::mounted().into_iter().collect();
        loop {
            std::thread::sleep(Duration::from_secs(3));
            let now: HashSet<PathBuf> = drives::mounted().into_iter().collect();
            let state = app.state::<AppState>();
            for path in now.difference(&seen) {
                let vol = privacy::volume_of(path);
                let key = drives::key_of(&vol);
                if state.drives.get(&key).is_some() || state.root_canon().is_some_and(|r| r.starts_with(path)) {
                    continue;
                }
                let label = path.file_name().map(|n| secure::display_safe(&n.to_string_lossy())).unwrap_or_default();
                state
                    .connected
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(key.clone(), (path.clone(), label.clone()));
                let _ = app.emit("drive-connected", ConnectedDrive { key, label, path: secure::plain_path(path) });
            }
            seen = now;
        }
    });
}

/// Open a newly connected drive in Safe Inspection Mode: metadata only,
/// no automatic previews until the user allows them.
#[tauri::command]
fn open_drive_safely(app: AppHandle, state: State<'_, AppState>, key: String) -> Result<Status, String> {
    let (path, label) = state
        .connected
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        .cloned()
        .ok_or("That drive is no longer connected.")?;
    state.temp.store(false, Ordering::SeqCst);
    state.drives.set(&key, &label, false);
    open_root(&app, &path)?;
    write_settings(&state, |s| s.root = Some(path.to_string_lossy().into_owned()))?;
    Ok(status(&state))
}

/// Allow (or stop) automatic previews for the current drive.
#[tauri::command]
fn set_drive_previews(app: AppHandle, state: State<'_, AppState>, on: bool) -> Result<(), String> {
    if state.temp.load(Ordering::SeqCst) {
        // For this session only: the drive's setting isn't remembered.
        state.session_safe.store(!on, Ordering::SeqCst);
        state.safe_mode.store(!on, Ordering::SeqCst);
        emit_status(&app);
        let _ = app.emit("index-changed", ());
        return Ok(());
    }
    let root = state.root_canon().ok_or("No folder selected")?;
    let vol = privacy::volume_of(&root);
    let label = vol.mount.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    state.drives.set(&drives::key_of(&vol), &label, on);
    state.safe_mode.store(!on, Ordering::SeqCst);
    emit_status(&app);
    let _ = app.emit("index-changed", ());
    Ok(())
}

// ---------------------------------------------------------- read-only mode

/// Read-only Mode is enforced by the mutation policy (policy.rs), not the UI.
#[tauri::command]
fn set_read_only(state: State<'_, AppState>, on: bool) -> Result<(), String> {
    write_settings(&state, |s| s.read_only = Some(on))?;
    state.read_only.store(on, Ordering::SeqCst);
    Ok(())
}

// --------------------------------------------------------- protected folders

/// Mark a folder "Never Modify" (or remove that). Mori state only: the
/// folder itself is not touched. Allowed in Read-only Mode.
#[tauri::command]
async fn set_folder_protected(app: AppHandle, id: String, protected: bool) -> Result<(), String> {
    app.state::<AppState>().persistent()?;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        if id.starts_with('x') || id.starts_with('y') {
            return Err("Choose the folder in the browser.".to_string());
        }
        let loc = state.locate(&id)?;
        if !loc.is_dir {
            return Err("Only folders can be protected.".into());
        }
        let (path, meta) = fileops::confined_item(&loc.root, &loc.rel)?;
        if !meta.is_dir() {
            return Err("Only folders can be protected.".into());
        }
        state.protected.set(&path, privacy::ino_of(&meta), protected)?;
        apply_index_change(&app, |_| {});
        Ok(())
    })
    .await
    .map_err(|_| "The operation failed.".to_string())?
}

// ---------------------------------------------------------- private folders

/// Make a browser folder private (a visibility boundary) or public again.
/// Only Mori's own records change; the folder itself is never touched.
#[tauri::command]
async fn set_folder_private(app: AppHandle, id: String, private: bool) -> Result<(), String> {
    app.state::<AppState>().persistent()?;
    tauri::async_runtime::spawn_blocking(move || set_private(&app, &id, private))
        .await
        .map_err(|_| "The operation failed.".to_string())?
}

fn set_private(app: &AppHandle, id: &str, private: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    if id.starts_with('x') || id.starts_with('y') {
        return Err("Choose the folder in the browser.".to_string());
    }
    let loc = state.locate(id)?;
    if !loc.is_dir {
        return Err("Only folders can be made private.".into());
    }
    let (path, meta) = fileops::confined_item(&loc.root, &loc.rel)?;
    if !meta.is_dir() {
        return Err("Only folders can be made private.".into());
    }
    state.privacy.set(&path, privacy::ino_of(&meta), private)?;
    if private {
        hide_from_analyses(&state, &path);
    }
    // Re-publishing applies the new boundaries to every view and count.
    apply_index_change(app, |_| {});
    Ok(())
}

/// Debug builds only: `MORI_DEBUG_PRIVACY=<folder relative to the root>`
/// with `MORI_DEBUG_PRIVACY_MODE=mark|unmark|report` changes or reports a
/// folder's privacy through the same path as the menu command and prints
/// what the views would show (testing without UI automation).
fn debug_privacy(app: &AppHandle) {
    let Ok(rel) = std::env::var("MORI_DEBUG_PRIVACY") else { return };
    let mode = std::env::var("MORI_DEBUG_PRIVACY_MODE").unwrap_or_else(|_| "report".into());
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let state = app.state::<AppState>();
        while state.scanning.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let report = |when: &str| {
            let idx = state.index();
            let s = index::stats(&idx);
            let lib = index::query(&idx, &Query { scope: "library".into(), kind: "all".into(), ..Default::default() });
            let inside = lib.items.iter().filter(|e| e.path.starts_with(&format!("{rel}/"))).count();
            let id = index::id_str(index::id_for(&rel));
            let parent = index::id_str(index::id_for(index::parent_of(&rel)));
            let rec = index::query(
                &idx,
                &Query { folder: parent, recursive: true, kind: "all".into(), ..Default::default() },
            );
            let own = index::query(&idx, &Query { folder: id.clone(), kind: "all".into(), ..Default::default() });
            let private = idx.get(&id).is_some_and(|e| e.private);
            eprintln!(
                "mori: DEBUG privacy {when}: private={private} files={} photos={} library-items-inside={inside} parent-recursive-inside={} own-folder-items={} private-scope={}",
                s.files,
                s.photo,
                rec.items.iter().filter(|e| e.path.starts_with(&format!("{rel}/"))).count(),
                own.items.len(),
                own.private_scope
            );
            id
        };
        let id = report("before");
        if mode != "report" {
            let r = set_private(&app, &id, mode == "mark");
            eprintln!("mori: DEBUG privacy {mode}: {r:?}");
            report("after");
        }
    });
}

/// A folder just became private: drop its contents from analysis results
/// that were computed from outside it (results from an analysis rooted at
/// the folder itself, or inside it, stay: that was an explicit choice).
fn hide_from_analyses(state: &AppState, dir: &Path) {
    let hidden = |root: &Path, rel: &str| !(root == dir || root.starts_with(dir)) && root.join(rel).starts_with(dir);
    if let Some(store) = state.analysis.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
        let a = &mut store.analysis;
        let gone: HashSet<usize> =
            (0..a.files.len()).filter(|&i| hidden(&a.roots[a.files[i].root].canon, &a.files[i].rel)).collect();
        dupes::forget(a, &gone);
    }
    if let Some(r) =
        state.similar.lock().unwrap_or_else(PoisonError::into_inner).as_mut().and_then(|s| s.result.as_mut())
    {
        let a = &r.analysis;
        let gone: HashSet<usize> =
            (0..a.files.len()).filter(|&i| hidden(&a.roots[a.files[i].root].canon, &a.files[i].rel)).collect();
        similar::forget(r, &gone);
    }
}

// ------------------------------------------------------ duplicate analyzer

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LocationInfo {
    key: String,
    label: String,
    path: String,
    drive: String,
}

fn drive_of(p: &Path) -> String {
    let s = secure::plain_path(p);
    if let Some(rest) = s.strip_prefix("/Volumes/") {
        return rest.split('/').next().unwrap_or("External drive").to_owned();
    }
    if cfg!(windows) {
        return s.chars().take(2).collect();
    }
    "This computer".into()
}

fn pretty_path(app: &AppHandle, p: &Path) -> String {
    let s = secure::plain_path(p);
    if let Ok(home) = app.path().home_dir() {
        let h = secure::plain_path(&home);
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}

/// Resolve a location key chosen in the UI to a folder. The UI never sends paths.
fn location_path(app: &AppHandle, state: &AppState, key: &str) -> Option<(PathBuf, String)> {
    let p = app.path();
    let (path, label) = match key {
        "library" => {
            (state.root_canon()?, state.root.read().unwrap_or_else(PoisonError::into_inner).as_ref()?.name.clone())
        }
        "home" => (p.home_dir().ok()?, "Home".into()),
        "pictures" => (p.picture_dir().ok()?, "Pictures".into()),
        "videos" => (p.video_dir().ok()?, if cfg!(target_os = "macos") { "Movies".into() } else { "Videos".into() }),
        "downloads" => (p.download_dir().ok()?, "Downloads".into()),
        "documents" => (p.document_dir().ok()?, "Documents".into()),
        k => {
            let n: usize = k.strip_prefix("custom-")?.parse().ok()?;
            let path = state.custom_locations.lock().unwrap_or_else(PoisonError::into_inner).get(n)?.clone();
            let label =
                path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| secure::plain_path(&path));
            (path, label)
        }
    };
    let canon = fs::canonicalize(path).ok().filter(|c| c.is_dir())?;
    Some((canon, secure::display_safe(&label)))
}

#[tauri::command]
fn analysis_locations(app: AppHandle, state: State<'_, AppState>) -> Vec<LocationInfo> {
    let mut keys: Vec<String> =
        ["library", "home", "pictures", "videos", "downloads", "documents"].iter().map(|s| s.to_string()).collect();
    let customs = state.custom_locations.lock().unwrap_or_else(PoisonError::into_inner).len();
    keys.extend((0..customs).map(|i| format!("custom-{i}")));
    keys.into_iter()
        .filter_map(|k| {
            let (path, label) = location_path(&app, &state, &k)?;
            Some(LocationInfo { drive: drive_of(&path), path: pretty_path(&app, &path), label, key: k })
        })
        .collect()
}

/// Native folder picker (runs in Rust): the chosen folder becomes an
/// authorised analysis location for this session only.
#[tauri::command]
async fn analysis_choose_folder(app: AppHandle) -> Result<LocationInfo, String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().set_title("Choose a folder to analyze").blocking_pick_folder();
    forget_open_panel_location();
    let picked = picked.ok_or("cancelled")?;
    let path = picked.into_path().map_err(|_| "That folder can't be opened")?;
    let canon = fs::canonicalize(path).map_err(|_| "That folder can't be opened")?;
    let state = app.state::<AppState>();
    let key = {
        let mut list = state.custom_locations.lock().unwrap_or_else(PoisonError::into_inner);
        let n = list.iter().position(|p| *p == canon).unwrap_or_else(|| {
            list.push(canon.clone());
            list.len() - 1
        });
        format!("custom-{n}")
    };
    let (path, label) = location_path(&app, &state, &key).ok_or("That folder can't be opened")?;
    Ok(LocationInfo { drive: drive_of(&path), path: pretty_path(&app, &path), label, key })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AnalysisEvent {
    status: &'static str,
    message: Option<String>,
}

/// The chosen analysis locations, with their private folders as boundaries.
fn analysis_roots(app: &AppHandle, state: &AppState, locations: &[String]) -> Vec<dupes::Root> {
    let mut roots: Vec<dupes::Root> = Vec::new();
    for key in locations.iter().take(32) {
        if let Some((canon, label)) = location_path(app, state, key) {
            if !roots.iter().any(|r| r.canon == canon) {
                let private = state.privacy.boundaries(&canon);
                roots.push(dupes::Root { canon, label, private });
            }
        }
    }
    roots
}

#[tauri::command]
fn analysis_start(
    app: AppHandle,
    state: State<'_, AppState>,
    locations: Vec<String>,
    kinds: Vec<String>,
    recursive: bool,
) -> Result<(), String> {
    if state.analysis_running.swap(true, Ordering::SeqCst) {
        return Err("An analysis is already running.".into());
    }
    let roots = analysis_roots(&app, &state, &locations);
    if roots.is_empty() {
        state.analysis_running.store(false, Ordering::SeqCst);
        return Err("Choose at least one available location.".into());
    }
    let mut ks = Vec::new();
    for k in &kinds {
        match k.as_str() {
            "images" => ks.extend([index::Kind::Photo, index::Kind::Gif]),
            "videos" => ks.push(index::Kind::Video),
            "documents" => ks.push(index::Kind::Document),
            "audio" => ks.push(index::Kind::Audio),
            "other" => ks.push(index::Kind::Other),
            _ => {}
        }
    }
    let spec = dupes::Spec { roots, kinds: (!ks.is_empty()).then_some(ks), recursive };
    // A new analysis replaces the previous one entirely.
    *state.analysis.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.volatile_thumbs.clear();
    state.analysis_cancel.store(false, Ordering::SeqCst);
    let cancel = state.analysis_cancel.clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        let res = catch_unwind(AssertUnwindSafe(|| {
            dupes::analyze(spec, &cancel, &mut |p| {
                let _ = app2.emit("analysis-progress", p);
            })
        }));
        let state = app2.state::<AppState>();
        let event = match res {
            Ok(Ok(analysis)) => {
                let ids = analysis
                    .files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| (analysis_id(&analysis.roots[f.root].canon, &f.rel), i))
                    .collect();
                *state.analysis.lock().unwrap_or_else(PoisonError::into_inner) = Some(AnalysisStore { analysis, ids });
                AnalysisEvent { status: "done", message: None }
            }
            Ok(Err(dupes::Cancelled)) => AnalysisEvent { status: "cancelled", message: None },
            Err(_) => AnalysisEvent { status: "failed", message: Some("The analysis stopped unexpectedly.".into()) },
        };
        state.analysis_running.store(false, Ordering::SeqCst);
        let _ = app2.emit("analysis-done", event);
    });
    Ok(())
}

#[tauri::command]
fn analysis_cancel(state: State<'_, AppState>) {
    state.analysis_cancel.store(true, Ordering::SeqCst);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewFile {
    id: String,
    name: String,
    path: String,
    location: String,
    drive: String,
    size: u64,
    modified: i64,
    created: Option<i64>,
    kind: index::Kind,
    ext: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewMember {
    files: Vec<ViewFile>,
    locked: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ViewGroup {
    index: usize,
    live: bool,
    unit_size: u64,
    recoverable: u64,
    suggested: usize,
    /// Why the suggested copy was picked.
    reasons: Vec<String>,
    members: Vec<ViewMember>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalysisView {
    locations: Vec<LocationInfo>,
    stats: dupes::Stats,
    groups: Vec<ViewGroup>,
}

#[tauri::command]
fn analysis_results(app: AppHandle, state: State<'_, AppState>) -> Option<AnalysisView> {
    let guard = state.analysis.lock().unwrap_or_else(PoisonError::into_inner);
    let a = &guard.as_ref()?.analysis;
    let drives: Vec<String> = a.roots.iter().map(|r| drive_of(&r.canon)).collect();
    let file = |i: usize| {
        let f = &a.files[i];
        ViewFile {
            id: analysis_id(&a.roots[f.root].canon, &f.rel),
            name: secure::display_safe(&f.name),
            path: secure::display_safe(&f.rel),
            location: a.roots[f.root].label.clone(),
            drive: drives[f.root].clone(),
            size: f.size,
            modified: f.modified,
            created: f.created,
            kind: f.kind,
            ext: secure::display_safe(&f.ext),
        }
    };
    Some(AnalysisView {
        locations: a
            .roots
            .iter()
            .map(|r| LocationInfo {
                key: String::new(),
                label: r.label.clone(),
                path: pretty_path(&app, &r.canon),
                drive: drive_of(&r.canon),
            })
            .collect(),
        stats: a.stats.clone(),
        groups: a
            .groups
            .iter()
            .enumerate()
            .map(|(gi, g)| ViewGroup {
                index: gi,
                live: g.live,
                unit_size: g.unit_size,
                recoverable: g.recoverable(),
                suggested: g.suggested,
                reasons: dupes::suggest_reasons(&a.files, &a.roots, g),
                members: g
                    .members
                    .iter()
                    .map(|m| ViewMember { locked: m.locked, files: m.files.iter().map(|&f| file(f)).collect() })
                    .collect(),
            })
            .collect(),
    })
}

/// Forget the analysis (memory only; nothing about it was written to disk).
#[tauri::command]
fn analysis_clear(state: State<'_, AppState>) {
    state.analysis_cancel.store(true, Ordering::SeqCst);
    *state.analysis.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.volatile_thumbs.clear();
}

#[derive(Serialize, Clone)]
struct CleanupProgress {
    done: u64,
    total: u64,
}

/// Move the selected duplicates to Trash after re-validating everything.
/// The plan is checked here (not only in the UI): every group keeps a copy.
#[tauri::command]
async fn analysis_cleanup(app: AppHandle, plan: Vec<dupes::PlanItem>) -> Result<dupes::Outcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.cleanup_cancel.store(false, Ordering::SeqCst);
        let cancel = state.cleanup_cancel.clone();
        let mut cleaned: Vec<history::Change> = Vec::new();
        let (outcome, removed_abs) = {
            let mut guard = state.analysis.lock().unwrap_or_else(PoisonError::into_inner);
            let store = guard.as_mut().ok_or("The analysis was cleared.")?;
            let app2 = app.clone();
            let thumb_state = app.state::<AppState>();
            let outcome = dupes::execute(
                &store.analysis,
                &plan,
                &mut |root, rel| {
                    let (path, meta) = fileops::confined_item(root, rel)?;
                    if !meta.is_file() {
                        return Err("not a regular file".into());
                    }
                    invalidate_thumbs(&thumb_state, &path, &meta);
                    let trashed = fileops::move_to_trash(&thumb_state.policy(), &path)?;
                    cleaned.push(history::Change::Trashed { original: path, trashed });
                    Ok(())
                },
                &cancel,
                &mut |done, total| {
                    let _ = app2.emit("cleanup-progress", CleanupProgress { done, total });
                },
            )?;
            let a = &store.analysis;
            let removed_abs: Vec<PathBuf> =
                outcome.removed.iter().map(|&i| a.roots[a.files[i].root].canon.join(&a.files[i].rel)).collect();
            (outcome, removed_abs)
        };
        forget_removed(&state, &removed_abs);
        state.history.record(trash_label(&cleaned), cleaned);
        let rels: Vec<String> = removed_abs.iter().filter_map(|p| browser_rel(&state, p)).collect();
        if !rels.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&rels));
        }
        Ok(outcome)
    })
    .await
    .map_err(|_| "The cleanup stopped unexpectedly.".to_string())?
}

#[tauri::command]
fn analysis_cleanup_cancel(state: State<'_, AppState>) {
    state.cleanup_cancel.store(true, Ordering::SeqCst);
}

// ----------------------------------------------------------- similar media

/// Whether the worker needs its HEIF profile for these bytes (tests use it
/// to drive the real worker binary).
#[cfg(test)]
fn heif_flag(bytes: &[u8]) -> bool {
    #[cfg(target_os = "macos")]
    return heif::is_heif(bytes);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bytes;
        false
    }
}

fn similar_id(root: &Path, rel: &str) -> String {
    format!("y{:016x}", thumbs::fnv(format!("{}\u{0}{rel}", root.to_string_lossy()).as_bytes()))
}

impl AppState {
    fn similar_dir(&self) -> PathBuf {
        self.thumb_dir.parent().map_or_else(|| self.thumb_dir.join("similar"), |p| p.join("similar"))
    }
    fn dismissed_file(&self) -> PathBuf {
        self.data_dir.join("similar-dismissed.bin")
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct CaptureRequest {
    token: u64,
    id: String,
    /// Seconds; empty = the standard sample positions.
    times: Vec<f64>,
}

#[tauri::command]
fn similar_start(
    app: AppHandle,
    state: State<'_, AppState>,
    locations: Vec<String>,
    photos: bool,
    videos: bool,
    recursive: bool,
    sensitivity: similar::Sensitivity,
) -> Result<(), String> {
    if !photos && !videos {
        return Err("Choose photos, videos or both.".into());
    }
    if state.similar_running.swap(true, Ordering::SeqCst) {
        return Err("An analysis is already running.".into());
    }
    let roots = analysis_roots(&app, &state, &locations);
    if roots.is_empty() {
        state.similar_running.store(false, Ordering::SeqCst);
        return Err("Choose at least one available location.".into());
    }
    launch_similar(app, &state, roots, photos, videos, recursive, sensitivity);
    Ok(())
}

fn launch_similar(
    app: AppHandle,
    state: &AppState,
    roots: Vec<dupes::Root>,
    photos: bool,
    videos: bool,
    recursive: bool,
    sensitivity: similar::Sensitivity,
) {
    // A new analysis replaces the previous one.
    *state.similar.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.volatile_thumbs.clear();
    state.similar_cancel.store(false, Ordering::SeqCst);
    let cancel = state.similar_cancel.clone();
    let spec = similar::Spec { roots, photos, videos, recursive, sensitivity };
    let app2 = app.clone();
    std::thread::spawn(move || {
        let state = app2.state::<AppState>();
        let res = catch_unwind(AssertUnwindSafe(|| {
            // Photos: the sandboxed worker, never this process.
            let decode = |bytes: Vec<u8>| {
                worker::run(worker::Op::Fingerprint, 64, bytes, Duration::from_secs(25))
                    .ok()
                    .map(|o| (o.width, o.height, o.bytes))
            };
            let roots_seen: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
            let mut registered = |roots: &[dupes::Root], files: &[dupes::FileRec]| {
                let canon: Vec<PathBuf> = roots.iter().map(|r| r.canon.clone()).collect();
                let ids = files.iter().enumerate().map(|(i, f)| (similar_id(&canon[f.root], &f.rel), i)).collect();
                *roots_seen.lock().unwrap() = canon.clone();
                *state.similar.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(SimilarStore { roots: canon, files: files.to_vec(), ids, result: None });
            };
            // Videos: the webview decodes sampled frames through Mori's guarded
            // video path (probe, blocklist, watchdog) and sends raw pixels back.
            let mut capture = |_: usize, f: &dupes::FileRec, times: &[f64]| {
                let Some(root) = roots_seen.lock().unwrap().get(f.root).cloned() else {
                    return Err(similar::CaptureError::Unavailable);
                };
                let token = state.similar_token.fetch_add(1, Ordering::SeqCst) + 1;
                let (tx, rx) = std::sync::mpsc::channel();
                *state.similar_capture.lock().unwrap_or_else(PoisonError::into_inner) = Some((token, tx));
                let _ = app2.emit(
                    "similar-capture",
                    CaptureRequest { token, id: similar_id(&root, &f.rel), times: times.to_vec() },
                );
                let deadline = std::time::Instant::now() + Duration::from_secs(40 + 2 * times.len() as u64);
                loop {
                    if cancel.load(Ordering::Relaxed) || std::time::Instant::now() > deadline {
                        *state.similar_capture.lock().unwrap_or_else(PoisonError::into_inner) = None;
                        return Err(similar::CaptureError::Unavailable);
                    }
                    if let Ok(r) = rx.recv_timeout(Duration::from_millis(200)) {
                        return r;
                    }
                }
            };
            let dismissed = similar::load_dismissed(&state.dismissed_file());
            let mut env = similar::Env {
                // A temporary session leaves no fingerprints behind either.
                cache_dir: (!state.temp.load(Ordering::SeqCst)).then(|| state.similar_dir()),
                decode: &decode,
                capture: &mut capture,
                registered: &mut registered,
                dismissed: &dismissed,
            };
            similar::analyze(spec, &mut env, &cancel, &mut |p| {
                let _ = app2.emit("similar-progress", p);
            })
        }));
        let event = match res {
            Ok(Ok(mut result)) => {
                // Bursts: capture times of the grouped photos only (worker-read EXIF).
                let wanted: Vec<(usize, PathBuf, dupes::FileRec)> = result
                    .analysis
                    .groups
                    .iter()
                    .zip(&result.meta)
                    .filter(|(g, m)| !m.video && g.members.len() >= 3)
                    .flat_map(|(g, _)| g.members.iter().map(|mem| mem.files[0]))
                    .map(|f| {
                        let rec = result.analysis.files[f].clone();
                        (f, result.analysis.roots[rec.root].canon.clone(), rec)
                    })
                    .collect();
                if !wanted.is_empty() {
                    let times = metascan::capture_times(&wanted);
                    similar::mark_bursts(&mut result, &times);
                }
                if let Some(store) = state.similar.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                    store.result = Some(result);
                }
                AnalysisEvent { status: "done", message: None }
            }
            Ok(Err(dupes::Cancelled)) => {
                *state.similar.lock().unwrap_or_else(PoisonError::into_inner) = None;
                AnalysisEvent { status: "cancelled", message: None }
            }
            Err(_) => {
                *state.similar.lock().unwrap_or_else(PoisonError::into_inner) = None;
                AnalysisEvent { status: "failed", message: Some("The analysis stopped unexpectedly.".into()) }
            }
        };
        *state.similar_capture.lock().unwrap_or_else(PoisonError::into_inner) = None;
        state.similar_running.store(false, Ordering::SeqCst);
        let _ = app2.emit("similar-done", event);
    });
}

/// Debug builds only: `MORI_DEBUG_SIMILAR=<folder>` runs a Similar Media
/// analysis on that folder at launch (real worker, real webview sampling)
/// and prints the groups, for testing without UI automation.
fn debug_similar_autorun(app: &AppHandle) {
    let Some(dir) = std::env::var_os("MORI_DEBUG_SIMILAR").map(PathBuf::from) else { return };
    let Ok(canon) = fs::canonicalize(dir) else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(4));
        let state = app.state::<AppState>();
        state.similar_running.store(true, Ordering::SeqCst);
        let t = std::time::Instant::now();
        let private = state.privacy.boundaries(&canon);
        let root = dupes::Root { canon, label: "Debug".into(), private };
        launch_similar(app.clone(), &state, vec![root], true, true, true, similar::Sensitivity::Balanced);
        while state.similar_running.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(200));
        }
        let guard = state.similar.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(r) = guard.as_ref().and_then(|s| s.result.as_ref()) else {
            eprintln!("mori: DEBUG similar: no result");
            return;
        };
        eprintln!("mori: DEBUG similar done in {:.1?}: {:?}", t.elapsed(), r.stats);
        for (g, m) in r.analysis.groups.iter().zip(&r.meta) {
            let names: Vec<String> = g
                .members
                .iter()
                .zip(&m.members)
                .map(|(mem, mm)| {
                    format!(
                        "{} {}%{}",
                        r.analysis.files[mem.files[0]].rel,
                        mm.similarity,
                        if mm.exact { " exact" } else { "" }
                    )
                })
                .collect();
            eprintln!(
                "mori: DEBUG group video={} {}% burst={:?} :: {}",
                m.video,
                m.similarity,
                m.burst,
                names.join(" | ")
            );
        }
    });
}

#[tauri::command]
fn similar_cancel(state: State<'_, AppState>) {
    state.similar_cancel.store(true, Ordering::SeqCst);
}

/// Sampled frames from the webview for a pending capture request. Body: raw
/// 64×64 grayscale planes; everything else arrives in fixed headers and is
/// validated here (sizes, ranges). An empty body means "could not decode".
#[tauri::command]
fn similar_frames(state: State<'_, AppState>, request: tauri::ipc::Request<'_>) {
    let h = |k: &str| request.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let Some(token) = h("mori-token").and_then(|t| t.parse::<u64>().ok()) else { return };
    let Some((pending, tx)) = state.similar_capture.lock().unwrap_or_else(PoisonError::into_inner).take() else {
        return;
    };
    if pending != token {
        // A late answer to an earlier request: keep waiting for the current one.
        *state.similar_capture.lock().unwrap_or_else(PoisonError::into_inner) = Some((pending, tx));
        return;
    }
    let num = |k: &str, max: u64| h(k).and_then(|v| v.parse::<u64>().ok()).filter(|v| *v <= max);
    let text = |k: &str| {
        h(k).map(|v| v.chars().filter(|c| c.is_ascii_alphanumeric() || *c == ' ').take(24).collect::<String>())
            .unwrap_or_default()
    };
    let planes = match request.body() {
        tauri::ipc::InvokeBody::Raw(b) => b.clone(),
        _ => Vec::new(),
    };
    let result = match (num("mori-width", 20_000), num("mori-height", 20_000), num("mori-duration", 48 * 3600 * 1000)) {
        (Some(w), Some(hh), Some(d))
            if !planes.is_empty() && planes.len() % similar::PLANE == 0 && planes.len() <= 64 * similar::PLANE =>
        {
            Ok(similar::Capture {
                width: w as u32,
                height: hh as u32,
                duration_ms: d as u32,
                container: text("mori-container"),
                codec: text("mori-codec"),
                planes,
            })
        }
        _ => Err(similar::CaptureError::Failed),
    };
    let _ = tx.send(result);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimFile {
    id: String,
    name: String,
    path: String,
    location: String,
    drive: String,
    size: u64,
    modified: i64,
    created: Option<i64>,
    kind: index::Kind,
    ext: String,
    #[serde(flatten)]
    media: similar::MediaInfo,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimMember {
    files: Vec<SimFile>,
    similarity: u8,
    exact: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimGroup {
    index: usize,
    video: bool,
    similarity: u8,
    /// Bytes recovered if every copy except the suggested one goes to Trash.
    recoverable: u64,
    /// Why the suggested copy (the first) is preferred.
    reasons: Vec<String>,
    /// Photos taken in quick succession by the same camera.
    burst: Option<similar::Burst>,
    members: Vec<SimMember>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SimView {
    locations: Vec<LocationInfo>,
    stats: similar::SimStats,
    groups: Vec<SimGroup>,
}

#[tauri::command]
fn similar_results(app: AppHandle, state: State<'_, AppState>) -> Option<SimView> {
    let guard = state.similar.lock().unwrap_or_else(PoisonError::into_inner);
    let r = guard.as_ref()?.result.as_ref()?;
    let a = &r.analysis;
    let drives: Vec<String> = a.roots.iter().map(|x| drive_of(&x.canon)).collect();
    let file = |i: usize| {
        let f = &a.files[i];
        SimFile {
            id: similar_id(&a.roots[f.root].canon, &f.rel),
            name: secure::display_safe(&f.name),
            path: secure::display_safe(&f.rel),
            location: a.roots[f.root].label.clone(),
            drive: drives[f.root].clone(),
            size: f.size,
            modified: f.modified,
            created: f.created,
            kind: f.kind,
            ext: secure::display_safe(&f.ext),
            media: r.media.get(&i).cloned().unwrap_or_default(),
        }
    };
    Some(SimView {
        locations: a
            .roots
            .iter()
            .map(|x| LocationInfo {
                key: String::new(),
                label: x.label.clone(),
                path: pretty_path(&app, &x.canon),
                drive: drive_of(&x.canon),
            })
            .collect(),
        stats: r.stats.clone(),
        groups: a
            .groups
            .iter()
            .zip(&r.meta)
            .enumerate()
            .map(|(gi, (g, m))| SimGroup {
                index: gi,
                video: m.video,
                similarity: m.similarity,
                recoverable: g.members[1..].iter().flat_map(|mem| &mem.files).map(|&f| a.files[f].size).sum(),
                reasons: similar::keep_reasons(r, gi),
                burst: m.burst.clone(),
                members: g
                    .members
                    .iter()
                    .zip(&m.members)
                    .map(|(mem, mm)| SimMember {
                        files: mem.files.iter().map(|&f| file(f)).collect(),
                        similarity: mm.similarity,
                        exact: mm.exact,
                    })
                    .collect(),
            })
            .collect(),
    })
}

/// "Not duplicates": forget the group (or one member) and remember the
/// decision locally by content identity, so it isn't suggested again.
#[tauri::command]
fn similar_dismiss(state: State<'_, AppState>, group: usize, member: Option<usize>) -> Result<(), String> {
    let pairs = {
        let mut guard = state.similar.lock().unwrap_or_else(PoisonError::into_inner);
        let r = guard.as_mut().and_then(|s| s.result.as_mut()).ok_or("The analysis was cleared.")?;
        similar::dismiss(r, group, member)
    };
    if pairs.is_empty() || state.temp.load(Ordering::SeqCst) {
        // A temporary session only removes the group from these results.
        return Ok(());
    }
    let path = state.dismissed_file();
    let mut set = similar::load_dismissed(&path);
    set.extend(pairs);
    similar::save_dismissed(&path, &set).map_err(|_| "Could not save the decision.".to_string())
}

#[tauri::command]
fn similar_dismissed_count(state: State<'_, AppState>) -> usize {
    similar::load_dismissed(&state.dismissed_file()).len()
}

#[tauri::command]
fn similar_forget_decisions(state: State<'_, AppState>) {
    let _ = fs::remove_file(state.dismissed_file());
}

#[tauri::command]
fn similar_clear(state: State<'_, AppState>) {
    state.similar_cancel.store(true, Ordering::SeqCst);
    *state.similar.lock().unwrap_or_else(PoisonError::into_inner) = None;
    state.previews.lock().unwrap_or_else(PoisonError::into_inner).clear();
    state.volatile_thumbs.clear();
}

/// Same rules as exact duplicates (shared `dupes::execute`): validated plan,
/// at least one copy kept, kept copies re-checked first, OS Trash only.
#[tauri::command]
async fn similar_cleanup(app: AppHandle, plan: Vec<dupes::PlanItem>) -> Result<dupes::Outcome, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.cleanup_cancel.store(false, Ordering::SeqCst);
        let cancel = state.cleanup_cancel.clone();
        let mut cleaned: Vec<history::Change> = Vec::new();
        let (outcome, removed_abs) = {
            let mut guard = state.similar.lock().unwrap_or_else(PoisonError::into_inner);
            let r = guard.as_mut().and_then(|s| s.result.as_mut()).ok_or("The analysis was cleared.")?;
            let app2 = app.clone();
            let thumb_state = app.state::<AppState>();
            let outcome = dupes::execute(
                &r.analysis,
                &plan,
                &mut |root, rel| {
                    let (path, meta) = fileops::confined_item(root, rel)?;
                    if !meta.is_file() {
                        return Err("not a regular file".into());
                    }
                    invalidate_thumbs(&thumb_state, &path, &meta);
                    let trashed = fileops::move_to_trash(&thumb_state.policy(), &path)?;
                    cleaned.push(history::Change::Trashed { original: path, trashed });
                    Ok(())
                },
                &cancel,
                &mut |done, total| {
                    let _ = app2.emit("cleanup-progress", CleanupProgress { done, total });
                },
            )?;
            let a = &r.analysis;
            let removed_abs: Vec<PathBuf> =
                outcome.removed.iter().map(|&i| a.roots[a.files[i].root].canon.join(&a.files[i].rel)).collect();
            (outcome, removed_abs)
        };
        forget_removed(&state, &removed_abs);
        state.history.record(trash_label(&cleaned), cleaned);
        let rels: Vec<String> = removed_abs.iter().filter_map(|p| browser_rel(&state, p)).collect();
        if !rels.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&rels));
        }
        Ok(outcome)
    })
    .await
    .map_err(|_| "The cleanup stopped unexpectedly.".to_string())?
}

/// Spawn the OS opener directly (absolute binary, argument array, no shell).
fn system_open(path: &Path, reveal: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("/usr/bin/open");
        if reveal {
            c.arg("-R");
        }
        c.arg(path);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let explorer =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("explorer.exe");
        let mut c = Command::new(explorer);
        let p = secure::plain_path(path);
        if reveal {
            c.raw_arg(format!("/select,\"{p}\""));
        } else {
            c.arg(p);
        }
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("/usr/bin/xdg-open");
        c.arg(if reveal { path.parent().unwrap_or(path) } else { path });
        c
    };
    cmd.spawn().map(|_| ()).map_err(|_| "Could not open".into())
}

/// If the UI stops answering heartbeats while a video session is active (and
/// the window is in front, so timers aren't being throttled), assume the
/// system media engine hung: block the file and reload the UI.
fn spawn_media_watchdog(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let _ = catch_unwind(AssertUnwindSafe(|| watchdog_tick(&app)));
    });
}

fn watchdog_tick(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.video.ui_stalled() {
        return;
    }
    let Some(win) = app.get_webview_window("main") else { return };
    let in_front = win.is_focused().unwrap_or(false) && !win.is_minimized().unwrap_or(true);
    if !in_front {
        // Timers are throttled in the background; don't misread that as a
        // hang, but stay armed so a real freeze is caught once it's in front.
        state.video.postpone();
        return;
    }
    let culprit = state.video.fail_active();
    debug_log!("mori: UI stopped responding (suspect: {culprit:?}); restarting web content");
    state.video.disarm(); // re-armed by the first ping from the fresh page
    restart_web_content(&win);
}

/// A wedged content process can't service a reload (it would queue behind the
/// stuck work). On macOS, terminate it (WebKit then reports the termination
/// and the hook in `main` reloads); as a backstop, keep reloading until the
/// fresh page pings back.
fn restart_web_content(win: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        let _ = win.with_webview(|wv| unsafe {
            use objc2::runtime::{AnyObject, Sel};
            use objc2::{msg_send, sel};
            let view = wv.inner() as *mut AnyObject;
            if view.is_null() {
                return;
            }
            // WebKit's own `_killWebContentProcess` is a no-op on a busy
            // process, so ask for its pid and terminate it ourselves.
            let pid_sel: Sel = sel!(_webProcessIdentifier);
            let supported: bool = msg_send![view, respondsToSelector: pid_sel];
            if !supported {
                return;
            }
            let pid: libc::pid_t = msg_send![view, _webProcessIdentifier];
            if is_webkit_content_process(pid) {
                debug_log!("mori: terminating hung web content process {pid}");
                libc::kill(pid, libc::SIGKILL);
            }
        });
    }
    let win = win.clone();
    std::thread::spawn(move || {
        let state = win.state::<AppState>();
        for attempt in 0..3u64 {
            std::thread::sleep(Duration::from_millis(600 + attempt * 2000));
            if state.video.ui_pinged() {
                return; // the fresh page is up
            }
            debug_log!("mori: reloading web content (attempt {})", attempt + 1);
            let _ = win.reload();
        }
    });
}

/// Only ever signal a process that really is WebKit's WebContent service.
#[cfg(target_os = "macos")]
fn is_webkit_content_process(pid: libc::pid_t) -> bool {
    if pid <= 0 || pid == std::process::id() as libc::pid_t {
        return false;
    }
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    n > 0 && String::from_utf8_lossy(&buf[..n as usize]).ends_with("com.apple.WebKit.WebContent")
}

/// Record only *where* a panic happened (source file and line of Mori or a
/// library) so failures are diagnosable, never any message (which could
/// contain file names). Stays on this machine, overwritten each time.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if cfg!(debug_assertions) {
            default(info);
        }
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("worker").to_owned();
        if let Some(dir) = dirs_data() {
            let _ = fs::create_dir_all(&dir);
            let _ = fs::write(dir.join("last-panic.txt"), format!("{}\n{thread}\n{location}\n", index::now_millis()));
        }
    }));
}

/// The app data dir without needing an AppHandle (the hook may run anywhere).
fn dirs_data() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/app.mori.viewer"));
    #[cfg(windows)]
    return std::env::var_os("APPDATA").map(|h| PathBuf::from(h).join("app.mori.viewer"));
    #[cfg(all(unix, not(target_os = "macos")))]
    return std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/app.mori.viewer"));
}

fn build_window(app: &tauri::App) -> tauri::Result<()> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
        .title("Mori")
        .inner_size(1280.0, 820.0)
        .min_inner_size(760.0, 480.0)
        .disable_drag_drop_handler()
        // Keep the page running when the window is in the background, so a
        // Similar Media analysis (videos are sampled by the webview) and
        // thumbnail capture don't stall while the user works in other apps.
        .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
        // Non-persistent web view storage: WebKit keeps no cookies, local
        // storage, IndexedDB, HTTP cache or tracking statistics on disk for
        // Mori; everything the page holds is gone when Mori quits.
        .incognito(true)
        // The webview may only ever show Mori's own bundled UI.
        .on_navigation(|url| {
            let s = url.as_str();
            s.starts_with("tauri://localhost")
                || s.starts_with("http://tauri.localhost")
                || s.starts_with("https://tauri.localhost")
                || (cfg!(debug_assertions) && s.starts_with("http://127.0.0.1:1420"))
        });
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
    builder.build()?;
    Ok(())
}

fn main() {
    // Worker mode: sandbox, decode one file from stdin, exit. Never starts the UI.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(worker::WORKER_FLAG) {
        worker::worker_main(&args[2..]);
    }
    // Developer aid (debug builds only): `mori --debug-probe <dir>` prints what
    // the video guard decides for every file in a folder.
    #[cfg(debug_assertions)]
    if args.get(1).map(String::as_str) == Some("--debug-probe") {
        let dir = PathBuf::from(&args[2]);
        let mut names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let name = name.to_string_lossy().into_owned();
            let Ok((mut f, m, _)) = secure::open_inside(&fs::canonicalize(&dir).unwrap(), &name) else { continue };
            let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
            let head = secure::read_head(&mut f, secure::SNIFF_LEN);
            let detected = secure::sniff(&head, &ext);
            let t = std::time::Instant::now();
            let info = if detected.is_video() { Some(video::probe_file(f, m.len(), detected)) } else { None };
            println!("{name:30} {detected:?} {info:?} ({:?})", t.elapsed());
        }
        return;
    }

    install_panic_hook();
    let builder = tauri::Builder::default();
    // If WebKit's content process dies (e.g. inside the media engine), block
    // the video that was loading so it can't crash Mori again, then reload.
    #[cfg(target_os = "macos")]
    let builder = builder.on_web_content_process_terminate(|webview| {
        debug_log!("mori: web content process terminated; recovering");
        if let Some(state) = webview.try_state::<AppState>() {
            state.video.fail_active();
            state.video.disarm();
        }
        let _ = webview.reload();
    });
    builder
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("mori", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                // A bug or hostile input can at worst fail this one request.
                let resp = catch_unwind(AssertUnwindSafe(|| protocol::handle(&app, request)))
                    .unwrap_or_else(|_| protocol::internal_error());
                let _ = catch_unwind(AssertUnwindSafe(move || responder.respond(resp)));
            });
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let cache_dir = app.path().app_cache_dir()?;
            // Remove caches written by older, less isolated versions.
            let _ = fs::remove_dir_all(cache_dir.join("thumbnails"));
            let _ = fs::remove_dir_all(data_dir.join("indexes"));
            let thumb_dir = cache_dir.join("thumbs");
            fs::create_dir_all(&data_dir)?;
            fs::create_dir_all(&thumb_dir)?;
            let data_dir_for_video = data_dir.clone();
            // Leftovers of writes interrupted by a crash or power loss.
            let dirs = localdata::Dirs::new(data_dir.clone(), cache_dir.clone());
            let stale_cleaned = localdata::startup_cleanup(&dirs);
            // A folder remembered by the system picker before a crash or forced quit.
            forget_open_panel_location();
            app.manage(AppState {
                dirs,
                stale_cleaned,
                data_dir,
                thumb_dir,
                volatile_thumbs: thumbs::Volatile::default(),
                root: RwLock::new(None),
                index: RwLock::new(Arc::new(Index::default())),
                scan_gen: AtomicU64::new(0),
                scanning: AtomicBool::new(false),
                scan_count: AtomicUsize::new(0),
                settings_lock: Mutex::new(()),
                previews: Mutex::new(PreviewCache::default()),
                video: video::VideoGuard::new(&data_dir_for_video),
                analysis: Mutex::new(None),
                analysis_running: AtomicBool::new(false),
                analysis_cancel: Arc::new(AtomicBool::new(false)),
                cleanup_cancel: Arc::new(AtomicBool::new(false)),
                custom_locations: Mutex::new(Vec::new()),
                similar: Mutex::new(None),
                similar_running: AtomicBool::new(false),
                similar_cancel: Arc::new(AtomicBool::new(false)),
                similar_capture: Mutex::new(None),
                similar_token: AtomicU64::new(0),
                privacy: privacy::Store::load(data_dir_for_video.join("private-folders.json")),
                protected: privacy::Store::load(data_dir_for_video.join("protected-folders.json")),
                drives: drives::Store::load(data_dir_for_video.join("drives.json")),
                safe_mode: AtomicBool::new(false),
                decoded: AtomicU64::new(0),
                connected: Mutex::new(HashMap::new()),
                favorites: privacy::Store::load(data_dir_for_video.join("favorites.json")),
                tags: tags::Store::load(data_dir_for_video.join("tags.json")),
                temp: AtomicBool::new(false),
                private_inspection: AtomicBool::new(false),
                session_read_only: AtomicBool::new(false),
                session_safe: AtomicBool::new(false),
                capture_not: privacy::Store::load(data_dir_for_video.join("capture-not.json")),
                capture_yes: privacy::Store::load(data_dir_for_video.join("capture-yes.json")),
                history: history::History::default(),
                transfer_dests: Mutex::new(Vec::new()),
                transfer_cancel: AtomicBool::new(false),
                cleanup: cleanup::Store::load(data_dir_for_video.join("cleanup-sessions.json")),
                checksums: integrity::Cache::default(),
                checksum_cancel: AtomicBool::new(false),
                integrity: integrity::Store::new(data_dir_for_video.join("integrity")),
                integrity_job: Arc::new(jobs::Control::default()),
                integrity_running: AtomicBool::new(false),
                health: Mutex::new(None),
                health_running: AtomicBool::new(false),
                health_job: Arc::new(jobs::Control::default()),
                meta_scan: Mutex::new(None),
                meta_running: AtomicBool::new(false),
                meta_job: Arc::new(jobs::Control::default()),
                read_only: AtomicBool::new(
                    fs::read(data_dir_for_video.join("settings.json"))
                        .ok()
                        .and_then(|d| serde_json::from_slice::<Settings>(&d).ok())
                        .and_then(|s| s.read_only)
                        .unwrap_or(false),
                ),
            });
            spawn_media_watchdog(app.handle().clone());
            watch_drives(app.handle().clone());
            // Debug builds only: MORI_DEBUG_FREEZE_AT=<secs> wedges the page's JS
            // for 60 s at that time, to test hang detection and recovery.
            if cfg!(debug_assertions) {
                if let Some(secs) = std::env::var("MORI_DEBUG_FREEZE_AT").ok().and_then(|v| v.parse::<u64>().ok()) {
                    let handle = app.handle().clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(secs));
                        if let Some(w) = handle.get_webview_window("main") {
                            eprintln!("mori: DEBUG freezing the page now");
                            let _ = w.eval("const __u = Date.now() + 60000; while (Date.now() < __u) {}");
                        }
                    });
                }
            }
            build_window(app)?;
            if cfg!(debug_assertions) {
                debug_similar_autorun(app.handle());
                metascan::debug_autorun(app.handle());
                health::debug_autorun(app.handle());
                debug_org(app.handle());
                debug_ops(app.handle());
                debug_transfer(app.handle());
                debug_privacy(app.handle());
                debug_ephemeral(app.handle());
                if std::env::var_os("MORI_DEBUG_DIAG").is_some() {
                    let h = app.handle().clone();
                    std::thread::spawn(move || {
                        let checks = tauri::async_runtime::block_on(run_diagnostics(h.clone()));
                        for c in checks {
                            eprintln!(
                                "mori: DEBUG diag [{}] {:?} {} = {} — {}",
                                c.section, c.status, c.label, c.value, c.detail
                            );
                        }
                        eprintln!("mori: DEBUG diag done");
                        h.exit(0);
                    });
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            plan_transfer,
            transfer_items,
            transfer_cancel,
            transfer_choose_folder,
            cleanup_saved,
            cleanup_save,
            cleanup_discard,
            init,
            update_settings,
            choose_root,
            rescan,
            clear_cache,
            get_status,
            query,
            stats,
            subfolders,
            inspect,
            preview_info,
            read_text,
            open_file,
            reveal_file,
            copy_path,
            store_frame,
            video_session_start,
            ui_alive,
            video_session_end,
            take_recovered,
            trash_summary,
            trash_items,
            rename_item,
            analysis_locations,
            analysis_choose_folder,
            analysis_start,
            analysis_cancel,
            analysis_results,
            analysis_clear,
            analysis_cleanup,
            analysis_cleanup_cancel,
            similar_start,
            similar_cancel,
            similar_frames,
            similar_results,
            similar_dismiss,
            similar_dismissed_count,
            similar_forget_decisions,
            similar_clear,
            similar_cleanup,
            set_folder_private,
            file_report,
            set_read_only,
            set_folder_protected,
            pdf_info,
            archive_listing,
            open_drive_safely,
            set_drive_previews,
            set_capture_override,
            storage_report,
            set_favorite,
            plan_operation,
            checksum_file,
            run_diagnostics,
            local_data,
            clear_mori_data,
            reset_mori,
            checksum_cancel,
            compare_files,
            integrity_save,
            integrity_verify,
            integrity_pause,
            integrity_cancel,
            integrity_list,
            integrity_delete,
            delete_items,
            history_list,
            history_undo,
            tags_list,
            tag_items,
            tag_rename,
            tag_delete,
            open_temporary,
            end_temporary,
            start_private_inspection,
            forget_drive,
            clear_session_data,
            empty_folders,
            trash_empty_folders,
            health::health_start,
            health::health_pause,
            health::health_cancel,
            health::health_clear,
            health::health_results,
            metascan::file_metadata,
            metascan::meta_start,
            metascan::meta_pause,
            metascan::meta_cancel,
            metascan::meta_clear,
            metascan::meta_results,
            metascan::meta_places,
            metascan::sanitize_copies
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mori")
        .run(|_, event| {
            // macOS's folder picker records the last folder in Mori's
            // preferences, sometimes after Mori's own removal; clear it on quit.
            if let tauri::RunEvent::Exit = event {
                forget_open_panel_location();
            }
        });
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn forgetting_only_ever_removes_inside_mori_directories() {
        let data = Path::new("/Users/x/Library/Application Support/app.mori.viewer");
        let cache = Path::new("/Users/x/Library/Caches/app.mori.viewer");
        assert!(super::inside_app_dirs(&data.join("index-v2/abc.json"), data, cache));
        assert!(super::inside_app_dirs(&cache.join("thumbs/v1234"), data, cache));
        assert!(!super::inside_app_dirs(data, data, cache), "never the directory itself");
        assert!(!super::inside_app_dirs(Path::new("/Volumes/Drive/photo.jpg"), data, cache));
        assert!(!super::inside_app_dirs(&data.join("../../../../Volumes/Drive"), data, cache));
        assert!(!super::inside_app_dirs(Path::new("/x/y"), Path::new("/x"), cache), "a too-short base never qualifies");
    }

    use super::*;

    #[test]
    fn detects_external_volume() {
        let exe = Path::new("/Volumes/My Drive/Mori/Mori.app/Contents/MacOS/mori");
        assert_eq!(portable_root_for(exe), Some(PathBuf::from("/Volumes/My Drive")));
        assert_eq!(portable_root_for(Path::new("/Applications/Mori.app/Contents/MacOS/mori")), None);
    }
}
