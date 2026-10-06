//! Video safety: decide *before* the webview sees a file whether it can be
//! played safely, and contain the damage if the system player still fails.
//!
//! 1. Probe: the container's codec is identified by the sandboxed worker
//!    (`probe.rs`). Only codecs measured to work in this platform's webview
//!    are ever streamed. (On macOS, for example, an AV1 WebM wedges WebKit's
//!    content process, and `hev1` HEVC / AV1-in-MP4 fail outright.)
//! 2. One media session at a time, with heartbeats from the UI. If the UI
//!    stops responding while a video is loading, or WebKit's content process
//!    dies, the file is put on a persistent blocklist and the UI is reloaded,
//!    so the same file can never take Mori down twice.

use crate::secure::Detected;
use crate::worker::{self, Op, OutFormat};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

const PROBE_TIMEOUT: Duration = Duration::from_secs(6);
/// Largest `moov` box we're willing to hand to the worker.
const MAX_MOOV: u64 = 48 * 1024 * 1024;
/// How much of a WebM file the worker sees (headers live at the start).
const WEBM_HEAD: u64 = 2 * 1024 * 1024;
/// UI silence (no liveness ping) that counts as a hung web content process.
pub const HANG_AFTER: Duration = Duration::from_secs(10);
/// A video whose session ended this recently is still a suspect (tearing
/// down a bad decoder can itself wedge WebKit).
const SUSPECT_WINDOW: Duration = Duration::from_secs(30);

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum VideoStatus {
    Playable,
    /// Container fine, codec not on this platform's allow-list.
    UnsupportedCodec,
    /// No video track (e.g. an audio-only .mp4).
    NoVideo,
    /// Structurally broken, truncated, or too complex to probe safely.
    Damaged,
    /// Previously crashed or hung the system player.
    Blocked,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfo {
    pub status: VideoStatus,
    pub container: &'static str,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

impl VideoInfo {
    fn bare(status: VideoStatus, container: &'static str) -> VideoInfo {
        VideoInfo { status, container, video_codec: None, audio_codec: None, width: 0, height: 0, duration_ms: 0 }
    }
}

/// Video codecs the system webview decodes reliably (measured, not assumed).
fn codec_allowed(container: Detected, codec: &str) -> bool {
    // Debug builds only: force a codec through, to exercise the hang/crash recovery.
    if cfg!(debug_assertions)
        && std::env::var("MORI_DEBUG_ALLOW_CODECS").is_ok_and(|v| v.split(',').any(|c| c == codec))
    {
        return true;
    }
    #[cfg(target_os = "macos")]
    let (mp4, webm): (&[&str], &[&str]) = (
        // H.264, HEVC (hvc1 only: WebKit rejects hev1), MPEG-4 Part 2, MJPEG, ProRes.
        &["avc1", "avc3", "hvc1", "mp4v", "jpeg", "mjpa", "apch", "apcn", "apcs", "apco", "ap4h", "ap4x"],
        &["V_VP8", "V_VP9"],
    );
    #[cfg(windows)]
    let (mp4, webm): (&[&str], &[&str]) = (&["avc1", "avc3", "vp09", "av01"], &["V_VP8", "V_VP9", "V_AV1"]);
    #[cfg(all(unix, not(target_os = "macos")))]
    let (mp4, webm): (&[&str], &[&str]) = (&["avc1", "avc3"], &["V_VP8", "V_VP9"]);
    match container {
        Detected::Mp4 | Detected::Mov => mp4.contains(&codec),
        Detected::Webm => webm.contains(&codec),
        _ => false,
    }
}

/// Walk top-level MP4 box headers (8/16 bytes each, nothing else is read)
/// to find the `moov` box, which may sit at the very end of the file.
fn find_moov(file: &mut File, len: u64) -> Option<(u64, u64)> {
    let mut pos = 0u64;
    for _ in 0..4096 {
        if pos.checked_add(8)? > len {
            return None;
        }
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 16];
        file.read_exact(&mut h[..8]).ok()?;
        let mut size = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
        let mut header = 8;
        if size == 1 {
            file.read_exact(&mut h[8..16]).ok()?;
            size = u64::from_be_bytes(h[8..16].try_into().unwrap());
            header = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header || pos.checked_add(size)? > len {
            return None; // inconsistent or truncated: treat as damaged
        }
        if &h[4..8] == b"moov" {
            return Some((pos + header, size - header));
        }
        pos += size;
    }
    None
}

fn parse_probe(text: &[u8]) -> Option<(Option<String>, Option<String>, u64)> {
    let text = std::str::from_utf8(text).ok()?;
    let (mut video, mut audio, mut duration) = (None, None, 0);
    for line in text.lines() {
        let (k, v) = line.split_once('=')?;
        if v.len() > 32 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')) {
            return None;
        }
        match k {
            "video" if !v.is_empty() => video = Some(v.to_string()),
            "audio" if !v.is_empty() => audio = Some(v.to_string()),
            "duration_ms" => duration = v.parse().ok()?,
            "video" | "audio" | "width" | "height" => {}
            _ => return None,
        }
    }
    Some((video, audio, duration))
}

pub fn probe_file_unavailable() -> VideoInfo {
    VideoInfo::bare(VideoStatus::Damaged, "unknown")
}

/// Probe an already-opened (and confined) video file. Never decodes frames.
pub fn probe_file(mut file: File, len: u64, detected: Detected) -> VideoInfo {
    let container = match detected {
        Detected::Mp4 => "MP4",
        Detected::Mov => "QuickTime",
        Detected::Webm => "WebM",
        _ => return VideoInfo::bare(VideoStatus::Damaged, "unknown"),
    };
    let mut input = Vec::new();
    match detected {
        Detected::Webm => {
            input.extend_from_slice(b"WEBM");
            if file.seek(SeekFrom::Start(0)).is_err() || file.by_ref().take(WEBM_HEAD).read_to_end(&mut input).is_err()
            {
                return VideoInfo::bare(VideoStatus::Damaged, container);
            }
        }
        _ => {
            let Some((at, size)) = find_moov(&mut file, len) else {
                return VideoInfo::bare(VideoStatus::Damaged, container);
            };
            if size > MAX_MOOV {
                return VideoInfo::bare(VideoStatus::Damaged, container);
            }
            input.extend_from_slice(b"MP4\0");
            input.reserve(size as usize);
            if file.seek(SeekFrom::Start(at)).is_err() || file.by_ref().take(size).read_to_end(&mut input).is_err() {
                return VideoInfo::bare(VideoStatus::Damaged, container);
            }
        }
    }
    let out = match worker::run(Op::Probe, 256, input, PROBE_TIMEOUT) {
        Ok(out) if out.format == OutFormat::Probe => out,
        _ => return VideoInfo::bare(VideoStatus::Damaged, container),
    };
    let Some((video, audio, duration_ms)) = parse_probe(&out.bytes) else {
        return VideoInfo::bare(VideoStatus::Damaged, container);
    };
    let status = match &video {
        None => VideoStatus::NoVideo,
        Some(c) if !codec_allowed(detected, c) => VideoStatus::UnsupportedCodec,
        // A video track with no usable dimensions is corrupt (WebKit "plays" it as 0×0).
        Some(_) if out.width == 0 || out.height == 0 || out.width > 16384 || out.height > 16384 => VideoStatus::Damaged,
        Some(_) => VideoStatus::Playable,
    };
    VideoInfo {
        status,
        container,
        video_codec: video,
        audio_codec: audio,
        width: out.width,
        height: out.height,
        duration_ms,
    }
}

// --------------------------------------------------------------- sessions

pub struct Session {
    pub token: u64,
    pub key: String,
    pub name: String,
}

/// Probe cache, blocklist and the single active media session.
pub struct VideoGuard {
    blocklist_file: PathBuf,
    blocked: Mutex<HashSet<String>>,
    probes: Mutex<HashMap<String, VideoInfo>>,
    pub session: Mutex<Option<Session>>,
    /// The last video session that ended, and when.
    recent: Mutex<Option<(Session, Instant)>>,
    /// Last liveness ping from the UI; `None` until the UI has (re)loaded.
    last_ui_beat: Mutex<Option<Instant>>,
    next_token: Mutex<u64>,
    /// Names of files blocked by the last recovery, for a one-time notice.
    pub recovered: Mutex<Vec<String>>,
}

impl VideoGuard {
    pub fn new(data_dir: &std::path::Path) -> VideoGuard {
        let blocklist_file = data_dir.join("blocked-media.json");
        let blocked = fs::read(&blocklist_file)
            .ok()
            .and_then(|d| serde_json::from_slice::<Vec<String>>(&d).ok())
            .unwrap_or_default()
            .into_iter()
            .take(10_000)
            .collect();
        VideoGuard {
            blocklist_file,
            blocked: Mutex::new(blocked),
            probes: Mutex::new(HashMap::new()),
            session: Mutex::new(None),
            recent: Mutex::new(None),
            last_ui_beat: Mutex::new(None),
            next_token: Mutex::new(0),
            recovered: Mutex::new(Vec::new()),
        }
    }

    /// Cached probe for a file version (`key` = thumbnail-cache stem string).
    pub fn info(&self, key: &str, probe: impl FnOnce() -> VideoInfo) -> VideoInfo {
        if self.blocked.lock().unwrap_or_else(PoisonError::into_inner).contains(key) {
            return VideoInfo::bare(VideoStatus::Blocked, "video");
        }
        if let Some(hit) = self.probes.lock().unwrap_or_else(PoisonError::into_inner).get(key) {
            return hit.clone();
        }
        let info = probe();
        self.probes.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_owned(), info.clone());
        info
    }

    pub fn block(&self, key: &str) {
        let mut b = self.blocked.lock().unwrap_or_else(PoisonError::into_inner);
        b.insert(key.to_owned());
        let list: Vec<&String> = b.iter().collect();
        let _ = fs::write(&self.blocklist_file, serde_json::to_vec(&list).unwrap_or_default());
        self.probes.lock().unwrap_or_else(PoisonError::into_inner).remove(key);
    }

    pub fn clear(&self) {
        self.blocked.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.probes.lock().unwrap_or_else(PoisonError::into_inner).clear();
        let _ = fs::remove_file(&self.blocklist_file);
    }

    pub fn start(&self, key: String, name: String) -> u64 {
        let mut t = self.next_token.lock().unwrap_or_else(PoisonError::into_inner);
        *t += 1;
        let prev =
            self.session.lock().unwrap_or_else(PoisonError::into_inner).replace(Session { token: *t, key, name });
        if let Some(prev) = prev {
            *self.recent.lock().unwrap_or_else(PoisonError::into_inner) = Some((prev, Instant::now()));
        }
        *t
    }

    pub fn end(&self, token: u64) {
        let mut s = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if s.as_ref().is_some_and(|s| s.token == token) {
            if let Some(ended) = s.take() {
                *self.recent.lock().unwrap_or_else(PoisonError::into_inner) = Some((ended, Instant::now()));
            }
        }
    }

    pub fn ui_beat(&self) {
        *self.last_ui_beat.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }

    /// True when the UI has stopped pinging for longer than `HANG_AFTER`.
    pub fn ui_stalled(&self) -> bool {
        self.last_ui_beat.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|t| t.elapsed() > HANG_AFTER)
    }

    /// Background windows have throttled timers: restart the silence timer
    /// (if armed) so a hang is judged only once the window is back in front.
    pub fn postpone(&self) {
        if let Some(t) = self.last_ui_beat.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            *t = Instant::now();
        }
    }

    /// Has the UI pinged since it was last disarmed?
    pub fn ui_pinged(&self) -> bool {
        self.last_ui_beat.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }

    /// Forget the UI's liveness until it pings again (after a restart, or
    /// while the window is in the background and timers are throttled).
    pub fn disarm(&self) {
        *self.last_ui_beat.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The web content process died or hung: blocklist the video that was
    /// playing or had just been torn down, if any. Returns its name.
    pub fn fail_active(&self) -> Option<String> {
        let active = self.session.lock().unwrap_or_else(PoisonError::into_inner).take();
        let suspect = active.or_else(|| {
            self.recent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .filter(|(_, ended)| ended.elapsed() < SUSPECT_WINDOW)
                .map(|(s, _)| s)
        })?;
        self.block(&suspect.key);
        self.recovered.lock().unwrap_or_else(PoisonError::into_inner).push(suspect.name.clone());
        Some(suspect.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_is_validated() {
        assert_eq!(
            parse_probe(b"video=avc1\naudio=mp4a\nwidth=1\nheight=1\nduration_ms=42\n"),
            Some((Some("avc1".into()), Some("mp4a".into()), 42))
        );
        assert_eq!(parse_probe(b"video=<script>\n"), None);
        assert_eq!(parse_probe(b"evil=1\n"), None);
    }

    #[test]
    fn allow_list_is_platform_specific() {
        assert!(codec_allowed(Detected::Mp4, "avc1"));
        assert!(!codec_allowed(Detected::Unknown, "avc1"));
        #[cfg(target_os = "macos")]
        {
            assert!(!codec_allowed(Detected::Webm, "V_AV1"), "wedges WebKit on Apple silicon without AV1");
            assert!(!codec_allowed(Detected::Mp4, "hev1"));
            assert!(!codec_allowed(Detected::Mp4, "av01"));
        }
    }

    #[test]
    fn sessions_and_blocklist() {
        let dir = std::env::temp_dir().join(format!("mori-video-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let g = VideoGuard::new(&dir);
        let a = g.start("A".into(), "a.mp4".into());
        let b = g.start("B".into(), "b.mp4".into());
        g.end(a); // a stale session ending must not end the newer one
        assert_eq!(g.session.lock().unwrap_or_else(PoisonError::into_inner).as_ref().map(|s| s.token), Some(b));
        assert_eq!(g.fail_active().as_deref(), Some("b.mp4"));
        assert_eq!(g.info("B", || unreachable!()).status, VideoStatus::Blocked);
        // A video that *just* ended is still blamed for a hang during its teardown.
        let c = g.start("C".into(), "c.mp4".into());
        g.end(c);
        assert_eq!(g.fail_active().as_deref(), Some("c.mp4"));
        assert_eq!(g.fail_active(), None, "nothing left to blame");
        // Liveness is not armed until the first ping.
        assert!(!g.ui_stalled());
        g.ui_beat();
        assert!(!g.ui_stalled());
        // Blocklist persists across restarts.
        assert_eq!(VideoGuard::new(&dir).info("B", || unreachable!()).status, VideoStatus::Blocked);
        fs::remove_dir_all(dir).unwrap();
    }
}
