//! Real file type detection from content (magic bytes / container
//! signatures), never from the extension. Nothing is executed or decoded:
//! only the first bytes (and, where useful, the last bytes) are looked at.
//!
//! `secure::sniff` remains the narrow, security-tuned decision of *what Mori
//! may preview*; this module is the broader, descriptive answer to "what is
//! this file really?", used by the inspector and the risk indicators.

use serde::Serialize;

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Executable,
    Script,
    Web,
    Text,
    Font,
    Data,
    Unknown,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileType {
    /// Stable short id ("jpeg", "macho", "zip"…).
    pub id: &'static str,
    pub label: &'static str,
    pub family: Family,
    pub mime: &'static str,
}

const fn t(id: &'static str, label: &'static str, family: Family, mime: &'static str) -> FileType {
    FileType { id, label, family, mime }
}

use Family::*;

pub const UNKNOWN: FileType = t("unknown", "Unknown format", Unknown, "application/octet-stream");
pub const EMPTY: FileType = t("empty", "Empty file", Data, "application/x-empty");
pub const TEXT: FileType = t("text", "Plain text", Text, "text/plain");

/// How many leading bytes `detect` uses.
pub const HEAD_LEN: usize = 4096;

fn ftyp_brand(h: &[u8]) -> Option<&[u8]> {
    (h.len() >= 12 && &h[4..8] == b"ftyp").then(|| &h[8..12])
}

fn find(h: &[u8], needle: &[u8]) -> bool {
    h.windows(needle.len()).any(|w| w == needle)
}

fn starts_ci(h: &[u8], s: &[u8]) -> bool {
    h.len() >= s.len() && h[..s.len()].eq_ignore_ascii_case(s)
}

/// Skip a UTF-8 BOM and leading whitespace (for text-based formats).
fn trim_text(h: &[u8]) -> &[u8] {
    let h = h.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(h);
    let n = h.iter().take_while(|b| b.is_ascii_whitespace()).count();
    &h[n..]
}

fn looks_like_text(h: &[u8]) -> bool {
    !h.is_empty() && crate::secure::looks_like_text(&h[..h.len().min(HEAD_LEN)])
}

/// Detect the real type from the first bytes of a file (`size` is its full
/// length; 0 = empty).
pub fn detect(h: &[u8], size: u64) -> FileType {
    if size == 0 || h.is_empty() {
        return EMPTY;
    }
    let s = |sig: &[u8]| h.starts_with(sig);
    // ---- images
    if s(&[0xFF, 0xD8, 0xFF]) {
        return t("jpeg", "JPEG image", Image, "image/jpeg");
    }
    if s(b"\x89PNG\r\n\x1a\n") {
        return t("png", "PNG image", Image, "image/png");
    }
    if s(b"GIF87a") || s(b"GIF89a") {
        return t("gif", "GIF image", Image, "image/gif");
    }
    if h.len() >= 12 && &h[0..4] == b"RIFF" {
        match &h[8..12] {
            b"WEBP" => return t("webp", "WebP image", Image, "image/webp"),
            b"WAVE" => return t("wav", "WAV audio", Audio, "audio/wav"),
            b"AVI " => return t("avi", "AVI video", Video, "video/x-msvideo"),
            _ => {}
        }
    }
    if s(b"BM") && h.len() >= 26 && h[6..10] == [0, 0, 0, 0] {
        return t("bmp", "BMP image", Image, "image/bmp");
    }
    if s(b"II*\0") || s(b"MM\0*") {
        if h.len() >= 10 && &h[8..10] == b"CR" {
            return t("cr2", "Canon RAW (CR2)", Image, "image/x-canon-cr2");
        }
        return t("tiff", "TIFF image (or TIFF-based RAW)", Image, "image/tiff");
    }
    if s(b"IIRO") || s(b"IIRS") {
        return t("orf", "Olympus RAW", Image, "image/x-olympus-orf");
    }
    if s(b"IIU\0") {
        return t("rw2", "Panasonic RAW", Image, "image/x-panasonic-rw2");
    }
    if s(b"FUJIFILMCCD-RAW") {
        return t("raf", "Fujifilm RAW", Image, "image/x-fuji-raf");
    }
    if s(&[0, 0, 1, 0]) && h.len() > 6 && h[4] > 0 {
        return t("ico", "Windows icon", Image, "image/x-icon");
    }
    if s(b"8BPS") {
        return t("psd", "Photoshop document", Image, "image/vnd.adobe.photoshop");
    }
    if let Some(brand) = ftyp_brand(h) {
        return match brand {
            b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"hevc" | b"hevx" => {
                t("heif", "HEIF / HEIC image", Image, "image/heic")
            }
            b"avif" | b"avis" => t("avif", "AVIF image", Image, "image/avif"),
            b"crx " => t("cr3", "Canon RAW (CR3)", Image, "image/x-canon-cr3"),
            b"qt  " => t("mov", "QuickTime movie", Video, "video/quicktime"),
            b"M4A " | b"M4B " | b"M4P " => t("m4a", "MPEG-4 audio", Audio, "audio/mp4"),
            b"3gp4" | b"3gp5" | b"3gp6" | b"3g2a" | b"3ge6" | b"3gs7" => t("3gp", "3GPP video", Video, "video/3gpp"),
            b"M4V " | b"M4VH" | b"M4VP" => t("m4v", "MPEG-4 video", Video, "video/x-m4v"),
            _ => t("mp4", "MPEG-4 video", Video, "video/mp4"),
        };
    }
    if h.len() >= 8 && matches!(&h[4..8], b"moov" | b"mdat" | b"wide" | b"free" | b"skip" | b"pnot") {
        return t("mov", "QuickTime movie", Video, "video/quicktime");
    }
    if s(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let window = &h[..h.len().min(64)];
        return if find(window, b"webm") {
            t("webm", "WebM video", Video, "video/webm")
        } else {
            t("mkv", "Matroska video", Video, "video/x-matroska")
        };
    }
    if s(b"FLV\x01") {
        return t("flv", "Flash video", Video, "video/x-flv");
    }
    if s(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        return t("asf", "Windows Media (ASF)", Video, "video/x-ms-asf");
    }
    if s(&[0, 0, 1, 0xBA]) {
        return t("mpeg", "MPEG program stream", Video, "video/mpeg");
    }
    if h.len() > 376 && h[0] == 0x47 && h[188] == 0x47 && h[376] == 0x47 {
        return t("mpegts", "MPEG transport stream", Video, "video/mp2t");
    }
    // ---- audio
    if s(b"fLaC") {
        return t("flac", "FLAC audio", Audio, "audio/flac");
    }
    if s(b"ID3") {
        return t("mp3", "MP3 audio", Audio, "audio/mpeg");
    }
    if s(b"OggS") {
        let w = &h[..h.len().min(128)];
        return if find(w, b"\x80theora") {
            t("ogv", "Ogg video", Video, "video/ogg")
        } else if find(w, b"OpusHead") {
            t("opus", "Opus audio", Audio, "audio/opus")
        } else {
            t("ogg", "Ogg audio", Audio, "audio/ogg")
        };
    }
    if h.len() >= 12 && &h[0..4] == b"FORM" && matches!(&h[8..12], b"AIFF" | b"AIFC") {
        return t("aiff", "AIFF audio", Audio, "audio/aiff");
    }
    if s(b"MThd") {
        return t("midi", "MIDI", Audio, "audio/midi");
    }
    if s(b"#!AMR") {
        return t("amr", "AMR audio", Audio, "audio/amr");
    }
    if h.len() >= 2 && h[0] == 0xFF && (h[1] & 0xF6) == 0xF0 {
        return t("aac", "AAC audio (ADTS)", Audio, "audio/aac");
    }
    if h.len() >= 2 && h[0] == 0xFF && (h[1] & 0xE0) == 0xE0 && (h[1] & 0x06) != 0 {
        return t("mp3", "MP3 audio", Audio, "audio/mpeg");
    }
    // ---- executables and code
    if s(&[0xFE, 0xED, 0xFA, 0xCE])
        || s(&[0xFE, 0xED, 0xFA, 0xCF])
        || s(&[0xCE, 0xFA, 0xED, 0xFE])
        || s(&[0xCF, 0xFA, 0xED, 0xFE])
    {
        return t("macho", "Mach-O executable (macOS)", Executable, "application/x-mach-binary");
    }
    if s(&[0xCA, 0xFE, 0xBA, 0xBE]) && h.len() >= 8 {
        // Universal Mach-O and Java class files share this magic; a fat
        // binary has a small architecture count where Java has its version.
        let n = u32::from_be_bytes([h[4], h[5], h[6], h[7]]);
        return if n > 0 && n < 20 {
            t("macho", "Universal Mach-O executable (macOS)", Executable, "application/x-mach-binary")
        } else {
            t("javaclass", "Java class file", Executable, "application/java-vm")
        };
    }
    if s(&[0x7F, b'E', b'L', b'F']) {
        return t("elf", "ELF executable (Linux)", Executable, "application/x-elf");
    }
    if s(b"MZ") {
        return t("pe", "Windows executable (PE)", Executable, "application/vnd.microsoft.portable-executable");
    }
    if s(b"dex\n") {
        return t("dex", "Android bytecode", Executable, "application/x-dex");
    }
    if s(b"\0asm") {
        return t("wasm", "WebAssembly module", Executable, "application/wasm");
    }
    if s(&[0x4C, 0, 0, 0, 0x01, 0x14, 0x02, 0]) {
        return t("lnk", "Windows shortcut", Executable, "application/x-ms-shortcut");
    }
    if s(b"FasdUAS") {
        return t("scpt", "Compiled AppleScript", Script, "application/x-applescript");
    }
    if s(b"#!") {
        return t("shebang", "Script (#! interpreter line)", Script, "text/x-script");
    }
    // ---- documents and containers
    if s(b"%PDF-") {
        return t("pdf", "PDF document", Document, "application/pdf");
    }
    if s(b"{\\rtf") {
        return t("rtf", "Rich Text document", Document, "application/rtf");
    }
    if s(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return t("ole", "Microsoft Office (legacy) / OLE container", Document, "application/x-ole-storage");
    }
    if s(b"PK\x03\x04") || s(b"PK\x05\x06") || s(b"PK\x07\x08") {
        // The first local header's name hints at zip-based formats.
        let name = if h.len() >= 30 {
            let n = u16::from_le_bytes([h[26], h[27]]) as usize;
            h.get(30..30 + n).unwrap_or(&[])
        } else {
            &[]
        };
        if name == b"mimetype" {
            let w = &h[..h.len().min(200)];
            if find(w, b"application/epub+zip") {
                return t("epub", "EPUB book", Document, "application/epub+zip");
            }
            return t("odf", "OpenDocument file", Document, "application/vnd.oasis.opendocument");
        }
        if name == b"[Content_Types].xml" || find(h, b"[Content_Types].xml") {
            return t("ooxml", "Microsoft Office document (zip-based)", Document, "application/vnd.openxmlformats");
        }
        if name.starts_with(b"META-INF/") || find(&h[..h.len().min(512)], b"AndroidManifest.xml") {
            return t("jar", "Java / Android package (zip-based)", Executable, "application/java-archive");
        }
        return t("zip", "ZIP archive", Archive, "application/zip");
    }
    if s(&[0x1F, 0x8B]) {
        return t("gzip", "Gzip compressed data", Archive, "application/gzip");
    }
    if s(b"BZh") {
        return t("bzip2", "Bzip2 compressed data", Archive, "application/x-bzip2");
    }
    if s(&[0xFD, b'7', b'z', b'X', b'Z', 0]) {
        return t("xz", "XZ compressed data", Archive, "application/x-xz");
    }
    if s(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return t("zstd", "Zstandard compressed data", Archive, "application/zstd");
    }
    if s(&[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]) {
        return t("7z", "7-Zip archive", Archive, "application/x-7z-compressed");
    }
    if s(b"Rar!\x1A\x07") {
        return t("rar", "RAR archive", Archive, "application/vnd.rar");
    }
    if s(b"xar!") {
        return t("xar", "XAR archive / macOS installer package", Executable, "application/x-xar");
    }
    if s(b"MSCF") {
        return t("cab", "Windows Cabinet archive", Archive, "application/vnd.ms-cab-compressed");
    }
    if h.len() > 262 && &h[257..262] == b"ustar" {
        return t("tar", "TAR archive", Archive, "application/x-tar");
    }
    if s(b"070707") || s(b"070701") || s(b"070702") {
        return t("cpio", "cpio archive", Archive, "application/x-cpio");
    }
    if s(b"SQLite format 3\0") {
        return t("sqlite", "SQLite database", Data, "application/vnd.sqlite3");
    }
    if s(b"bplist00") {
        return t("bplist", "Binary property list", Data, "application/x-bplist");
    }
    if s(&[0, 1, 0, 0, 0]) || s(b"OTTO") || s(b"true") || s(b"ttcf") {
        return t("font", "Font file", Font, "font/ttf");
    }
    if s(b"wOFF") || s(b"wOF2") {
        return t("woff", "Web font", Font, "font/woff");
    }
    // ---- text-based formats
    let tx = trim_text(h);
    if starts_ci(tx, b"<!doctype html")
        || starts_ci(tx, b"<html")
        || starts_ci(tx, b"<head")
        || starts_ci(tx, b"<script")
    {
        return t("html", "HTML document", Web, "text/html");
    }
    if starts_ci(tx, b"<svg") || (starts_ci(tx, b"<?xml") && find(&tx[..tx.len().min(1024)], b"<svg")) {
        return t("svg", "SVG image (XML, may contain scripts)", Web, "image/svg+xml");
    }
    if starts_ci(tx, b"<?xml") {
        if find(&tx[..tx.len().min(512)], b"<plist") || find(&tx[..tx.len().min(512)], b"PropertyList") {
            return t("plist", "Property list (XML)", Data, "application/x-plist");
        }
        return t("xml", "XML document", Text, "application/xml");
    }
    if looks_like_text(h) {
        return TEXT;
    }
    UNKNOWN
}

/// Type ids an extension is allowed to have. `None` = Mori has no
/// expectation for this extension (no mismatch can be claimed).
pub fn expected_ids(ext: &str) -> Option<&'static [&'static str]> {
    Some(match ext {
        "jpg" | "jpeg" | "jpe" | "jfif" => &["jpeg"],
        "png" => &["png"],
        "gif" => &["gif"],
        "webp" => &["webp"],
        "bmp" => &["bmp"],
        "tif" | "tiff" => &["tiff"],
        "dng" | "nef" | "arw" | "pef" | "srw" | "nrw" => &["tiff"],
        "cr2" => &["cr2", "tiff"],
        "cr3" => &["cr3"],
        "orf" => &["orf"],
        "rw2" => &["rw2"],
        "raf" => &["raf"],
        "ico" => &["ico"],
        "psd" => &["psd"],
        "heic" | "heif" | "hif" => &["heif"],
        "avif" => &["avif"],
        "svg" => &["svg", "xml"],
        "mp4" => &["mp4", "m4v", "mov", "3gp"],
        "m4v" => &["m4v", "mp4", "mov"],
        "mov" | "qt" => &["mov", "mp4"],
        "3gp" | "3g2" => &["3gp", "mp4"],
        "webm" => &["webm", "mkv"],
        "mkv" => &["mkv", "webm"],
        "avi" => &["avi"],
        "flv" => &["flv"],
        "wmv" | "asf" | "wma" => &["asf"],
        "mpg" | "mpeg" => &["mpeg", "mpegts"],
        "ts" | "mts" | "m2ts" => &["mpegts", "text"],
        "ogv" => &["ogv"],
        "mp3" => &["mp3"],
        "m4a" | "m4b" | "aac" => &["m4a", "mp4", "aac"],
        "wav" => &["wav"],
        "flac" => &["flac"],
        "ogg" | "oga" => &["ogg", "opus"],
        "opus" => &["opus"],
        "aif" | "aiff" | "aifc" => &["aiff"],
        "mid" | "midi" => &["midi"],
        "amr" => &["amr"],
        "pdf" => &["pdf"],
        "rtf" => &["rtf"],
        "doc" | "xls" | "ppt" | "msg" => &["ole"],
        "docx" | "xlsx" | "pptx" => &["ooxml", "zip"],
        "odt" | "ods" | "odp" => &["odf", "zip"],
        "epub" => &["epub", "zip"],
        "zip" => &["zip"],
        "gz" | "tgz" => &["gzip"],
        "bz2" => &["bzip2"],
        "xz" => &["xz"],
        "zst" => &["zstd"],
        "7z" => &["7z"],
        "rar" => &["rar"],
        "tar" => &["tar"],
        "pkg" | "xar" => &["xar"],
        "jar" | "apk" => &["jar", "zip"],
        "exe" | "dll" | "scr" | "sys" | "com" => &["pe"],
        "dylib" | "bundle" | "so" | "o" => &["macho", "elf"],
        "class" => &["javaclass"],
        "wasm" => &["wasm"],
        "lnk" => &["lnk"],
        "scpt" => &["scpt"],
        "sqlite" | "db" | "sqlite3" => &["sqlite"],
        "plist" => &["bplist", "plist"],
        "ttf" | "otf" | "ttc" => &["font"],
        "woff" | "woff2" => &["woff"],
        "html" | "htm" | "xhtml" => &["html", "xml", "text"],
        "xml" => &["xml", "plist", "svg", "text"],
        "txt" | "md" | "markdown" | "csv" | "tsv" | "log" | "json" | "yaml" | "yml" | "ini" | "srt" | "vtt" | "css"
        | "toml" => &["text", "empty"],
        "sh" | "bash" | "zsh" | "command" | "py" | "rb" | "pl" | "js" | "mjs" | "ts.js" | "ps1" | "bat" | "cmd"
        | "vbs" | "applescript" | "php" => &["shebang", "text"],
        _ => return None,
    })
}

/// The family an extension *claims* (for "disguised" checks).
pub fn claimed_family(ext: &str) -> Option<Family> {
    use crate::index::Kind;
    match crate::index::kind_for_ext(ext) {
        Kind::Photo | Kind::Gif => Some(Image),
        Kind::Video => Some(Video),
        Kind::Audio => Some(Audio),
        Kind::Document => Some(Document),
        _ => None,
    }
}

/// Extensions that run code or install software when opened.
pub const EXECUTABLE_EXT: &[&str] = &[
    "app",
    "exe",
    "scr",
    "bat",
    "cmd",
    "com",
    "pif",
    "msi",
    "msp",
    "js",
    "jse",
    "vbs",
    "vbe",
    "wsf",
    "wsh",
    "ps1",
    "psm1",
    "hta",
    "cpl",
    "jar",
    "command",
    "sh",
    "bash",
    "zsh",
    "csh",
    "tool",
    "scpt",
    "applescript",
    "workflow",
    "action",
    "pkg",
    "mpkg",
    "dmg",
    "lnk",
    "url",
    "webloc",
    "inetloc",
    "terminal",
    "py",
    "rb",
    "pl",
    "dll",
    "dylib",
    "so",
    "deb",
    "rpm",
    "apk",
    "appx",
    "msix",
    "reg",
    "inf",
    "desktop",
    "xll",
    "xlam",
    "docm",
    "xlsm",
    "pptm",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn id(h: &[u8]) -> &'static str {
        detect(h, h.len() as u64).id
    }

    #[test]
    fn detects_common_formats_by_content() {
        assert_eq!(id(b"\xFF\xD8\xFF\xE0\0\x10JFIF"), "jpeg");
        assert_eq!(id(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), "png");
        assert_eq!(id(b"GIF89a\x01\0"), "gif");
        assert_eq!(id(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
        assert_eq!(id(b"RIFF\0\0\0\0WAVEfmt "), "wav");
        assert_eq!(id(b"\0\0\0\x18ftypheic\0\0\0\0"), "heif");
        assert_eq!(id(b"\0\0\0\x18ftypqt  \0\0\0\0"), "mov");
        assert_eq!(id(b"\0\0\0\x18ftypisom\0\0\0\0"), "mp4");
        assert_eq!(id(b"\0\0\0\x18ftypM4A \0\0\0\0"), "m4a");
        assert_eq!(id(b"\x1A\x45\xDF\xA3\x9f\x42\x86\x81\x01webm"), "webm");
        assert_eq!(id(b"ID3\x04\0\0"), "mp3");
        assert_eq!(id(b"fLaC\0\0\0\x22"), "flac");
        assert_eq!(id(b"%PDF-1.7\n"), "pdf");
        assert_eq!(id(b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x05\0\0\0hello"), "zip");
        assert_eq!(id(b"\xCF\xFA\xED\xFE\x07\0\0\x01"), "macho");
        assert_eq!(id(b"\xCA\xFE\xBA\xBE\0\0\0\x02"), "macho");
        assert_eq!(id(b"\xCA\xFE\xBA\xBE\0\0\0\x34"), "javaclass");
        assert_eq!(id(b"\x7FELF\x02\x01"), "elf");
        assert_eq!(id(b"MZ\x90\0\x03"), "pe");
        assert_eq!(id(b"#!/bin/sh\necho"), "shebang");
        assert_eq!(id(b"#!AMR\n"), "amr");
        assert_eq!(id(b"\xEF\xBB\xBF  <!DOCTYPE html><html>"), "html");
        assert_eq!(id(b"<?xml version=\"1.0\"?>\n<svg xmlns="), "svg");
        assert_eq!(id(b"SQLite format 3\0"), "sqlite");
        assert_eq!(id(b"hello world\n"), "text");
        assert_eq!(id(b"\x00\x13\x37\xFF\xEE"), "unknown");
        assert_eq!(detect(b"", 0).id, "empty");
    }

    #[test]
    fn zip_based_formats_are_recognised() {
        let mut h = b"PK\x03\x04".to_vec();
        h.extend([0u8; 22]);
        h.extend((8u16).to_le_bytes());
        h.extend([0u8; 2]);
        h.extend(b"mimetypeapplication/epub+zip");
        assert_eq!(id(&h), "epub");
        let mut h = b"PK\x03\x04".to_vec();
        h.extend([0u8; 22]);
        h.extend((19u16).to_le_bytes());
        h.extend([0u8; 2]);
        h.extend(b"[Content_Types].xml");
        assert_eq!(id(&h), "ooxml");
    }

    #[test]
    fn extension_expectations() {
        assert!(expected_ids("jpg").unwrap().contains(&"jpeg"));
        assert!(!expected_ids("jpg").unwrap().contains(&"macho"));
        assert!(expected_ids("docx").unwrap().contains(&"ooxml"));
        assert!(expected_ids("weird").is_none());
        assert_eq!(claimed_family("jpg"), Some(Family::Image));
        assert_eq!(claimed_family("exe"), None);
    }
}
