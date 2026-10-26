//! Risk indicators: objective, explainable anomalies about a file.
//!
//! These are facts ("the name says JPEG, the content is a Mach-O
//! executable"), never verdicts. Mori is not an antivirus and nothing here
//! claims a file is malicious or safe; the absence of findings only means
//! that none of these specific checks found an anomaly.

use crate::filetype::{self, Family, FileType};
use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Attention,
    High,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub level: Level,
    /// Stable machine code ("extension-mismatch"…).
    pub code: &'static str,
    pub title: String,
    /// Why it was flagged.
    pub detail: String,
}

fn f(level: Level, code: &'static str, title: impl Into<String>, detail: impl Into<String>) -> Finding {
    Finding { level, code, title: title.into(), detail: detail.into() }
}

/// Everything the checks may look at. Gathered without decoding anything.
#[derive(Default)]
pub struct Input<'a> {
    pub name: &'a str,
    pub ext: &'a str,
    pub size: u64,
    /// Leading bytes (larger for JPEG, whose size marker can follow big metadata).
    pub head: &'a [u8],
    /// Last bytes of the file (empty if not read).
    pub tail: &'a [u8],
    pub detected: Option<FileType>,
    pub executable_bit: bool,
    pub quarantined: bool,
    /// A previous sandboxed decode of this exact file version failed.
    pub decode_failed: bool,
    /// This video previously hung or crashed the system media engine.
    pub media_blocked: bool,
    /// A video container that could not be parsed.
    pub broken_container: bool,
    pub symlink_target: Option<&'a str>,
    pub symlink_outside: bool,
}

/// Names that hide their real extension behind a harmless-looking one.
pub fn name_findings(name: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    if name
        .chars()
        .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'))
    {
        out.push(f(
            Level::High,
            "bidi-control",
            tr!("Name contains a text-direction control character"),
            tr!("Characters like U+202E can make “photo\u{2026}exe.jpg” display as an image name. Mori shows them as visible symbols."),
        ));
    }
    let lower = name.to_lowercase();
    let parts: Vec<&str> = lower.split('.').collect();
    if parts.len() >= 3 {
        let last = parts[parts.len() - 1].trim();
        let prev = parts[parts.len() - 2].trim();
        let prev_is_doc = filetype::claimed_family(prev).is_some();
        if prev_is_doc && filetype::EXECUTABLE_EXT.contains(&last) {
            out.push(f(
                Level::High,
                "double-extension",
                tr!("Double extension: “.{prev}.{last}”", prev = prev, last = last),
                tr!(
                    "The name looks like a .{prev} file but the real extension is .{last}, which runs code or installs software when opened.",
                    prev = prev,
                    last = last
                ),
            ));
        }
    }
    if name.contains("    ") || name.trim_end() != name {
        out.push(f(
            Level::Attention,
            "padded-name",
            tr!("Name contains a long run of spaces"),
            tr!("Padding like “invoice.pdf          .exe” can push the real extension out of view."),
        ));
    }
    out
}

/// Declared image dimensions read from the header only (no decoding).
pub fn header_dimensions(h: &[u8], id: &str) -> Option<(u64, u64)> {
    let be16 = |i: usize| h.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as u64);
    let le16 = |i: usize| h.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u64);
    let be32 = |i: usize| h.get(i..i + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64);
    let le32 = |i: usize| h.get(i..i + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]).unsigned_abs() as u64);
    match id {
        "png" => Some((be32(16)?, be32(20)?)),
        "gif" => Some((le16(6)?, le16(8)?)),
        "bmp" => Some((le32(18)?, le32(22)?)),
        "webp" => match h.get(12..16)? {
            b"VP8X" => {
                let w = 1 + (h.get(24)?.to_owned() as u64 | (*h.get(25)? as u64) << 8 | (*h.get(26)? as u64) << 16);
                let hh = 1 + (h.get(27)?.to_owned() as u64 | (*h.get(28)? as u64) << 8 | (*h.get(29)? as u64) << 16);
                Some((w, hh))
            }
            b"VP8 " => Some((le16(26)? & 0x3FFF, le16(28)? & 0x3FFF)),
            b"VP8L" => {
                let b = h.get(21..25)?;
                let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                Some(((v & 0x3FFF) as u64 + 1, ((v >> 14) & 0x3FFF) as u64 + 1))
            }
            _ => None,
        },
        "jpeg" => {
            // Walk the marker segments to the first start-of-frame.
            let mut i = 2;
            while i + 9 <= h.len() {
                if h[i] != 0xFF {
                    return None;
                }
                let m = h[i + 1];
                if m == 0xFF {
                    i += 1;
                    continue;
                }
                let len = be16(i + 2)? as usize;
                if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                if len < 2 {
                    return None;
                }
                i += 2 + len;
            }
            None
        }
        _ => None,
    }
}

const MAX_SANE_PIXELS: u64 = 100_000_000;
const MAX_SANE_EDGE: u64 = 30_000;

pub fn assess(x: &Input) -> Vec<Finding> {
    let mut out = name_findings(x.name);
    if let Some(target) = x.symlink_target {
        out.push(f(
            Level::Info,
            "symlink",
            tr!("Symbolic link"),
            tr!(
                "Points to “{target}”. Mori never follows links when browsing, scanning or analysing.",
                target = target
            ),
        ));
        if x.symlink_outside {
            out.push(f(
                Level::Attention,
                "symlink-outside",
                tr!("Link points outside this folder"),
                tr!("Its target is outside the folder Mori was given."),
            ));
        }
        out.sort_by_key(|f| std::cmp::Reverse(f.level));
        return out;
    }
    let Some(dt) = &x.detected else { return out };
    let claimed = filetype::claimed_family(x.ext);
    let expected = filetype::expected_ids(x.ext);
    let code_content = matches!(dt.family, Family::Executable | Family::Script);

    if let Some(exp) = expected {
        if !exp.contains(&dt.id) && dt.id != "empty" {
            if code_content && claimed.is_some() {
                out.push(f(
                    Level::High,
                    "disguised-executable",
                    tr!("{kind} disguised as .{ext}", kind = dt.label, ext = x.ext),
                    disguised_detail(claimed, x.ext, &dt.label),
                ));
            } else if dt.id == "unknown" {
                out.push(f(
                    Level::Attention,
                    "unknown-signature",
                    tr!("Unknown file signature"),
                    tr!(
                        "The content doesn't start like any .{ext} file Mori knows. It may be damaged, encrypted or something else.",
                        ext = x.ext
                    ),
                ));
            } else {
                out.push(f(
                    Level::Attention,
                    "extension-mismatch",
                    tr!("Extension mismatch: .{ext} contains {kind}", ext = x.ext, kind = dt.label),
                    tr!("The file's content does not match its extension."),
                ));
            }
        }
    } else if code_content && !filetype::EXECUTABLE_EXT.contains(&x.ext) {
        out.push(f(
            Level::Attention,
            "unexpected-executable",
            tr!("Contains {kind}", kind = dt.label),
            tr!("The name gives no hint that this file is a program or script."),
        ));
    }
    if filetype::EXECUTABLE_EXT.contains(&x.ext) || code_content {
        out.push(f(
            Level::Info,
            "runs-code",
            tr!("Program, script or installer"),
            tr!("Files like this run code when opened by the system. Mori never opens them."),
        ));
    }
    if x.executable_bit
        && !code_content
        && matches!(dt.family, Family::Image | Family::Video | Family::Audio | Family::Document)
    {
        out.push(f(
            Level::Attention,
            "executable-bit",
            tr!("Executable permission on a non-program file"),
            tr!("Media and documents don't normally carry the executable permission."),
        ));
    }
    if x.quarantined {
        out.push(f(
            Level::Info,
            "quarantine",
            tr!("Downloaded from the internet"),
            tr!("macOS marked this file with a quarantine attribute when it was downloaded."),
        ));
    }
    if dt.family == Family::Archive || dt.id == "jar" || dt.id == "xar" {
        out.push(f(
            Level::Info,
            "archive",
            tr!("Archive"),
            tr!("Contents are not extracted. Archives can hide other files."),
        ));
    }
    if dt.id == "svg" || dt.id == "html" {
        out.push(f(Level::Info, "active-content", tr!("Can contain scripts"), tr!("Mori never renders HTML or SVG.")));
    }
    // Declared image dimensions (header only).
    if let Some((w, h)) = header_dimensions(x.head, dt.id) {
        if w == 0 || h == 0 {
            out.push(f(
                Level::Attention,
                "invalid-dimensions",
                tr!("Invalid image dimensions"),
                tr!("The header declares {w} × {h} pixels.", w = w, h = h),
            ));
        } else if w * h > MAX_SANE_PIXELS || w > MAX_SANE_EDGE || h > MAX_SANE_EDGE {
            out.push(f(
                Level::Attention,
                "extreme-dimensions",
                tr!("Suspiciously extreme image dimensions"),
                tr!("The header declares {w} × {h} pixels. Mori refuses to decode images this large.", w = w, h = h),
            ));
        }
        let decoded = w.saturating_mul(h).saturating_mul(3);
        if x.size > 0 && decoded / x.size.max(1) > 1000 && decoded > 50_000_000 {
            out.push(f(
                Level::Attention,
                "compression-ratio",
                tr!("Extreme compression ratio"),
                tr!(
                    "{bytes} bytes would expand to about {mb} MB of pixels (a pattern used by decompression bombs).",
                    bytes = x.size,
                    mb = decoded / 1_000_000
                ),
            ));
        }
    }
    // Truncation checks need the end of the file.
    if !x.tail.is_empty() {
        let truncated = match dt.id {
            "jpeg" => !x.tail.windows(2).any(|w| w == [0xFF, 0xD9]),
            "png" => !x.tail.windows(4).any(|w| w == b"IEND"),
            "gif" => !x.tail.contains(&0x3B),
            "pdf" => !x.tail.windows(5).any(|w| w == b"%%EOF"),
            _ => false,
        };
        if truncated {
            out.push(f(
                Level::Attention,
                "truncated",
                tr!("File appears truncated"),
                tr!("A complete {kind} ends with a marker that is missing here.", kind = dt.label),
            ));
        }
    }
    if x.broken_container {
        out.push(f(
            Level::Attention,
            "broken-container",
            tr!("Broken media container"),
            tr!("Mori could not read the container structure."),
        ));
    }
    if x.decode_failed {
        out.push(f(
            Level::Attention,
            "decode-failed",
            tr!("Previous preview attempt failed"),
            tr!("Mori's isolated decoder could not read this exact version of the file."),
        ));
    }
    if x.media_blocked {
        out.push(f(
            Level::High,
            "media-engine-failure",
            tr!("Crashed or hung the media engine before"),
            tr!("This video stopped the system media engine in an earlier session, so Mori no longer loads it."),
        ));
    }
    if x.size == 0 {
        out.push(f(Level::Info, "empty", tr!("Empty file"), tr!("The file has no content.")));
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.level));
    out
}

/// "The extension .jpg says this is an image, but the content is …" — one
/// sentence per family, so translators get whole sentences.
fn disguised_detail(f: Option<Family>, ext: &str, kind: &str) -> String {
    match f {
        Some(Family::Image) => {
            tr!("The extension .{ext} says this is an image, but the content is {kind}.", ext = ext, kind = kind)
        }
        Some(Family::Video) => {
            tr!("The extension .{ext} says this is a video, but the content is {kind}.", ext = ext, kind = kind)
        }
        Some(Family::Audio) => {
            tr!("The extension .{ext} says this is an audio file, but the content is {kind}.", ext = ext, kind = kind)
        }
        Some(Family::Document) => {
            tr!("The extension .{ext} says this is a document, but the content is {kind}.", ext = ext, kind = kind)
        }
        _ => tr!("The extension .{ext} says this is a file, but the content is {kind}.", ext = ext, kind = kind),
    }
}

/// Short factual summary when nothing was flagged (never "safe").
pub fn summary(findings: &[Finding], detected: Option<&FileType>, ext: &str) -> String {
    if findings.iter().any(|f| f.level > Level::Info) {
        return tr!("Anomalies found — see below");
    }
    match (detected, filetype::expected_ids(ext)) {
        (Some(dt), Some(exp)) if exp.contains(&dt.id) => {
            tr!("No anomaly detected · extension matches detected format")
        }
        _ => tr!("No anomaly detected by Mori's checks"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(name: &str, head: &[u8], tail: &[u8]) -> Vec<&'static str> {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        let x = Input {
            name,
            ext: &ext,
            size: 5000,
            head,
            tail,
            detected: Some(filetype::detect(head, 5000)),
            ..Default::default()
        };
        assess(&x).into_iter().map(|f| f.code).collect()
    }

    #[test]
    fn executable_disguised_as_photo() {
        let codes = run("holiday.jpg", b"\xCF\xFA\xED\xFE\x07\0\0\x01", b"");
        assert_eq!(codes[0], "disguised-executable");
    }

    #[test]
    fn double_extension_and_bidi() {
        assert!(run("invoice.pdf.app", b"%PDF-1.4", b"").contains(&"double-extension"));
        assert!(run("photo.jpg.exe", b"MZ\x90\0", b"").contains(&"double-extension"));
        assert!(run("cv\u{202E}fdp.exe", b"MZ\x90\0", b"").contains(&"bidi-control"));
        assert!(!run("my.holiday.photo.jpg", b"\xFF\xD8\xFF\xE0", b"\xFF\xD9").contains(&"double-extension"));
    }

    #[test]
    fn matching_files_have_no_anomaly() {
        let codes = run("photo.jpg", b"\xFF\xD8\xFF\xE0\0\x10JFIF", b"\0\0\xFF\xD9");
        assert!(codes.is_empty(), "{codes:?}");
        let x = Input { ext: "jpg", detected: Some(filetype::detect(b"\xFF\xD8\xFF", 3)), ..Default::default() };
        assert!(summary(&assess(&x), x.detected.as_ref(), "jpg").contains("extension matches"));
    }

    #[test]
    fn mismatch_unknown_truncated() {
        assert!(run("song.mp3", b"\x89PNG\r\n\x1a\n", b"IEND").contains(&"extension-mismatch"));
        assert!(run("movie.mp4", b"\x13\x37\xBE\xEF\x00", b"").contains(&"unknown-signature"));
        assert!(run("photo.jpg", b"\xFF\xD8\xFF\xE0\0\x10JFIF", b"\x12\x34\x56").contains(&"truncated"));
        assert!(run("image.png", b"\x89PNG\r\n\x1a\n", b"\0\0\0\0").contains(&"truncated"));
    }

    #[test]
    fn extreme_dimensions_from_header_only() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(60_000u32.to_be_bytes());
        png.extend(60_000u32.to_be_bytes());
        png.extend([8, 2, 0, 0, 0]);
        let codes = run("bomb.png", &png, b"IEND");
        assert!(codes.contains(&"extreme-dimensions") && codes.contains(&"compression-ratio"), "{codes:?}");
        // JPEG: the size is in the SOF segment after other segments.
        let jpeg =
            [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80];
        assert_eq!(header_dimensions(&jpeg, "jpeg"), Some((640, 480)));
    }

    #[test]
    fn scripts_without_hint_and_symlinks() {
        assert!(run("notes", b"#!/bin/bash\nrm -rf", b"").contains(&"unexpected-executable"));
        let x = Input { name: "link", symlink_target: Some("/etc"), symlink_outside: true, ..Default::default() };
        let codes: Vec<_> = assess(&x).iter().map(|f| f.code).collect();
        assert_eq!(codes, ["symlink-outside", "symlink"]);
    }
}
