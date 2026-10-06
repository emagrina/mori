//! Archive inspection: list what an archive contains — never extract it.
//!
//! Supported: ZIP (incl. ZIP64), TAR, gzip-compressed TAR and plain gzip.
//! Everything is bounded: entry counts, bytes read, decompressed bytes (only
//! ever decompressed into memory to look *inside nested archives*, with hard
//! caps), nesting depth and wall-clock time. Declared sizes are only
//! reported; they can lie, so no allocation is ever sized from them.
//! Parsing is memory-safe Rust with every index bounds-checked.

use crate::risk::{Finding, Level};
use crate::secure::display_safe;
use flate2::read::{DeflateDecoder, GzDecoder};
use serde::Serialize;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

pub const MAX_ENTRIES: usize = 200_000;
/// Entries returned to the UI per archive level.
pub const MAX_LISTED: usize = 5_000;
pub const MAX_DEPTH: u8 = 3;
/// Largest nested archive Mori will decompress (into memory) to look inside.
pub const MAX_NESTED_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes decompressed in total while listing one archive.
pub const MAX_INFLATE_TOTAL: u64 = 512 * 1024 * 1024;
/// Largest central directory accepted.
const MAX_CENTRAL_DIR: u64 = 128 * 1024 * 1024;
pub const TIME_BUDGET: Duration = Duration::from_secs(20);
const RATIO_ATTENTION: f64 = 100.0;
const BOMB_RATIO: f64 = 1000.0;
const BOMB_TOTAL: u64 = 50 * 1024 * 1024 * 1024;

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub compressed: u64,
    pub dir: bool,
    pub symlink: bool,
    pub encrypted: bool,
    /// "traversal" | "absolute" | "drive-letter" | "control-chars" | "nested-archive" | "high-ratio"
    pub flags: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nested: Option<Box<Listing>>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub format: &'static str,
    pub entries: Vec<Entry>,
    pub total_entries: u64,
    pub total_size: u64,
    pub total_compressed: u64,
    /// Deepest archive-in-archive level found (0 = no nesting).
    pub nested_depth: u8,
    /// Listing stopped early (limits or time) — the totals are partial.
    pub truncated: bool,
    pub findings: Vec<Finding>,
}

struct Budget {
    started: Instant,
    inflated: u64,
}

impl Budget {
    fn out_of_time(&self) -> bool {
        self.started.elapsed() > TIME_BUDGET
    }
}

fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{v:.1} {}", U[i])
}

fn finding(level: Level, code: &'static str, title: String, detail: String) -> Finding {
    Finding { level, code, title, detail }
}

/// Path checks that matter if the archive were ever extracted.
fn path_flags(raw: &str) -> Vec<&'static str> {
    let mut f = Vec::new();
    let p = raw.replace('\\', "/");
    if p.starts_with('/') {
        f.push("absolute");
    }
    let b = p.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        f.push("drive-letter");
    }
    if p.split('/').any(|c| c == "..") {
        f.push("traversal");
    }
    if raw.chars().any(|c| c.is_control()) {
        f.push("control-chars");
    }
    f
}

fn looks_archive(name: &str, head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        return Some("zip");
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return Some("gzip");
    }
    if head.len() > 262 && &head[257..262] == b"ustar" {
        return Some("tar");
    }
    let n = name.to_lowercase();
    [".zip", ".tar", ".tgz", ".gz", ".jar", ".apk", ".7z", ".rar", ".xz", ".bz2", ".cab", ".iso", ".dmg"]
        .iter()
        .any(|e| n.ends_with(e))
        .then_some("other")
}

/// List an archive file. `kind` is the detected type id ("zip", "tar", "gzip").
pub fn inspect<R: Read + Seek>(r: &mut R, size: u64, kind: &str) -> Result<Listing, String> {
    let mut budget = Budget { started: Instant::now(), inflated: 0 };
    let mut l = match kind {
        "zip" | "jar" | "ooxml" | "odf" | "epub" => zip(r, size, 0, &mut budget)?,
        "tar" => tar(r, "tar", 0, &mut budget),
        "gzip" => gzip(r, size, 0, &mut budget)?,
        _ => return Err("Mori can't list this archive format.".into()),
    };
    summarise(&mut l);
    Ok(l)
}

fn summarise(l: &mut Listing) {
    let flagged = |f: &str| l.entries.iter().filter(|e| e.flags.contains(&f)).count();
    let (trav, abs, links) = (
        flagged("traversal"),
        flagged("absolute") + flagged("drive-letter"),
        l.entries.iter().filter(|e| e.symlink).count(),
    );
    if trav > 0 {
        l.findings.push(finding(
            Level::High,
            "archive-traversal",
            format!("{trav} path{} with “../”", if trav == 1 { "" } else { "s" }),
            "Extracting this archive naively could write files outside the destination folder.".into(),
        ));
    }
    if abs > 0 {
        l.findings.push(finding(
            Level::High,
            "archive-absolute",
            format!("{abs} absolute path{}", if abs == 1 { "" } else { "s" }),
            "Entries name absolute locations (or drive letters) instead of paths inside the archive.".into(),
        ));
    }
    if links > 0 {
        l.findings.push(finding(
            Level::Attention,
            "archive-symlinks",
            format!("{links} symbolic link{}", if links == 1 { "" } else { "s" }),
            "Links inside archives can redirect later files to other places when extracted.".into(),
        ));
    }
    let ratio = if l.total_compressed > 0 { l.total_size as f64 / l.total_compressed as f64 } else { 0.0 };
    if l.total_size > BOMB_TOTAL || (ratio > BOMB_RATIO && l.total_size > 1 << 30) {
        l.findings.push(finding(
            Level::High,
            "archive-bomb",
            "Potential archive bomb".into(),
            format!(
                "Declares {} of content from {} compressed (ratio {ratio:.0}:1). Mori never extracts it.",
                human(l.total_size),
                human(l.total_compressed)
            ),
        ));
    } else if ratio > RATIO_ATTENTION && l.total_size > 100 << 20 {
        l.findings.push(finding(
            Level::Attention,
            "archive-ratio",
            "Extreme compression ratio".into(),
            format!("Content would expand about {ratio:.0} times."),
        ));
    }
    if l.total_entries > 50_000 {
        l.findings.push(finding(
            Level::Attention,
            "archive-entries",
            "Huge number of entries".into(),
            format!("{} entries.", l.total_entries),
        ));
    }
    if l.nested_depth >= 2 {
        l.findings.push(finding(
            Level::Attention,
            "archive-nesting",
            format!("Nested archive depth: {}", l.nested_depth),
            format!("Archives inside archives. Mori looks at most {MAX_DEPTH} levels deep and never extracts them."),
        ));
    } else if l.nested_depth == 1 {
        l.findings.push(finding(
            Level::Info,
            "archive-nested",
            "Contains archives".into(),
            "Some entries are archives themselves.".into(),
        ));
    }
    if l.entries.iter().any(|e| e.encrypted) {
        l.findings.push(finding(
            Level::Info,
            "archive-encrypted",
            "Encrypted entries".into(),
            "Their contents can't be inspected.".into(),
        ));
    }
    if l.truncated {
        l.findings.push(finding(
            Level::Info,
            "archive-partial",
            "Listing stopped early".into(),
            "Mori's limits (entries, bytes or time) were reached; totals are partial.".into(),
        ));
    }
}

// ---------------------------------------------------------------- ZIP

fn u16le(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 2).map(|v| u16::from_le_bytes([v[0], v[1]]) as u64)
}
fn u32le(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as u64)
}
fn u64le(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 8).map(|v| u64::from_le_bytes(v.try_into().unwrap()))
}

fn read_at<R: Read + Seek>(r: &mut R, at: u64, len: u64) -> Result<Vec<u8>, String> {
    r.seek(SeekFrom::Start(at)).map_err(|_| "Read error")?;
    let mut v = Vec::new();
    r.take(len).read_to_end(&mut v).map_err(|_| "Read error")?;
    Ok(v)
}

fn zip<R: Read + Seek>(r: &mut R, size: u64, depth: u8, budget: &mut Budget) -> Result<Listing, String> {
    // End of central directory: in the last 64 KiB + 22 bytes.
    let tail_len = size.min(65_557);
    let tail = read_at(r, size - tail_len, tail_len)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| tail[i..].starts_with(b"PK\x05\x06"))
        .ok_or("Not a readable ZIP archive")?;
    let mut entries_total = u16le(&tail, eocd + 10).ok_or("Bad ZIP")?;
    let mut cd_size = u32le(&tail, eocd + 12).ok_or("Bad ZIP")?;
    let mut cd_off = u32le(&tail, eocd + 16).ok_or("Bad ZIP")?;
    // ZIP64: locator 20 bytes before the EOCD.
    if (entries_total == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_off == 0xFFFF_FFFF)
        && eocd >= 20
        && tail[eocd - 20..].starts_with(b"PK\x06\x07")
    {
        let z64_off = u64le(&tail, eocd - 20 + 8).ok_or("Bad ZIP64")?;
        let z = read_at(r, z64_off, 56)?;
        if z.starts_with(b"PK\x06\x06") {
            entries_total = u64le(&z, 32).ok_or("Bad ZIP64")?;
            cd_size = u64le(&z, 40).ok_or("Bad ZIP64")?;
            cd_off = u64le(&z, 48).ok_or("Bad ZIP64")?;
        }
    }
    if cd_size > MAX_CENTRAL_DIR || cd_off.saturating_add(cd_size) > size {
        return Err("The archive's directory is damaged or implausibly large.".into());
    }
    let cd = read_at(r, cd_off, cd_size)?;
    let mut l = Listing { format: "zip", ..Default::default() };
    let mut i = 0usize;
    let mut count = 0u64;
    while i + 46 <= cd.len() && cd[i..].starts_with(b"PK\x01\x02") {
        if count as usize >= MAX_ENTRIES || budget.out_of_time() {
            l.truncated = true;
            break;
        }
        let made_by = u16le(&cd, i + 4).unwrap_or(0) >> 8;
        let gp = u16le(&cd, i + 8).unwrap_or(0);
        let method = u16le(&cd, i + 10).unwrap_or(0);
        let mut csize = u32le(&cd, i + 20).unwrap_or(0);
        let mut usize_ = u32le(&cd, i + 24).unwrap_or(0);
        let nlen = u16le(&cd, i + 28).unwrap_or(0) as usize;
        let xlen = u16le(&cd, i + 30).unwrap_or(0) as usize;
        let clen = u16le(&cd, i + 32).unwrap_or(0) as usize;
        let ext_attr = u32le(&cd, i + 38).unwrap_or(0);
        let mut local = u32le(&cd, i + 42).unwrap_or(0);
        let Some(name_bytes) = cd.get(i + 46..i + 46 + nlen) else { break };
        let extra = cd.get(i + 46 + nlen..i + 46 + nlen + xlen).unwrap_or(&[]);
        // ZIP64 extra field (id 1): sizes/offset that overflowed 32 bits, in order.
        let mut j = 0;
        while j + 4 <= extra.len() {
            let id = u16le(extra, j).unwrap_or(0);
            let len = u16le(extra, j + 2).unwrap_or(0) as usize;
            if id == 1 {
                let mut k = j + 4;
                for v in [&mut usize_, &mut csize, &mut local] {
                    if *v == 0xFFFF_FFFF {
                        if let Some(x) = u64le(extra, k) {
                            *v = x;
                            k += 8;
                        }
                    }
                }
            }
            j += 4 + len;
        }
        let raw = String::from_utf8_lossy(name_bytes).into_owned();
        let mode = (ext_attr >> 16) as u32;
        let symlink = made_by == 3 && mode & 0o170000 == 0o120000;
        let dir = raw.ends_with('/') || raw.ends_with('\\');
        let mut e = Entry {
            path: display_safe(&raw),
            size: usize_,
            compressed: csize,
            dir,
            symlink,
            encrypted: gp & 1 != 0,
            flags: path_flags(&raw),
            nested: None,
        };
        if csize > 0 && usize_ as f64 / csize as f64 > RATIO_ATTENTION && usize_ > 10 << 20 {
            e.flags.push("high-ratio");
        }
        if !dir && !symlink && depth < MAX_DEPTH {
            let head = zip_entry_head(r, local, method, csize, budget);
            if let Some(kind) = looks_archive(&raw, &head) {
                e.flags.push("nested-archive");
                l.nested_depth = l.nested_depth.max(1);
                if kind != "other" && !e.encrypted && usize_ <= MAX_NESTED_BYTES && csize <= MAX_NESTED_BYTES {
                    if let Some(bytes) = zip_entry_bytes(r, local, method, csize, budget) {
                        let mut c = Cursor::new(bytes);
                        let len = c.get_ref().len() as u64;
                        let inner = match kind {
                            "zip" => zip(&mut c, len, depth + 1, budget).ok(),
                            "gzip" => gzip(&mut c, len, depth + 1, budget).ok(),
                            _ => Some(tar(&mut c, "tar", depth + 1, budget)),
                        };
                        if let Some(inner) = inner {
                            l.nested_depth = l.nested_depth.max(1 + inner.nested_depth);
                            e.nested = Some(Box::new(inner));
                        }
                    }
                }
            }
        }
        l.total_size = l.total_size.saturating_add(usize_);
        l.total_compressed = l.total_compressed.saturating_add(csize);
        if l.entries.len() < MAX_LISTED {
            l.entries.push(e);
        }
        count += 1;
        i += 46 + nlen + xlen + clen;
    }
    // When stopped early, report what the archive declares.
    l.total_entries = if l.truncated { entries_total.max(count) } else { count };
    Ok(l)
}

/// Offset of an entry's data, from its local header.
fn zip_data_offset<R: Read + Seek>(r: &mut R, local: u64) -> Option<u64> {
    let h = read_at(r, local, 30).ok()?;
    if !h.starts_with(b"PK\x03\x04") {
        return None;
    }
    Some(local + 30 + u16le(&h, 26)? + u16le(&h, 28)?)
}

/// First bytes of an entry (decompressed), to recognise nested archives.
fn zip_entry_head<R: Read + Seek>(r: &mut R, local: u64, method: u64, csize: u64, budget: &mut Budget) -> Vec<u8> {
    let Some(off) = zip_data_offset(r, local) else { return Vec::new() };
    let Ok(raw) = read_at(r, off, csize.min(64 * 1024)) else { return Vec::new() };
    match method {
        0 => raw,
        8 => {
            let mut out = Vec::new();
            let _ = DeflateDecoder::new(&raw[..]).take(1024).read_to_end(&mut out);
            budget.inflated += out.len() as u64;
            out
        }
        _ => Vec::new(),
    }
}

/// A whole (small) entry decompressed into memory, within the budget.
fn zip_entry_bytes<R: Read + Seek>(
    r: &mut R,
    local: u64,
    method: u64,
    csize: u64,
    budget: &mut Budget,
) -> Option<Vec<u8>> {
    let off = zip_data_offset(r, local)?;
    let raw = read_at(r, off, csize).ok()?;
    let left = MAX_INFLATE_TOTAL.saturating_sub(budget.inflated).min(MAX_NESTED_BYTES);
    let out = match method {
        0 => raw,
        8 => {
            let mut out = Vec::new();
            DeflateDecoder::new(&raw[..]).take(left + 1).read_to_end(&mut out).ok()?;
            out
        }
        _ => return None,
    };
    if out.len() as u64 > left {
        return None;
    }
    budget.inflated += out.len() as u64;
    Some(out)
}

// ---------------------------------------------------------------- TAR / gzip

fn octal(b: &[u8]) -> u64 {
    // GNU base-256 for big sizes.
    if b.first().is_some_and(|&c| c & 0x80 != 0) {
        return b[1..].iter().fold(0u64, |v, &c| v.saturating_mul(256).saturating_add(c as u64));
    }
    let s = String::from_utf8_lossy(b);
    u64::from_str_radix(s.trim_matches(|c: char| c == '\0' || c == ' '), 8).unwrap_or(0)
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Walk TAR headers from a stream (data is skipped by reading, so this also
/// works on decompressed streams). Bounded by entries, bytes and time.
fn tar<R: Read>(r: &mut R, format: &'static str, depth: u8, budget: &mut Budget) -> Listing {
    let mut l = Listing { format, ..Default::default() };
    let mut hdr = [0u8; 512];
    let mut long_name: Option<String> = None;
    let mut read_total: u64 = 0;
    loop {
        if l.total_entries as usize >= MAX_ENTRIES || budget.out_of_time() || read_total > MAX_INFLATE_TOTAL {
            l.truncated = true;
            break;
        }
        if r.read_exact(&mut hdr).is_err() {
            break;
        }
        read_total += 512;
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let size = octal(&hdr[124..136]);
        let kind = hdr[156];
        let padded = size.div_ceil(512) * 512;
        let mut name = cstr(&hdr[0..100]);
        if &hdr[257..262] == b"ustar" {
            let prefix = cstr(&hdr[345..500]);
            if !prefix.is_empty() {
                name = format!("{prefix}/{name}");
            }
        }
        // GNU long name / pax header: the next entry's real name is in the data.
        if kind == b'L' || kind == b'x' {
            let mut data = Vec::new();
            if r.by_ref().take(padded.min(1 << 20)).read_to_end(&mut data).is_err() {
                break;
            }
            read_total += padded;
            let text = cstr(&data[..data.len().min(size as usize)]);
            long_name = if kind == b'L' {
                Some(text)
            } else {
                text.lines().find_map(|l| l.split_once(" path=").map(|(_, p)| p.to_owned()))
            };
            if padded > 1 << 20 {
                break;
            }
            continue;
        }
        if let Some(n) = long_name.take() {
            name = n;
        }
        let mut e = Entry {
            flags: path_flags(&name),
            dir: kind == b'5' || name.ends_with('/'),
            symlink: kind == b'2' || kind == b'1',
            size: if matches!(kind, b'0' | 0 | b'7') { size } else { 0 },
            compressed: if matches!(kind, b'0' | 0 | b'7') { size } else { 0 },
            path: display_safe(&name),
            ..Default::default()
        };
        // Peek at regular entries to spot nested archives; skip the rest.
        let mut consumed = 0u64;
        if matches!(kind, b'0' | 0 | b'7') && size > 0 {
            let mut head = Vec::new();
            let peek = size.min(512);
            if r.by_ref().take(peek).read_to_end(&mut head).is_err() {
                break;
            }
            consumed = peek;
            if let Some(k) = looks_archive(&name, &head) {
                e.flags.push("nested-archive");
                l.nested_depth = l.nested_depth.max(1);
                if k != "other" && depth < MAX_DEPTH && size <= MAX_NESTED_BYTES {
                    let mut rest = Vec::new();
                    if r.by_ref().take(size - peek).read_to_end(&mut rest).is_ok() {
                        consumed = size;
                        head.extend(rest);
                        let len = head.len() as u64;
                        let mut c = Cursor::new(head);
                        let inner = match k {
                            "zip" => zip(&mut c, len, depth + 1, budget).ok(),
                            "gzip" => gzip(&mut c, len, depth + 1, budget).ok(),
                            _ => Some(tar(&mut c, "tar", depth + 1, budget)),
                        };
                        if let Some(inner) = inner {
                            l.nested_depth = l.nested_depth.max(1 + inner.nested_depth);
                            e.nested = Some(Box::new(inner));
                        }
                    }
                }
            }
        }
        // Skip the remaining data (bounded copy into the void).
        let skip = padded.saturating_sub(consumed);
        if std::io::copy(&mut r.by_ref().take(skip), &mut std::io::sink()).map_or(true, |n| n < skip) {
            l.total_entries += 1;
            l.truncated = true;
            if l.entries.len() < MAX_LISTED {
                l.entries.push(e);
            }
            break;
        }
        read_total += padded;
        l.total_size = l.total_size.saturating_add(e.size);
        l.total_compressed = l.total_compressed.saturating_add(e.size);
        l.total_entries += 1;
        if l.entries.len() < MAX_LISTED {
            l.entries.push(e);
        }
    }
    l
}

/// gzip: if it contains a TAR, list that; otherwise report the single file.
fn gzip<R: Read + Seek>(r: &mut R, size: u64, depth: u8, budget: &mut Budget) -> Result<Listing, String> {
    // ISIZE (uncompressed size mod 2^32) is the last 4 bytes.
    let isize = if size >= 4 { u32le(&read_at(r, size - 4, 4)?, 0).unwrap_or(0) } else { 0 };
    let head = read_at(r, 0, size.min(10 * 1024))?;
    // Original file name (FNAME flag), when no FEXTRA field precedes it.
    let flags = head.get(3).copied().unwrap_or(0);
    let name = if flags & 0x08 != 0 && flags & 0x04 == 0 { cstr(head.get(10..).unwrap_or(&[])) } else { String::new() };
    r.seek(SeekFrom::Start(0)).map_err(|_| "Read error")?;
    let left = MAX_INFLATE_TOTAL.saturating_sub(budget.inflated);
    let mut dec = GzDecoder::new(r).take(left);
    let mut first = vec![0u8; 512];
    let n = dec.read(&mut first).unwrap_or(0);
    if n == 512 && &first[257..262] == b"ustar" {
        let mut chained = Cursor::new(first).chain(&mut dec);
        let mut l = tar(&mut chained, "tar.gz", depth, budget);
        l.total_compressed = size;
        return Ok(l);
    }
    let entry = Entry {
        path: if name.is_empty() { "(compressed data)".into() } else { display_safe(&name) },
        size: isize,
        compressed: size,
        flags: path_flags(&name),
        ..Default::default()
    };
    Ok(Listing {
        format: "gzip",
        total_entries: 1,
        total_size: isize,
        total_compressed: size,
        entries: vec![entry],
        ..Default::default()
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::io::Write;

    /// Minimal ZIP writer for tests (stored or deflated entries).
    pub fn make_zip(entries: &[(&str, &[u8], bool, Option<u32>)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, data, deflate, mode) in entries {
            let (method, body) = if *deflate {
                let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
                e.write_all(data).unwrap();
                (8u16, e.finish().unwrap())
            } else {
                (0u16, data.to_vec())
            };
            let crc = crc32(data);
            let off = out.len() as u32;
            out.extend(b"PK\x03\x04");
            out.extend(20u16.to_le_bytes());
            out.extend([0, 0]);
            out.extend(method.to_le_bytes());
            out.extend([0, 0, 0, 0]);
            out.extend(crc.to_le_bytes());
            out.extend((body.len() as u32).to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            out.extend((name.len() as u16).to_le_bytes());
            out.extend([0, 0]);
            out.extend(name.as_bytes());
            out.extend(&body);
            cd.extend(b"PK\x01\x02");
            cd.extend(((3u16 << 8) | 20).to_le_bytes());
            cd.extend(20u16.to_le_bytes());
            cd.extend([0, 0]);
            cd.extend(method.to_le_bytes());
            cd.extend([0, 0, 0, 0]);
            cd.extend(crc.to_le_bytes());
            cd.extend((body.len() as u32).to_le_bytes());
            cd.extend((data.len() as u32).to_le_bytes());
            cd.extend((name.len() as u16).to_le_bytes());
            cd.extend([0, 0, 0, 0, 0, 0, 0, 0]);
            cd.extend((mode.unwrap_or(0o100644) << 16).to_le_bytes());
            cd.extend(off.to_le_bytes());
            cd.extend(name.as_bytes());
        }
        let cd_off = out.len() as u32;
        out.extend(&cd);
        out.extend(b"PK\x05\x06");
        out.extend([0, 0, 0, 0]);
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        out.extend((cd.len() as u32).to_le_bytes());
        out.extend(cd_off.to_le_bytes());
        out.extend([0, 0]);
        out
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = !0u32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    }

    fn list(bytes: Vec<u8>, kind: &str) -> Listing {
        let len = bytes.len() as u64;
        inspect(&mut Cursor::new(bytes), len, kind).unwrap()
    }

    fn codes(l: &Listing) -> Vec<&'static str> {
        l.findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn traversal_absolute_and_symlinks_are_flagged() {
        let z = make_zip(&[
            ("ok/readme.txt", b"hello", false, None),
            ("../../etc/passwd", b"x", false, None),
            ("/abs/path", b"x", false, None),
            ("C:\\Windows\\evil.dll", b"x", false, None),
            ("link", b"/etc/shadow", false, Some(0o120777)),
        ]);
        let l = list(z, "zip");
        assert_eq!(l.total_entries, 5);
        assert!(l.entries[1].flags.contains(&"traversal"));
        assert!(l.entries[2].flags.contains(&"absolute"));
        assert!(l.entries[3].flags.contains(&"drive-letter"));
        assert!(l.entries[4].symlink);
        let c = codes(&l);
        assert!(
            c.contains(&"archive-traversal") && c.contains(&"archive-absolute") && c.contains(&"archive-symlinks"),
            "{c:?}"
        );
    }

    #[test]
    fn bombs_are_detected_from_declared_sizes_without_extracting() {
        let zeros = vec![0u8; 20 << 20];
        let z = make_zip(&[("zeros.bin", &zeros, true, None)]);
        assert!(z.len() < 100_000, "highly compressible");
        let l = list(z, "zip");
        assert!(l.entries[0].flags.contains(&"high-ratio"));
        // A forged central directory declaring an absurd size is reported, never allocated.
        let mut forged = make_zip(&[("big.bin", b"tiny", false, None)]);
        let cd = forged.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
        forged[cd + 24..cd + 28].copy_from_slice(&u32::MAX.to_le_bytes());
        let l = list(forged, "zip");
        assert_eq!(l.entries[0].size, u32::MAX as u64);
    }

    #[test]
    fn nested_archives_are_listed_with_a_depth_limit() {
        let mut inner = make_zip(&[("payload.txt", b"deep", false, None)]);
        for level in 0..5 {
            inner = make_zip(&[(&format!("level{level}.zip"), &inner, level % 2 == 0, None)]);
        }
        let l = list(inner, "zip");
        assert_eq!(l.nested_depth, MAX_DEPTH);
        assert!(codes(&l).contains(&"archive-nesting"));
        // The deepest level inspected stops recursing.
        let mut cur = &l;
        let mut seen = 0;
        while let Some(n) = cur.entries.first().and_then(|e| e.nested.as_deref()) {
            cur = n;
            seen += 1;
        }
        assert_eq!(seen, MAX_DEPTH as usize);
    }

    #[test]
    fn many_entries_and_truncated_or_garbage_input_do_not_panic() {
        let names: Vec<String> = (0..3000).map(|i| format!("f{i}")).collect();
        let entries: Vec<(&str, &[u8], bool, Option<u32>)> =
            names.iter().map(|n| (n.as_str(), &b""[..], false, None)).collect();
        let l = list(make_zip(&entries), "zip");
        assert_eq!(l.total_entries, 3000);
        let z = make_zip(&[("a", b"hello", false, None)]);
        for cut in [0, 10, 30, z.len() / 2, z.len() - 5] {
            let len = cut as u64;
            let _ = inspect(&mut Cursor::new(z[..cut].to_vec()), len, "zip");
        }
        let _ = inspect(&mut Cursor::new(vec![0xAB; 5000]), 5000, "zip");
        let _ = inspect(&mut Cursor::new(vec![0x1F, 0x8B, 8, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]), 13, "gzip");
    }

    #[test]
    fn tar_and_tar_gz() {
        fn header(name: &str, size: u64, kind: u8) -> Vec<u8> {
            let mut h = vec![0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            let s = format!("{size:011o}\0");
            h[124..136].copy_from_slice(s.as_bytes());
            h[156] = kind;
            h[257..263].copy_from_slice(b"ustar\0");
            h
        }
        let mut tar = Vec::new();
        tar.extend(header("docs/a.txt", 5, b'0'));
        let mut data = b"hello".to_vec();
        data.resize(512, 0);
        tar.extend(&data);
        tar.extend(header("../escape.sh", 0, b'0'));
        tar.extend(header("evil-link", 0, b'2'));
        tar.extend(vec![0u8; 1024]);
        let l = list(tar.clone(), "tar");
        assert_eq!(l.total_entries, 3);
        assert!(l.entries[1].flags.contains(&"traversal"));
        assert!(l.entries[2].symlink);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar).unwrap();
        let l = list(gz.finish().unwrap(), "gzip");
        assert_eq!(l.format, "tar.gz");
        assert_eq!(l.total_entries, 3);
    }
}
