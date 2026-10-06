//! Path confinement, safe file opening and content sniffing.
//!
//! Everything here treats the drive as hostile: paths are re-validated and
//! canonicalised on every access, files are opened without following
//! symlinks, and file types are decided from magic bytes, not extensions.

use serde::Serialize;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    Invalid,
    NotFound,
    Outside,
    NotRegular,
    TooLarge,
}

/// Open `rel` (a path taken from the index) strictly inside `root`.
/// `root` must already be canonical.
pub fn open_inside(root: &Path, rel: &str) -> Result<(File, Metadata, PathBuf), OpenError> {
    let rel_path = Path::new(rel);
    if rel.is_empty() || rel_path.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(OpenError::Invalid);
    }
    // Resolves `..`, symlinks and junctions anywhere along the path.
    let canon = fs::canonicalize(root.join(rel_path)).map_err(|_| OpenError::NotFound)?;
    if !canon.starts_with(root) {
        return Err(OpenError::Outside);
    }
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a symlink swapped in after canonicalisation, and never
        // block on FIFOs / device nodes.
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = opts.open(&canon).map_err(|_| OpenError::NotFound)?;
    let meta = file.metadata().map_err(|_| OpenError::NotFound)?;
    if !meta.is_file() {
        return Err(OpenError::NotRegular);
    }
    Ok((file, meta, canon))
}

/// Read a whole file, refusing anything over `limit` bytes.
pub fn read_limited(mut file: File, meta: &Metadata, limit: u64) -> Result<Vec<u8>, OpenError> {
    if meta.len() > limit {
        return Err(OpenError::TooLarge);
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.by_ref().take(limit + 1).read_to_end(&mut buf).map_err(|_| OpenError::NotFound)?;
    if buf.len() as u64 > limit {
        return Err(OpenError::TooLarge);
    }
    Ok(buf)
}

pub fn read_head(file: &mut File, n: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(n);
    let _ = file.by_ref().take(n as u64).read_to_end(&mut buf);
    buf
}

// ----------------------------------------------------------------- sniffing

/// What a file *actually* is, judged by its leading bytes.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Detected {
    Jpeg,
    Png,
    Webp,
    Gif,
    /// HEIC / HEIF still (decoded only where the platform decoder is available).
    Heif,
    Mp4,
    Mov,
    Webm,
    Pdf,
    Text,
    Executable,
    Unknown,
}

impl Detected {
    pub fn is_image(self) -> bool {
        matches!(self, Detected::Jpeg | Detected::Png | Detected::Webp | Detected::Gif)
            || (self == Detected::Heif && cfg!(target_os = "macos"))
    }
    pub fn is_video(self) -> bool {
        matches!(self, Detected::Mp4 | Detected::Mov | Detected::Webm)
    }
    pub fn video_mime(self) -> Option<&'static str> {
        Some(match self {
            Detected::Mp4 => "video/mp4",
            Detected::Mov => "video/quicktime",
            Detected::Webm => "video/webm",
            _ => return None,
        })
    }
}

/// Extensions whose content may be shown as plain text.
pub const TEXT_EXT: &[&str] =
    &["txt", "md", "markdown", "csv", "tsv", "log", "json", "srt", "vtt", "ini", "yaml", "yml"];

/// How many leading bytes `sniff` wants.
pub const SNIFF_LEN: usize = 4096;

const MP4_BRANDS: &[&[u8; 4]] = &[
    b"isom", b"iso2", b"iso4", b"iso5", b"iso6", b"mp41", b"mp42", b"avc1", b"M4V ", b"M4VH", b"M4VP", b"dash",
    b"mmp4", b"MSNV",
];

pub fn sniff(head: &[u8], ext: &str) -> Detected {
    let starts = |sig: &[u8]| head.starts_with(sig);
    if starts(&[0xFF, 0xD8, 0xFF]) {
        return Detected::Jpeg;
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return Detected::Png;
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return Detected::Gif;
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Detected::Webp;
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if brand == b"qt  " {
            return Detected::Mov;
        }
        if matches!(brand, b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1") {
            return Detected::Heif;
        }
        // AVIF and other ISO-BMFF flavours are deliberately unsupported.
        return if MP4_BRANDS.iter().any(|b| &b[..] == brand) { Detected::Mp4 } else { Detected::Unknown };
    }
    if head.len() >= 8 && ext == "mov" && matches!(&head[4..8], b"moov" | b"mdat" | b"wide" | b"free" | b"skip") {
        return Detected::Mov; // pre-ftyp QuickTime
    }
    if starts(&[0x1A, 0x45, 0xDF, 0xA3]) {
        // Matroska container: only the WebM profile is accepted.
        let window = &head[..head.len().min(64)];
        return if window.windows(4).any(|w| w == b"webm") { Detected::Webm } else { Detected::Unknown };
    }
    if starts(b"%PDF-") {
        return Detected::Pdf;
    }
    if starts(b"MZ")
        || starts(&[0x7F, b'E', b'L', b'F'])
        || starts(&[0xFE, 0xED, 0xFA, 0xCE])
        || starts(&[0xFE, 0xED, 0xFA, 0xCF])
        || starts(&[0xCE, 0xFA, 0xED, 0xFE])
        || starts(&[0xCF, 0xFA, 0xED, 0xFE])
        || starts(&[0xCA, 0xFE, 0xBA, 0xBE])
        || starts(b"#!")
    {
        return Detected::Executable;
    }
    if TEXT_EXT.contains(&ext) && looks_like_text(head) {
        return Detected::Text;
    }
    Detected::Unknown
}

pub fn looks_like_text(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let valid = match std::str::from_utf8(bytes) {
        Ok(_) => true,
        // A multi-byte character cut off at the end of the sample is fine.
        Err(e) => e.error_len().is_none(),
    };
    valid && !bytes.iter().any(|&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C))
}

/// What the extension claims, used only to flag disguised files.
pub fn expected_for_ext(ext: &str) -> Option<&'static [Detected]> {
    use Detected::*;
    Some(match ext {
        "jpg" | "jpeg" | "jpe" | "jfif" => &[Jpeg],
        "png" => &[Png],
        "webp" => &[Webp],
        "gif" => &[Gif],
        "heic" | "heif" => &[Heif],
        "mp4" | "m4v" | "mov" => &[Mp4, Mov],
        "webm" => &[Webm],
        "pdf" => &[Pdf],
        e if TEXT_EXT.contains(&e) => &[Text],
        _ => return None,
    })
}

/// Passive formats the OS may open in their default viewer. Anything that can
/// run code or reach the network when opened (apps, scripts, installers,
/// HTML/SVG, shortcuts, macro-enabled Office files…) is excluded.
const OPEN_ALLOW: &[&str] = &[
    "jpg", "jpeg", "png", "webp", "gif", "heic", "heif", "avif", "bmp", "tif", "tiff", "dng", "cr2", "cr3", "nef",
    "arw", "orf", "rw2", "raf", "mp4", "mov", "m4v", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "3gp", "mts", "m2ts",
    "mp3", "wav", "flac", "aac", "m4a", "ogg", "opus", "aiff", "aif", "pdf", "txt", "md", "csv", "tsv", "rtf", "doc",
    "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pages", "numbers", "key", "epub", "srt", "vtt",
];

pub fn may_open_externally(ext: &str, detected: Detected) -> bool {
    detected != Detected::Executable && OPEN_ALLOW.contains(&ext)
}

/// Replace control and bidi-override characters so a filename can't spoof
/// its extension (e.g. "photo\u{202E}gpj.exe") or break the layout.
pub fn display_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            let bidi =
                matches!(c, '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
            if c.is_control() || bidi {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Strip Windows' `\\?\` verbatim prefix for display and for Explorer.
pub fn plain_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        Some(rest) => format!(r"\\{}", &rest[4..]),
        None => s.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_by_content_not_extension() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n....", "jpg"), Detected::Png);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0], "png"), Detected::Jpeg);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 ", "jpg"), Detected::Webp);
        assert_eq!(sniff(b"\0\0\0\x18ftypmp42\0\0\0\0", "mov"), Detected::Mp4);
        assert_eq!(sniff(b"\0\0\0\x14ftypqt  \0\0\0\0", "mov"), Detected::Mov);
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0", "heic"), Detected::Heif);
        assert_eq!(sniff(b"\0\0\0\x18ftypavif\0\0\0\0", "avif"), Detected::Unknown);
        assert_eq!(
            sniff(b"\x1a\x45\xdf\xa3\x01\0\0\0\0\0\0\x1f\x42\x86\x81\x01\x42\x82\x84webm", "webm"),
            Detected::Webm
        );
        assert_eq!(sniff(b"\x1a\x45\xdf\xa3\x01\0\0\0\0\0\0\x1f\x42\x82\x88matroska", "webm"), Detected::Unknown);
        assert_eq!(sniff(b"MZ\x90\0", "jpg"), Detected::Executable);
        assert_eq!(sniff(b"#!/bin/sh\nrm -rf ~", "txt"), Detected::Executable);
        assert_eq!(sniff(b"\xCF\xFA\xED\xFE", "mp4"), Detected::Executable);
        assert_eq!(sniff(b"hello world\n", "txt"), Detected::Text);
        assert_eq!(sniff(b"hello\0world", "txt"), Detected::Unknown);
        assert_eq!(sniff(b"<svg onload=alert(1)>", "svg"), Detected::Unknown);
        assert_eq!(sniff(b"%PDF-1.7", "pdf"), Detected::Pdf);
    }

    #[test]
    fn never_opens_active_content() {
        assert!(!may_open_externally("jpg", Detected::Executable));
        for ext in ["app", "command", "sh", "exe", "bat", "html", "svg", "webloc", "url", "docm", "scpt"] {
            assert!(!may_open_externally(ext, Detected::Unknown), "{ext}");
        }
        assert!(may_open_externally("pdf", Detected::Pdf));
    }

    #[test]
    fn sanitises_spoofed_names() {
        assert_eq!(display_safe("photo\u{202E}gpj.exe"), "photo\u{FFFD}gpj.exe");
        assert_eq!(display_safe("a\nb\u{7}"), "a\u{FFFD}b\u{FFFD}");
        assert_eq!(display_safe("cumpleaños.jpg"), "cumpleaños.jpg");
    }

    #[cfg(unix)]
    #[test]
    fn confines_paths_and_symlinks() {
        let base = std::env::temp_dir().join(format!("mori-secure-{}", std::process::id()));
        let root = base.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(base.join("secret.txt"), b"secret").unwrap();
        fs::write(root.join("sub/ok.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink(base.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&base, root.join("escape")).unwrap();
        let root = fs::canonicalize(&root).unwrap();

        assert!(open_inside(&root, "sub/ok.txt").is_ok());
        assert_eq!(open_inside(&root, "../secret.txt").unwrap_err(), OpenError::Invalid);
        assert_eq!(open_inside(&root, "/etc/hosts").unwrap_err(), OpenError::Invalid);
        assert_eq!(open_inside(&root, "link.txt").unwrap_err(), OpenError::Outside);
        assert_eq!(open_inside(&root, "escape/secret.txt").unwrap_err(), OpenError::Outside);
        assert_eq!(open_inside(&root, "sub").unwrap_err(), OpenError::NotRegular);
        fs::remove_dir_all(&base).unwrap();
    }
}
