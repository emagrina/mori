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
            "Name contains a text-direction control character",
            "Characters like U+202E can make “photo\u{2026}exe.jpg” display as an image name. Mori shows them as visible symbols.",
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
                format!("Double extension: “.{prev}.{last}”"),
                format!("The name looks like a .{prev} file but the real extension is .{last}, which runs code or installs software when opened."),
            ));
        }
    }
    if name.contains("    ") || name.trim_end() != name {
        out.push(f(
            Level::Attention,
            "padded-name",
            "Name contains a long run of spaces",
            "Padding like “invoice.pdf          .exe” can push the real extension out of view.",
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
            "Symbolic link",
            format!("Points to “{target}”. Mori never follows links when browsing, scanning or analysing."),
        ));
        if x.symlink_outside {
            out.push(f(
                Level::Attention,
                "symlink-outside",
                "Link points outside this folder",
                "Its target is outside the folder Mori was given.",
            ));
        }
        out.sort_by_key(|f| std::cmp::Reverse(f.level));
        return out;
    }
    let Some(dt) = x.detected else { return out };
    let claimed = filetype::claimed_family(x.ext);
    let expected = filetype::expected_ids(x.ext);
    let code_content = matches!(dt.family, Family::Executable | Family::Script);

    if let Some(exp) = expected {
        if !exp.contains(&dt.id) && dt.id != "empty" {
            if code_content && claimed.is_some() {
                out.push(f(
                    Level::High,
                    "disguised-executable",
                    format!("{} disguised as .{}", dt.label, x.ext),
                    format!(
                        "The extension .{} says this is {}, but the content is {}.",
                        x.ext,
                        family_word(claimed),
                        dt.label
                    ),
                ));
            } else if dt.id == "unknown" {
                out.push(f(
                    Level::Attention,
                    "unknown-signature",
                    "Unknown file signature",
                    format!("The content doesn't start like any .{} file Mori knows. It may be damaged, encrypted or something else.", x.ext),
                ));
            } else {
                out.push(f(
                    Level::Attention,
                    "extension-mismatch",
                    format!("Extension mismatch: .{} contains {}", x.ext, dt.label),
                    "The file's content does not match its extension.".to_string(),
                ));
            }
        }
    } else if code_content && !filetype::EXECUTABLE_EXT.contains(&x.ext) {
        out.push(f(
            Level::Attention,
            "unexpected-executable",
            format!("Contains {}", dt.label),
            "The name gives no hint that this file is a program or script.".to_string(),
        ));
    }
    if filetype::EXECUTABLE_EXT.contains(&x.ext) || code_content {
        out.push(f(
            Level::Info,
            "runs-code",
            "Program, script or installer",
            "Files like this run code when opened by the system. Mori never opens them.",
        ));
    }
    if x.executable_bit
        && !code_content
        && matches!(dt.family, Family::Image | Family::Video | Family::Audio | Family::Document)
    {
        out.push(f(
            Level::Attention,
            "executable-bit",
            "Executable permission on a non-program file",
            "Media and documents don't normally carry the executable permission.",
        ));
    }
    if x.quarantined {
        out.push(f(
            Level::Info,
            "quarantine",
            "Downloaded from the internet",
            "macOS marked this file with a quarantine attribute when it was downloaded.",
        ));
    }
    if dt.family == Family::Archive || dt.id == "jar" || dt.id == "xar" {
        out.push(f(Level::Info, "archive", "Archive", "Contents are not extracted. Archives can hide other files."));
    }
    if dt.id == "svg" || dt.id == "html" {
        out.push(f(Level::Info, "active-content", "Can contain scripts", "Mori never renders HTML or SVG."));
    }
    // Declared image dimensions (header only).
    if let Some((w, h)) = header_dimensions(x.head, dt.id) {
        if w == 0 || h == 0 {
            out.push(f(
                Level::Attention,
                "invalid-dimensions",
                "Invalid image dimensions",
                format!("The header declares {w} × {h} pixels."),
            ));
        } else if w * h > MAX_SANE_PIXELS || w > MAX_SANE_EDGE || h > MAX_SANE_EDGE {
            out.push(f(
                Level::Attention,
                "extreme-dimensions",
                "Suspiciously extreme image dimensions",
                format!("The header declares {w} × {h} pixels. Mori refuses to decode images this large."),
            ));
        }
        let decoded = w.saturating_mul(h).saturating_mul(3);
        if x.size > 0 && decoded / x.size.max(1) > 1000 && decoded > 50_000_000 {
            out.push(f(
                Level::Attention,
                "compression-ratio",
                "Extreme compression ratio",
                format!(
                    "{} bytes would expand to about {} MB of pixels (a pattern used by decompression bombs).",
                    x.size,
                    decoded / 1_000_000
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
                "File appears truncated",
                format!("A complete {} ends with a marker that is missing here.", dt.label),
            ));
        }
    }
    if x.broken_container {
        out.push(f(
            Level::Attention,
            "broken-container",
            "Broken media container",
            "Mori could not read the container structure.",
        ));
    }
    if x.decode_failed {
        out.push(f(
            Level::Attention,
            "decode-failed",
            "Previous preview attempt failed",
            "Mori's isolated decoder could not read this exact version of the file.",
        ));
    }
    if x.media_blocked {
        out.push(f(
            Level::High,
            "media-engine-failure",
            "Crashed or hung the media engine before",
            "This video stopped the system media engine in an earlier session, so Mori no longer loads it.",
        ));
    }
    if x.size == 0 {
        out.push(f(Level::Info, "empty", "Empty file", "The file has no content."));
    }
    out.sort_by_key(|f| std::cmp::Reverse(f.level));
    out
}

fn family_word(f: Option<Family>) -> &'static str {
    match f {
        Some(Family::Image) => "an image",
        Some(Family::Video) => "a video",
        Some(Family::Audio) => "an audio file",
        Some(Family::Document) => "a document",
        _ => "a file",
    }
}

/// Short factual summary when nothing was flagged (never "safe").
pub fn summary(findings: &[Finding], detected: Option<FileType>, ext: &str) -> String {
    if findings.iter().any(|f| f.level > Level::Info) {
        return "Anomalies found — see below".into();
    }
    match (detected, filetype::expected_ids(ext)) {
        (Some(dt), Some(exp)) if exp.contains(&dt.id) => {
            "No anomaly detected · extension matches detected format".into()
        }
        _ => "No anomaly detected by Mori's checks".into(),
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
        assert!(summary(&assess(&x), x.detected, "jpg").contains("extension matches"));
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
