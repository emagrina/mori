// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dupes;
mod fileops;
#[cfg(target_os = "macos")]
mod heif;
mod index;
mod privacy;
mod probe;
mod protocol;
mod secure;
mod similar;
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

    pub fn root_canon(&self) -> Option<PathBuf> {
        self.root.read().unwrap_or_else(PoisonError::into_inner).as_ref().map(|r| r.canon.clone())
    }

    pub fn index(&self) -> Arc<Index> {
        self.index.read().unwrap_or_else(PoisonError::into_inner).clone()
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
            });
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
        v
    }

    /// Open a file by id, confined to its root.
    fn open_by_id(&self, id: &str) -> Result<(fs::File, fs::Metadata, PathBuf, String), String> {
        let loc = self.locate(id)?;
        if loc.is_dir {
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
        let canon = fs::canonicalize(loc.root.join(&loc.rel)).map_err(|_| "Item unavailable")?;
        if !canon.starts_with(&loc.root) {
            return Err("Item unavailable".into());
        }
        Ok(canon)
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
        if index::save(&state.index_file(&root), &idx).is_err() {
            debug_log!("mori: could not save index");
        }
        // Private folders moved or renamed outside Mori: follow them by inode.
        let dirs: Vec<(&str, u64)> = idx.dirs.iter().map(|d| (d.path.as_str(), d.ino)).collect();
        state.privacy.reconcile(&root, &dirs);
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
    let Some(picked) = app.dialog().file().set_title("Choose a drive or folder for Mori").blocking_pick_folder() else {
        return Err("cancelled".into());
    };
    let path = picked.into_path().map_err(|_| "That folder can't be opened")?;
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
    let preview = if detected.is_image() {
        "image"
    } else if video.as_ref().is_some_and(|v| v.status == video::VideoStatus::Playable) {
        "video"
    } else if detected == Detected::Text {
        "text"
    } else {
        "none"
    };
    // A file whose content contradicts its name is suspicious: never hand it to another app.
    let can_open = !mismatch && secure::may_open_externally(&ext, detected);
    Ok(Inspection { detected, preview, can_open, mismatch, video })
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
    let tauri::ipc::InvokeBody::Raw(body) = request.body() else { return Ok(false) };
    let body = body.clone();
    // The worker can take seconds; never block the async runtime or the UI thread.
    tauri::async_runtime::spawn_blocking(move || protocol::store_frame(&app.state::<AppState>(), &id, body))
        .await
        .map_err(|_| ())
}

// ------------------------------------------------------- file management

/// Remove a file's cached thumbnails (it is about to disappear or change name).
fn invalidate_thumbs(state: &AppState, canon: &Path, meta: &fs::Metadata) {
    let stem = thumbs::stem(&state.thumb_dir, canon, meta, protocol::THUMB_SIZE);
    for ext in ["jpg", "png", "none"] {
        let _ = fs::remove_file(stem.with_extension(ext));
    }
}

/// Apply an in-place change to the browser index, persist it and refresh the UI.
fn apply_index_change(app: &AppHandle, change: impl FnOnce(&mut Index)) {
    let state = app.state::<AppState>();
    let mut idx = (*state.index()).clone();
    change(&mut idx);
    if let Some(root) = state.root_canon() {
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
            match fileops::move_to_trash(&path) {
                Ok(()) => {
                    out.trashed.push(id.clone());
                    out.bytes += bytes;
                    if let Some(rel) = browser_rel(&state, &path) {
                        removed_rels.push(rel);
                    }
                    removed_abs.push(path);
                }
                Err(e) => out.failed.push(dupes::Failure { path: shown, reason: e }),
            }
        }
        if !removed_rels.is_empty() {
            apply_index_change(&app, |idx| idx.remove_paths(&removed_rels));
        }
        forget_removed(&state, &removed_abs);
        out
    })
    .await
    .map_err(|_| "The operation failed.".to_string())
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
    fileops::rename_no_replace(&path, &target)?;
    if meta.is_dir() {
        // A private folder (or one containing private folders) keeps its privacy.
        state.privacy.renamed(&path, &target);
    }
    let old_rel = loc.rel.clone();
    let new_rel = match old_rel.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{name}"),
        None => name.clone(),
    };
    apply_index_change(&app, |idx| idx.rename_path(&old_rel, &new_rel));
    Ok(index::id_str(index::id_for(&new_rel)))
}

// ---------------------------------------------------------- private folders

/// Make a browser folder private (a visibility boundary) or public again.
/// Only Mori's own records change; the folder itself is never touched.
#[tauri::command]
async fn set_folder_private(app: AppHandle, id: String, private: bool) -> Result<(), String> {
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
    let picked =
        app.dialog().file().set_title("Choose a folder to analyze").blocking_pick_folder().ok_or("cancelled")?;
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
    let mut roots: Vec<dupes::Root> = Vec::new();
    for key in locations.iter().take(32) {
        if let Some((canon, label)) = location_path(&app, &state, key) {
            if !roots.iter().any(|r| r.canon == canon) {
                let private = state.privacy.boundaries(&canon);
                roots.push(dupes::Root { canon, label, private });
            }
        }
    }
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
                    fileops::move_to_trash(&path)
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
    return false;
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
    let mut roots: Vec<dupes::Root> = Vec::new();
    for key in locations.iter().take(32) {
        if let Some((canon, label)) = location_path(&app, &state, key) {
            if !roots.iter().any(|r| r.canon == canon) {
                let private = state.privacy.boundaries(&canon);
                roots.push(dupes::Root { canon, label, private });
            }
        }
    }
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
                cache_dir: Some(state.similar_dir()),
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
            Ok(Ok(result)) => {
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
            eprintln!("mori: DEBUG group video={} {}% :: {}", m.video, m.similarity, names.join(" | "));
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
    if pairs.is_empty() {
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
                    fileops::move_to_trash(&path)
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
            app.manage(AppState {
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
            });
            spawn_media_watchdog(app.handle().clone());
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
                debug_privacy(app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
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
            set_folder_private
        ])
        .run(tauri::generate_context!())
        .expect("error while running Mori");
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn detects_external_volume() {
        let exe = Path::new("/Volumes/My Drive/Mori/Mori.app/Contents/MacOS/mori");
        assert_eq!(portable_root_for(exe), Some(PathBuf::from("/Volumes/My Drive")));
        assert_eq!(portable_root_for(Path::new("/Applications/Mori.app/Contents/MacOS/mori")), None);
    }
}
