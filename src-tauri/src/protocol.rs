//! `mori://` — the only way the webview receives anything derived from files.
//!
//!   mori://localhost/thumb/<id>    worker-generated thumbnail (JPEG/PNG)
//!   mori://localhost/preview/<id>  worker-generated preview (JPEG/PNG/GIF)
//!   mori://localhost/media/<id>    byte ranges of a verified MP4/MOV/WebM file
//!
//! (On Windows the same routes are served from http://mori.localhost/.)
//! Requests carry opaque ids only; there is no way to ask for a path.

use crate::secure::{self, Detected};
use crate::worker::{self, Op, Output, WorkerError};
use crate::AppState;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::PoisonError;
use std::time::Duration;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Manager};

pub const THUMB_SIZE: u32 = 512;
const PREVIEW_SIZE: u32 = 3072;
const THUMB_TIMEOUT: Duration = Duration::from_secs(12);
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(25);
/// Largest slice of a video served per request.
const MEDIA_CHUNK: u64 = 4 * 1024 * 1024;

const ALLOWED_ORIGINS: &[&str] = &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost"];

pub fn handle(app: &AppHandle, req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let state = app.state::<AppState>();
    let route = percent_decode(req.uri().path());
    let route = route.trim_start_matches('/');
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| ALLOWED_ORIGINS.contains(o) || (cfg!(debug_assertions) && *o == "http://127.0.0.1:1420"))
        .map(str::to_owned);

    // "iso-…" routes serve the isolated view: an explicit, per-file request
    // that is allowed even in Safe Inspection Mode. Everything else respects it.
    let (kind, rest) = route.split_once('/').unwrap_or((route, ""));
    let (kind, explicit) = match kind.strip_prefix("iso-") {
        Some(k) => (k, true),
        None => (kind, false),
    };
    let mut resp = match kind {
        "thumb" => thumbnail(&state, rest, explicit),
        "preview" => preview(&state, rest, explicit),
        "media" => media(&state, rest, req.headers().get(header::RANGE).and_then(|v| v.to_str().ok()), explicit),
        "frame" => frame(&state, rest, explicit),
        "pdf" => pdf_page(&state, rest, explicit),
        _ => status(StatusCode::NOT_FOUND),
    };
    let h = resp.headers_mut();
    h.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
    // If the bytes were ever navigated to directly, they still can't run anything.
    h.insert("Content-Security-Policy", "default-src 'none'; sandbox".parse().unwrap());
    h.insert("Referrer-Policy", "no-referrer".parse().unwrap());
    if let Some(o) = origin.and_then(|o| o.parse().ok()) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
    }
    resp
}

pub fn internal_error() -> Response<Vec<u8>> {
    status(StatusCode::INTERNAL_SERVER_ERROR)
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(code).body(Vec::new()).unwrap()
}

fn ok(bytes: Vec<u8>, mime: &str, cache: bool) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, if cache { "private, max-age=86400" } else { "no-store" })
        .body(bytes)
        .unwrap()
}

/// Open a file by id, re-validating that it lives inside the root.
/// Open a browser or analyzer file by id, confined to its authorised root.
fn open(state: &AppState, id: &str) -> Option<(File, std::fs::Metadata, std::path::PathBuf, String)> {
    let loc = state.locate(id).ok()?;
    if loc.is_dir || loc.is_link {
        return None;
    }
    let (file, meta, canon) = secure::open_inside(&loc.root, &loc.rel).ok()?;
    Some((file, meta, canon, loc.ext))
}

fn sniff_file(file: &mut File, ext: &str) -> Detected {
    let head = secure::read_head(file, secure::SNIFF_LEN);
    let _ = file.seek(SeekFrom::Start(0));
    secure::sniff(&head, ext)
}

/// Files outside the browsed folder (analyzer results elsewhere) never get
/// their thumbnails written to disk.
fn volatile(state: &AppState, canon: &std::path::Path) -> bool {
    // A temporary session writes nothing: thumbnails stay in memory.
    state.temp.load(std::sync::atomic::Ordering::SeqCst)
        || state.root_canon().is_none_or(|root| !canon.starts_with(root))
}

/// Safe Inspection Mode: no automatic decoding of files under the browsed
/// root until the user allows previews for the drive.
fn gated(state: &AppState, canon: &std::path::Path, explicit: bool) -> bool {
    !explicit
        && state.safe_mode.load(std::sync::atomic::Ordering::SeqCst)
        && state.root_canon().is_some_and(|r| canon.starts_with(r))
}

fn decoded(state: &AppState) {
    state.decoded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

fn thumbnail(state: &AppState, id: &str, explicit: bool) -> Response<Vec<u8>> {
    let Some((mut file, meta, canon, ext)) = open(state, id) else { return status(StatusCode::NOT_FOUND) };
    if gated(state, &canon, explicit) {
        return status(StatusCode::FORBIDDEN);
    }
    let stem = crate::thumbs::stem(&state.thumb_dir_for(&canon), &canon, &meta, THUMB_SIZE);
    let mem = volatile(state, &canon);
    if mem {
        match state.volatile_thumbs.get(&stem) {
            Some(Some((bytes, mime))) => return ok(bytes, mime, true),
            Some(None) => return status(StatusCode::NOT_FOUND),
            None => {}
        }
    } else {
        if let Some((bytes, mime)) = crate::thumbs::cached(&stem) {
            return ok(bytes, mime, true);
        }
        if crate::thumbs::is_miss(&stem) {
            return status(StatusCode::NOT_FOUND);
        }
    }
    let miss = |stem: &std::path::Path| {
        if mem {
            state.volatile_thumbs.put(stem, None);
        } else {
            crate::thumbs::mark_miss(stem); // never retried automatically
        }
    };
    if !sniff_file(&mut file, &ext).is_image() {
        // Videos get a thumbnail from a webview-captured frame (see `store_frame`).
        return status(StatusCode::NOT_FOUND);
    }
    let Ok(input) = secure::read_limited(file, &meta, worker::MAX_INPUT) else {
        miss(&stem);
        return status(StatusCode::PAYLOAD_TOO_LARGE);
    };
    match worker::run(Op::Thumb, THUMB_SIZE, input, THUMB_TIMEOUT) {
        Ok(out) => {
            decoded(state);
            if mem {
                state.volatile_thumbs.put(&stem, Some((out.bytes.clone(), out.format.mime())));
            } else {
                crate::thumbs::store(&stem, &out);
            }
            ok(out.bytes, out.format.mime(), true)
        }
        Err(_) => {
            miss(&stem);
            status(StatusCode::UNPROCESSABLE_ENTITY)
        }
    }
}

fn preview(state: &AppState, id: &str, explicit: bool) -> Response<Vec<u8>> {
    let Some((mut file, meta, canon, ext)) = open(state, id) else { return status(StatusCode::NOT_FOUND) };
    if gated(state, &canon, explicit) {
        return status(StatusCode::FORBIDDEN);
    }
    let key =
        crate::thumbs::stem(&state.thumb_dir_for(&canon), &canon, &meta, PREVIEW_SIZE).to_string_lossy().into_owned();
    if let Some(hit) = state.previews.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return hit;
    }
    if !sniff_file(&mut file, &ext).is_image() {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let result = secure::read_limited(file, &meta, worker::MAX_INPUT)
        .map_err(|_| WorkerError::Failed)
        .and_then(|input| worker::run(Op::Preview, PREVIEW_SIZE, input, PREVIEW_TIMEOUT));
    let mut cache = state.previews.lock().unwrap_or_else(PoisonError::into_inner);
    match result {
        Ok(out) => {
            decoded(state);
            cache.put(id, key.clone(), out);
            cache.get(&key).unwrap()
        }
        Err(e) => {
            cache.fail(key.clone());
            status(if e == WorkerError::Unsupported {
                StatusCode::UNSUPPORTED_MEDIA_TYPE
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            })
        }
    }
}

fn media(state: &AppState, id: &str, range: Option<&str>, explicit: bool) -> Response<Vec<u8>> {
    let Some((mut file, meta, canon, ext)) = open(state, id) else { return status(StatusCode::NOT_FOUND) };
    if gated(state, &canon, explicit) {
        return status(StatusCode::FORBIDDEN);
    }
    // Only containers verified by magic bytes, whose codec passed the sandboxed
    // probe, are ever handed to the system player.
    let detected = sniff_file(&mut file, &ext);
    let Some(mime) = detected.video_mime() else {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    };
    if !state.video_playable(&canon, &meta, detected) {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let len = meta.len();
    if len == 0 {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let (start, end) = match range.and_then(parse_range) {
        Some((s, e)) => (s, e.unwrap_or(u64::MAX)),
        None => (0, u64::MAX),
    };
    if start >= len {
        return Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(Vec::new())
            .unwrap();
    }
    let end = end.min(len - 1).min(start + MEDIA_CHUNK - 1);
    let mut buf = vec![0u8; (end - start + 1) as usize];
    if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut buf).is_err() {
        return status(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
        .header(header::CACHE_CONTROL, "no-store")
        .body(buf)
        .unwrap()
}

/// `bytes=START-[END]` only; suffix and multi-ranges are refused.
fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let spec = v.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (s, e) = spec.split_once('-')?;
    let start = s.trim().parse().ok()?;
    let end = match e.trim() {
        "" => None,
        e => Some(e.parse().ok().filter(|&e: &u64| e >= start)?),
    };
    Some((start, end))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Turn a webview-captured PNG frame into a cached video thumbnail, via the worker.
/// Size of the sampled video frames (filmstrip, hover scrub, isolated view).
pub const FRAME_SIZE: u32 = 480;
/// Cache tag for frame `k` (kept apart from thumbnail sizes).
fn frame_tag(k: u32) -> u32 {
    100_000 + k
}

/// Store a frame captured by the webview: the thumbnail (`frame` = None) or
/// sampled frame `k`, always re-encoded by the sandboxed worker first.
pub fn store_frame(state: &AppState, id: &str, png: Vec<u8>, frame: Option<u32>, explicit: bool) -> bool {
    let Some((mut file, meta, canon, ext)) = open(state, id) else { return false };
    if gated(state, &canon, explicit) || frame.is_some_and(|k| k >= 64) {
        return false;
    }
    let detected = sniff_file(&mut file, &ext);
    if !detected.is_video() || !state.video_playable(&canon, &meta, detected) {
        return false;
    }
    let (size, tag) = match frame {
        Some(k) => (FRAME_SIZE, frame_tag(k)),
        None => (THUMB_SIZE, THUMB_SIZE),
    };
    let stem = crate::thumbs::stem(&state.thumb_dir_for(&canon), &canon, &meta, tag);
    let mem = volatile(state, &canon);
    let result = if png.is_empty() || png.len() > 16 * 1024 * 1024 {
        None
    } else {
        worker::run(Op::Frame, size, png, THUMB_TIMEOUT).ok()
    };
    if result.is_some() {
        decoded(state);
    }
    match (result, mem) {
        (Some(out), true) => state.volatile_thumbs.put(&stem, Some((out.bytes, out.format.mime()))),
        (Some(out), false) => crate::thumbs::store(&stem, &out),
        (None, true) => state.volatile_thumbs.put(&stem, None),
        (None, false) => crate::thumbs::mark_miss(&stem),
    }
    state.volatile_thumbs.get(&stem).is_some_and(|v| v.is_some()) || (!mem && crate::thumbs::cached(&stem).is_some())
}

/// A stored video frame: `frame/<id>/<k>`.
fn frame(state: &AppState, rest: &str, explicit: bool) -> Response<Vec<u8>> {
    let Some((id, k)) = rest.split_once('/') else { return status(StatusCode::NOT_FOUND) };
    let Some(k) = k.parse::<u32>().ok().filter(|k| *k < 64) else { return status(StatusCode::NOT_FOUND) };
    let Some((_, meta, canon, _)) = open(state, id) else { return status(StatusCode::NOT_FOUND) };
    if gated(state, &canon, explicit) {
        return status(StatusCode::FORBIDDEN);
    }
    let stem = crate::thumbs::stem(&state.thumb_dir_for(&canon), &canon, &meta, frame_tag(k));
    if volatile(state, &canon) {
        return match state.volatile_thumbs.get(&stem) {
            Some(Some((bytes, mime))) => ok(bytes, mime, true),
            _ => status(StatusCode::NOT_FOUND),
        };
    }
    match crate::thumbs::cached(&stem) {
        Some((bytes, mime)) => ok(bytes, mime, true),
        None => status(StatusCode::NOT_FOUND),
    }
}

/// A PDF page rasterized by the sandboxed worker: `pdf/<id>/<page>/<size>`.
/// Pages are kept in the in-memory preview cache only.
fn pdf_page(state: &AppState, rest: &str, explicit: bool) -> Response<Vec<u8>> {
    let mut parts = rest.split('/');
    let (Some(id), Some(page), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
        return status(StatusCode::NOT_FOUND);
    };
    let (Some(page), Some(size)) = (page.parse::<u32>().ok().filter(|p| *p >= 1), size.parse::<u32>().ok()) else {
        return status(StatusCode::NOT_FOUND);
    };
    let size = size.clamp(64, 2400);
    let Some((mut file, meta, canon, ext)) = open(state, id) else { return status(StatusCode::NOT_FOUND) };
    if gated(state, &canon, explicit) {
        return status(StatusCode::FORBIDDEN);
    }
    let key =
        format!("{}#p{page}", crate::thumbs::stem(&state.thumb_dir_for(&canon), &canon, &meta, size).to_string_lossy());
    if let Some(hit) = state.previews.lock().unwrap_or_else(PoisonError::into_inner).get(&key) {
        return hit;
    }
    if sniff_file(&mut file, &ext) != Detected::Pdf {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let result =
        secure::read_limited(file, &meta, worker::MAX_INPUT).map_err(|_| WorkerError::Failed).and_then(|pdf| {
            let mut input = page.to_le_bytes().to_vec();
            input.extend(pdf);
            worker::run(Op::PdfPage, size, input, PREVIEW_TIMEOUT)
        });
    let mut cache = state.previews.lock().unwrap_or_else(PoisonError::into_inner);
    match result {
        Ok(out) => {
            decoded(state);
            cache.put(&format!("{id}#p{page}"), key.clone(), out);
            cache.get(&key).unwrap()
        }
        Err(_) => {
            cache.fail(key);
            status(StatusCode::UNPROCESSABLE_ENTITY)
        }
    }
}

// --------------------------------------------------------- preview cache

/// Small in-memory LRU of recent previews. Full previews are never written
/// to disk.
#[derive(Default)]
pub struct PreviewCache {
    entries: std::collections::VecDeque<(String, std::sync::Arc<Vec<u8>>, &'static str)>,
    failed: std::collections::HashSet<String>,
    dims: std::collections::HashMap<String, (u32, u32)>,
}

impl PreviewCache {
    const CAPACITY: usize = 40;

    fn get(&mut self, key: &str) -> Option<Response<Vec<u8>>> {
        if self.failed.contains(key) {
            return Some(status(StatusCode::UNPROCESSABLE_ENTITY));
        }
        let pos = self.entries.iter().position(|(k, ..)| k == key)?;
        let entry = self.entries.remove(pos)?;
        let resp = ok(entry.1.as_ref().clone(), entry.2, false);
        self.entries.push_back(entry);
        Some(resp)
    }

    fn put(&mut self, id: &str, key: String, out: Output) {
        self.dims.insert(id.to_owned(), (out.width, out.height));
        self.entries.push_back((key, std::sync::Arc::new(out.bytes), out.format.mime()));
        while self.entries.len() > Self::CAPACITY {
            self.entries.pop_front();
        }
    }

    fn fail(&mut self, key: String) {
        self.failed.insert(key);
    }

    /// Original pixel size of a previewed image (for the info bar).
    pub fn dims(&self, id: &str) -> Option<(u32, u32)> {
        self.dims.get(id).copied()
    }

    pub fn clear(&mut self) {
        *self = PreviewCache::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_decoding() {
        assert_eq!(parse_range("bytes=0-"), Some((0, None)));
        assert_eq!(parse_range("bytes=100-199"), Some((100, Some(199))));
        assert_eq!(parse_range("bytes=200-100"), None);
        assert_eq!(parse_range("bytes=-500"), None);
        assert_eq!(parse_range("bytes=0-1,5-6"), None);
        assert_eq!(percent_decode("/thumb%2F00ff"), "/thumb/00ff");
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }
}
