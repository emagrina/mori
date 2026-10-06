// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod index;
mod probe;
mod protocol;
mod secure;
mod thumbs;
mod video;
mod worker;

use index::{Index, Item, Query};
use protocol::PreviewCache;
use secure::Detected;
use serde::{Deserialize, Serialize};
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
    root: RwLock<Option<RootInfo>>,
    index: RwLock<Arc<Index>>,
    scan_gen: AtomicU64,
    scanning: AtomicBool,
    scan_count: AtomicUsize,
    settings_lock: Mutex<()>,
    pub previews: Mutex<PreviewCache>,
    pub video: video::VideoGuard,
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
            let root = self.root_canon();
            // Re-open through the confinement check; never trust `canon` alone.
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

    /// Open an indexed file by id, confined to the root.
    fn open_by_id(&self, id: &str) -> Result<(fs::File, fs::Metadata, PathBuf, String), String> {
        let root = self.root_canon().ok_or("No folder selected")?;
        let idx = self.index();
        let entry = idx.get(id).ok_or("Unknown file")?;
        if entry.kind == index::Kind::Folder {
            return Err("Not a file".into());
        }
        let (f, m, canon) = secure::open_inside(&root, &entry.path).map_err(|_| "File unavailable")?;
        Ok((f, m, canon, entry.ext.clone()))
    }

    /// Canonical path of an indexed file or folder ("" = root), confined to the root.
    fn path_by_id(&self, id: &str) -> Result<PathBuf, String> {
        let root = self.root_canon().ok_or("No folder selected")?;
        if id.is_empty() {
            return Ok(root);
        }
        let idx = self.index();
        let rel = &idx.get(id).ok_or("Unknown item")?.path;
        if Path::new(rel).components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err("Invalid path".into());
        }
        let canon = fs::canonicalize(root.join(rel)).map_err(|_| "Item unavailable")?;
        if !canon.starts_with(&root) {
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

fn publish_index(app: &AppHandle, idx: Index) {
    *app.state::<AppState>().index.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(idx);
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
    for dir in [&state.thumb_dir, &state.index_dir()] {
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
    let name = state.index().get(&id).map(|e| secure::display_safe(&e.name)).unwrap_or_default();
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
                root: RwLock::new(None),
                index: RwLock::new(Arc::new(Index::default())),
                scan_gen: AtomicU64::new(0),
                scanning: AtomicBool::new(false),
                scan_count: AtomicUsize::new(0),
                settings_lock: Mutex::new(()),
                previews: Mutex::new(PreviewCache::default()),
                video: video::VideoGuard::new(&data_dir_for_video),
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
            take_recovered
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
