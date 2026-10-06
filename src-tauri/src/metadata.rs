//! Embedded metadata (EXIF, XMP, IPTC, MP4/QuickTime atoms, ID3v2, FLAC
//! Vorbis comments), extracted **inside the sandboxed worker** from untrusted
//! bytes. Everything is bounded: field counts, value lengths, box depth and
//! the input size. Values are plain text; nothing is interpreted beyond that.
//!
//! Each field may carry a sensitivity category, used by the Sensitive
//! Metadata analysis and the inspector. The categories describe what kind of
//! information a field holds (a location, a person, a device…), not a risk
//! judgement.

use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Category {
    Location,
    Person,
    Device,
    Software,
    Comment,
    Identifier,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Field {
    pub group: String,
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<Category>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Meta {
    /// Container that was parsed ("JPEG", "PNG", "MP4", "ID3", …).
    pub container: String,
    pub fields: Vec<Field>,
    /// [latitude, longitude] in degrees, when a valid position was found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps: Option<[f64; 2]>,
    /// EXIF orientation (1–8), kept by sanitized copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u16>,
    /// Some structure was damaged or a limit was reached: the list may be partial.
    #[serde(default)]
    pub partial: bool,
}

const MAX_FIELDS: usize = 1500;
const MAX_VALUE: usize = 400;
const MAX_XMP: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 8;

impl Meta {
    fn push(&mut self, group: &str, name: &str, value: &str, sensitive: Option<Category>) {
        if self.fields.len() >= MAX_FIELDS {
            self.partial = true;
            return;
        }
        let value = clean(value);
        if value.is_empty() {
            return;
        }
        // The same fact repeated by several blocks (EXIF and XMP) is listed once per group.
        if self.fields.iter().any(|f| f.group == group && f.name == name && f.value == value) {
            return;
        }
        self.fields.push(Field { group: group.into(), name: clean(name), value, sensitive });
    }

    fn set_gps(&mut self, lat: f64, lon: f64) {
        if self.gps.is_none() && valid_position(lat, lon) {
            self.gps = Some([lat, lon]);
        }
    }

    pub fn categories(&self) -> Vec<Category> {
        let mut c: Vec<Category> = self.fields.iter().filter_map(|f| f.sensitive).collect();
        if self.gps.is_some() {
            c.push(Category::Location);
        }
        c.sort();
        c.dedup();
        c
    }
}

/// Plausible coordinates; (0, 0) is the usual "unset" value, not a place.
pub fn valid_position(lat: f64, lon: f64) -> bool {
    lat.is_finite() && lon.is_finite() && lat.abs() <= 90.0 && lon.abs() <= 180.0 && !(lat == 0.0 && lon == 0.0)
}

/// Printable, single-line, bounded text.
fn clean(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_control()
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
            {
                ' '
            } else {
                c
            }
        })
        .collect();
    let s = s.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    if s.chars().count() > MAX_VALUE {
        let mut t: String = s.chars().take(MAX_VALUE).collect();
        t.push('…');
        t
    } else {
        s.to_owned()
    }
}

fn utf16(b: &[u8], big_endian: bool) -> String {
    let u: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| if big_endian { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) })
        .collect();
    String::from_utf16_lossy(&u)
}

fn u16be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64be(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// Entry point. Input: a 4-byte tag, then data:
///   `FILE` + the start of a file (or all of it), or `MP4\0` + a `moov` payload.
pub fn extract(input: &[u8]) -> Option<Meta> {
    let (tag, data) = input.split_at_checked(4)?;
    let mut m = Meta::default();
    match tag {
        b"MP4\0" => {
            m.container = "MP4".into();
            mp4_boxes(data, &mut m, 0, "moov");
        }
        b"FILE" => file(data, &mut m),
        _ => return None,
    }
    Some(m)
}

fn file(d: &[u8], m: &mut Meta) {
    if d.starts_with(&[0xFF, 0xD8, 0xFF]) {
        m.container = "JPEG".into();
        jpeg(d, m);
    } else if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        m.container = "PNG".into();
        png(d, m);
    } else if d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WEBP" {
        m.container = "WebP".into();
        webp(d, m);
    } else if d.starts_with(b"II*\0") || d.starts_with(b"MM\0*") {
        m.container = "TIFF".into();
        exif_tiff(d, m);
        xmp_scan(d, m);
    } else if d.len() >= 12 && &d[4..8] == b"ftyp" {
        let brand = &d[8..12];
        if [b"heic", b"heix", b"mif1", b"msf1", b"avif", b"heim", b"heis", b"hevc"].iter().any(|b| *b == brand) {
            m.container = if brand == b"avif" { "AVIF" } else { "HEIF" }.into();
            heif(d, m);
        } else {
            m.container = if brand.starts_with(b"qt") { "QuickTime" } else { "MP4" }.into();
            mp4_top(d, m);
        }
    } else if d.len() >= 8 && matches!(&d[4..8], b"moov" | b"mdat" | b"wide" | b"free" | b"skip") {
        m.container = "QuickTime".into();
        mp4_top(d, m);
    } else if d.starts_with(b"ID3") {
        m.container = "ID3".into();
        id3(d, m);
    } else if d.starts_with(b"fLaC") {
        m.container = "FLAC".into();
        flac(d, m);
    } else {
        m.container = "Unknown".into();
    }
}

// ------------------------------------------------------------------ EXIF

fn exif_category(tag: exif::Tag) -> Option<Category> {
    use exif::Tag as T;
    if tag.context() == exif::Context::Gps {
        // Every GPS IFD field describes where (and often when) the photo was taken.
        return Some(Category::Location);
    }
    Some(match tag {
        T::Artist | T::Copyright | T::CameraOwnerName => Category::Person,
        T::Make
        | T::Model
        | T::LensMake
        | T::LensModel
        | T::BodySerialNumber
        | T::LensSerialNumber
        | T::LensSpecification => Category::Device,
        T::Software => Category::Software,
        T::ImageDescription | T::UserComment => Category::Comment,
        T::ImageUniqueID => Category::Identifier,
        _ => {
            // XP* tags (Windows): 0x9C9B title, 0x9C9C comment, 0x9C9D author, 0x9C9E keywords, 0x9C9F subject.
            return match (tag.context(), tag.number()) {
                (exif::Context::Tiff, 0x9C9D) => Some(Category::Person),
                (exif::Context::Tiff, 0x013C) => Some(Category::Software), // HostComputer
                (exif::Context::Tiff, 0x9C9B | 0x9C9C | 0x9C9F) => Some(Category::Comment),
                _ => None,
            };
        }
    })
}

fn exif_fields(ex: &exif::Exif, m: &mut Meta) {
    use exif::{In, Tag, Value};
    for f in ex.fields() {
        if f.ifd_num != In::PRIMARY && f.tag.context() == exif::Context::Tiff {
            continue; // the embedded thumbnail's own tags
        }
        // Binary blobs: listed, never dumped.
        if matches!(
            f.tag,
            Tag::MakerNote | Tag::JPEGInterchangeFormat | Tag::JPEGInterchangeFormatLength | Tag::StripOffsets
        ) {
            if f.tag == Tag::MakerNote {
                m.push(
                    "EXIF",
                    "MakerNote",
                    &match &f.value {
                        Value::Undefined(b, _) | Value::Byte(b) => format!("Manufacturer data ({} bytes)", b.len()),
                        _ => "Manufacturer data".into(),
                    },
                    Some(Category::Device),
                );
            }
            continue;
        }
        let name = f.tag.to_string();
        let name = if name.starts_with("Tag(") { format!("Tag 0x{:04X}", f.tag.number()) } else { name };
        let value = match (&f.value, f.tag.number()) {
            // Windows XP* strings are UTF-16LE in a BYTE array.
            (Value::Byte(b), 0x9C9B..=0x9C9F) => utf16(b, false),
            (Value::Undefined(b, _), _) if f.tag == Tag::UserComment => user_comment(b),
            // Plain text without the quotes the crate's display adds.
            (Value::Ascii(parts), _) => {
                parts.iter().map(|p| String::from_utf8_lossy(p).into_owned()).collect::<Vec<_>>().join(", ")
            }
            _ => f.display_value().with_unit(ex).to_string(),
        };
        let group = if f.tag.context() == exif::Context::Gps { "GPS" } else { "EXIF" };
        m.push(group, &name, &value, exif_category(f.tag));
    }
    if let Some(o) = ex.get_field(Tag::Orientation, In::PRIMARY).and_then(|f| f.value.get_uint(0)) {
        if (1..=8).contains(&o) {
            m.orientation = Some(o as u16);
        }
    }
    let coord = |t: Tag, r: Tag| -> Option<f64> {
        let v = match &ex.get_field(t, In::PRIMARY)?.value {
            Value::Rational(v) if v.len() >= 3 => v[0].to_f64() + v[1].to_f64() / 60.0 + v[2].to_f64() / 3600.0,
            Value::Rational(v) if !v.is_empty() => v[0].to_f64(),
            _ => return None,
        };
        let neg = match &ex.get_field(r, In::PRIMARY)?.value {
            Value::Ascii(a) => a.first().is_some_and(|s| s.first().is_some_and(|c| *c == b'S' || *c == b'W')),
            _ => false,
        };
        Some(if neg { -v } else { v })
    };
    if let (Some(lat), Some(lon)) =
        (coord(Tag::GPSLatitude, Tag::GPSLatitudeRef), coord(Tag::GPSLongitude, Tag::GPSLongitudeRef))
    {
        m.set_gps(lat, lon);
    }
}

fn user_comment(b: &[u8]) -> String {
    let (code, text) = b.split_at(b.len().min(8));
    match code {
        b"UNICODE\0" => utf16(text, true),
        _ => String::from_utf8_lossy(text).into_owned(),
    }
}

fn exif_tiff(tiff: &[u8], m: &mut Meta) {
    let r = exif::Reader::new().continue_on_error(true).read_raw(tiff.to_vec());
    match r.or_else(|e| e.distill_partial_result(|_| {})) {
        Ok(ex) => exif_fields(&ex, m),
        Err(_) => m.partial = true,
    }
}

fn heif(d: &[u8], m: &mut Meta) {
    let mut cur = std::io::BufReader::new(std::io::Cursor::new(d));
    match exif::Reader::new()
        .continue_on_error(true)
        .read_from_container(&mut cur)
        .or_else(|e| e.distill_partial_result(|_| {}))
    {
        Ok(ex) => exif_fields(&ex, m),
        Err(exif::Error::NotFound(_)) => {}
        Err(_) => m.partial = true,
    }
    xmp_scan(d, m);
}

// ------------------------------------------------------------------ JPEG

fn jpeg(d: &[u8], m: &mut Meta) {
    let mut i = 2;
    while i + 4 <= d.len() {
        if d[i] != 0xFF {
            m.partial = true;
            return;
        }
        let marker = d[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            return; // image data follows: no more metadata segments before it
        }
        let Some(len) = u16be(d, i + 2).map(usize::from) else { return };
        if len < 2 {
            m.partial = true;
            return;
        }
        let Some(seg) = d.get(i + 4..i + 2 + len) else {
            m.partial = true;
            return;
        };
        match marker {
            0xE1 if seg.starts_with(b"Exif\0\0") => exif_tiff(&seg[6..], m),
            0xE1 if seg.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => xmp(&seg[29..], m),
            0xE1 if seg.starts_with(b"http://ns.adobe.com/xmp/extension/\0") => {
                m.push("XMP", "Extended XMP", &format!("{} bytes", seg.len()), None)
            }
            0xED if seg.starts_with(b"Photoshop 3.0\0") => photoshop(&seg[14..], m),
            0xFE => m.push("JPEG", "Comment", &String::from_utf8_lossy(seg), Some(Category::Comment)),
            0xE2 if seg.starts_with(b"MPF\0") => {
                m.push("JPEG", "Multi-picture", "Additional images after the main one", None)
            }
            0xE2 if seg.starts_with(b"ICC_PROFILE\0") => {}
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF if seg.len() >= 5 => {
                if let (Some(h), Some(w)) = (u16be(seg, 1), u16be(seg, 3)) {
                    m.push("Image", "Dimensions", &format!("{w} × {h}"), None);
                }
            }
            _ => {}
        }
        i += 2 + len;
    }
}

/// Photoshop image resources (APP13): IPTC-IIM lives in resource 0x0404.
fn photoshop(d: &[u8], m: &mut Meta) {
    let mut i = 0;
    let mut guard = 0;
    while i + 12 <= d.len() && guard < 1000 {
        guard += 1;
        if &d[i..i + 4] != b"8BIM" {
            return;
        }
        let Some(id) = u16be(d, i + 4) else { return };
        let name_len = d[i + 6] as usize;
        let mut p = i + 7 + name_len;
        if (name_len + 1) % 2 == 1 {
            p += 1;
        }
        let Some(size) = u32be(d, p).map(|s| s as usize) else { return };
        let Some(data) = d.get(p + 4..(p + 4).saturating_add(size)) else {
            m.partial = true;
            return;
        };
        if id == 0x0404 {
            iptc(data, m);
        }
        i = p + 4 + size + (size % 2);
    }
}

fn iptc(d: &[u8], m: &mut Meta) {
    let mut i = 0;
    while i + 5 <= d.len() {
        if d[i] != 0x1C {
            return;
        }
        let (rec, ds) = (d[i + 1], d[i + 2]);
        let Some(len) = u16be(d, i + 3).map(usize::from) else { return };
        if len & 0x8000 != 0 {
            return; // extended dataset sizes: not used for text fields
        }
        let Some(v) = d.get(i + 5..i + 5 + len) else {
            m.partial = true;
            return;
        };
        if rec == 2 {
            use Category::*;
            let (name, cat) = match ds {
                5 => ("Object Name", Some(Comment)),
                25 => ("Keywords", None),
                55 => ("Date Created", None),
                80 => ("By-line", Some(Person)),
                85 => ("By-line Title", Some(Person)),
                90 => ("City", Some(Location)),
                92 => ("Sub-location", Some(Location)),
                95 => ("Province/State", Some(Location)),
                100 => ("Country Code", Some(Location)),
                101 => ("Country", Some(Location)),
                105 => ("Headline", Some(Comment)),
                110 => ("Credit", Some(Person)),
                115 => ("Source", None),
                116 => ("Copyright Notice", Some(Person)),
                118 => ("Contact", Some(Person)),
                120 => ("Caption", Some(Comment)),
                122 => ("Caption Writer", Some(Person)),
                _ => ("", None),
            };
            if !name.is_empty() {
                m.push("IPTC", name, &String::from_utf8_lossy(v), cat);
            }
        }
        i += 5 + len;
    }
}

// ------------------------------------------------------------------- PNG

fn png(d: &[u8], m: &mut Meta) {
    let mut i = 8;
    while i + 12 <= d.len() {
        let Some(len) = u32be(d, i).map(|l| l as usize) else { return };
        let kind = &d[i + 4..i + 8];
        let Some(data) = d.get(i + 8..(i + 8).saturating_add(len)) else {
            m.partial = true;
            return;
        };
        match kind {
            b"IHDR" if data.len() >= 8 => m.push(
                "Image",
                "Dimensions",
                &format!("{} × {}", u32be(data, 0).unwrap_or(0), u32be(data, 4).unwrap_or(0)),
                None,
            ),
            b"eXIf" => exif_tiff(data, m),
            b"tEXt" => {
                if let Some(z) = data.iter().position(|b| *b == 0) {
                    png_text(&String::from_utf8_lossy(&data[..z]), &latin1(&data[z + 1..]), m);
                }
            }
            b"zTXt" => {
                if let Some(z) = data.iter().position(|b| *b == 0) {
                    let text = data.get(z + 2..).map(inflate).unwrap_or_default();
                    png_text(&String::from_utf8_lossy(&data[..z]), &latin1(&text), m);
                }
            }
            b"iTXt" => itxt(data, m),
            b"tIME" if data.len() >= 7 => m.push(
                "PNG",
                "Last Modified",
                &format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                    u16be(data, 0).unwrap_or(0),
                    data[2],
                    data[3],
                    data[4],
                    data[5],
                    data[6]
                ),
                None,
            ),
            b"IEND" => return,
            _ => {}
        }
        i += 12 + len;
    }
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

/// Bounded zlib inflate (compressed text chunks).
fn inflate(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = flate2::read::ZlibDecoder::new(b).take(MAX_XMP as u64).read_to_end(&mut out);
    out
}

fn png_text(key: &str, value: &str, m: &mut Meta) {
    let cat = match key.to_ascii_lowercase().as_str() {
        "author" | "copyright" => Some(Category::Person),
        "software" | "source" => Some(Category::Software),
        "comment" | "description" | "title" | "disclaimer" | "warning" => Some(Category::Comment),
        _ => None,
    };
    if key == "XML:com.adobe.xmp" {
        xmp(value.as_bytes(), m);
    } else {
        m.push("PNG", key, value, cat);
    }
}

fn itxt(d: &[u8], m: &mut Meta) {
    let Some(z) = d.iter().position(|b| *b == 0) else { return };
    let key = String::from_utf8_lossy(&d[..z]).into_owned();
    let (Some(&compressed), Some(rest)) = (d.get(z + 1), d.get(z + 3..)) else { return };
    // language tag \0 translated keyword \0 text
    let Some(a) = rest.iter().position(|b| *b == 0) else { return };
    let Some(b) = rest[a + 1..].iter().position(|b| *b == 0) else { return };
    let text = &rest[a + 1 + b + 1..];
    let text = if compressed == 1 { inflate(text) } else { text.to_vec() };
    png_text(&key, &String::from_utf8_lossy(&text), m);
}

// ------------------------------------------------------------------ WebP

fn webp(d: &[u8], m: &mut Meta) {
    let mut i = 12;
    while i + 8 <= d.len() {
        let Some(len) = u32le(d, i + 4).map(|l| l as usize) else { return };
        let Some(data) = d.get(i + 8..(i + 8).saturating_add(len)) else {
            m.partial = true;
            return;
        };
        match &d[i..i + 4] {
            b"EXIF" => exif_tiff(data.strip_prefix(b"Exif\0\0").unwrap_or(data), m),
            b"XMP " => xmp(data, m),
            _ => {}
        }
        i += 8 + len + (len & 1);
    }
}

// ------------------------------------------------------------------- XMP

/// Find an XMP packet anywhere in the data (HEIF, TIFF and others embed it raw).
fn xmp_scan(d: &[u8], m: &mut Meta) {
    let needle = b"<x:xmpmeta";
    let Some(start) = d.windows(needle.len()).position(|w| w == needle) else { return };
    let end_tag = b"</x:xmpmeta>";
    let tail = &d[start..d.len().min(start + MAX_XMP)];
    let end = tail.windows(end_tag.len()).position(|w| w == end_tag).map_or(tail.len(), |e| e + end_tag.len());
    xmp(&tail[..end], m);
}

fn xmp_category(local: &str) -> Option<Category> {
    use Category::*;
    Some(match local {
        "GPSLatitude" | "GPSLongitude" | "GPSAltitude" | "GPSAltitudeRef" | "GPSTimeStamp" | "GPSDestLatitude"
        | "GPSDestLongitude" | "Location" | "City" | "State" | "Country" | "CountryCode" | "Sublocation"
        | "ProvinceState" | "CountryName" | "WorldRegion" => Location,
        "creator" | "Artist" | "Credit" | "AuthorsPosition" | "Owner" | "OwnerName" | "CameraOwnerName"
        | "CaptionWriter" | "PersonInImage" | "rights" | "Copyright" | "Contributor" | "contributor" | "publisher"
        | "CiEmailWork" | "CiTelWork" | "CiAdrCity" | "CiAdrExtadr" | "CiUrlWork" => Person,
        "Make"
        | "Model"
        | "Lens"
        | "LensModel"
        | "LensInfo"
        | "SerialNumber"
        | "LensSerialNumber"
        | "CameraSerialNumber"
        | "BodySerialNumber"
        | "InternalSerialNumber" => Device,
        "CreatorTool" | "Software" | "softwareAgent" | "HostComputer" => Software,
        "description" | "title" | "UserComment" | "Headline" | "Instructions" | "ImageDescription" => Comment,
        "DocumentID" | "InstanceID" | "OriginalDocumentID" | "ImageUniqueID" | "documentID" | "instanceID" => {
            Identifier
        }
        _ => return None,
    })
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(semi) = rest.find(';').filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// "41,24.5N" / "41,24,30N" / "41.408N" (XMP EXIF coordinates).
fn xmp_coord(s: &str) -> Option<f64> {
    let s = s.trim();
    let dir = s.chars().last()?;
    let neg = match dir {
        'N' | 'E' => false,
        'S' | 'W' => true,
        _ => return s.parse().ok(),
    };
    let parts: Vec<f64> =
        s[..s.len() - 1].split(',').map(|p| p.trim().parse::<f64>()).collect::<Result<_, _>>().ok()?;
    let v = match parts.as_slice() {
        [d] => *d,
        [d, mm] => d + mm / 60.0,
        [d, mm, ss] => d + mm / 60.0 + ss / 3600.0,
        _ => return None,
    };
    Some(if neg { -v } else { v })
}

/// Tolerant, non-validating XMP reader: properties written as attributes or
/// simple elements (including rdf:Bag/Seq/Alt lists). No DTDs, no entity
/// definitions, no external references — just text.
fn xmp(packet: &[u8], m: &mut Meta) {
    let text = String::from_utf8_lossy(&packet[..packet.len().min(MAX_XMP)]);
    let mut props: Vec<(String, String)> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut rest: &str = &text;
    let mut steps = 0;
    while let Some(lt) = rest.find('<') {
        steps += 1;
        if steps > 50_000 {
            m.partial = true;
            break;
        }
        let content = rest[..lt].trim();
        if !content.is_empty() {
            if let Some(owner) = stack.iter().rev().find(|n| !n.starts_with("rdf:")) {
                if owner.contains(':') && stack.last().is_some_and(|l| l == owner || l == "rdf:li") {
                    props.push((owner.clone(), decode_entities(content)));
                }
            }
        }
        rest = &rest[lt + 1..];
        if rest.starts_with("!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('!') {
            rest = rest.find('>').map_or("", |e| &rest[e + 1..]);
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let tag = &rest[..gt];
        rest = &rest[gt + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let name = name.trim();
            if let Some(p) = stack.iter().rposition(|n| n == name) {
                stack.truncate(p);
            }
            continue;
        }
        let self_closing = tag.ends_with('/');
        let tag = tag.trim_end_matches('/');
        let name = tag.split_whitespace().next().unwrap_or("").to_owned();
        // Attributes: prefix:Name="value"
        let mut a = &tag[name.len()..];
        while let Some(eq) = a.find('=') {
            let key = a[..eq].trim().to_owned();
            let after = a[eq + 1..].trim_start();
            let Some(q) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else { break };
            let Some(close) = after[1..].find(q) else { break };
            let value = &after[1..1 + close];
            if key.contains(':')
                && !key.starts_with("xmlns")
                && !key.starts_with("rdf:")
                && !key.starts_with("xml:")
                && key != "x:xmptk"
            {
                props.push((key, decode_entities(value)));
            }
            a = &after[close + 2..];
        }
        if !self_closing && stack.len() < 64 {
            stack.push(name);
        }
    }
    // Lists (several rdf:li for one property) become one comma-separated value.
    let mut merged: Vec<(String, String)> = Vec::new();
    for (k, v) in props {
        match merged.iter_mut().find(|(mk, _)| *mk == k) {
            Some((_, mv)) if mv.len() < MAX_VALUE => {
                mv.push_str(", ");
                mv.push_str(&v);
            }
            Some(_) => {}
            None => merged.push((k, v)),
        }
    }
    let mut lat = None;
    let mut lon = None;
    for (k, v) in &merged {
        let local = k.rsplit(':').next().unwrap_or(k);
        match local {
            "GPSLatitude" => lat = xmp_coord(v),
            "GPSLongitude" => lon = xmp_coord(v),
            _ => {}
        }
        m.push("XMP", k, v, xmp_category(local));
    }
    if let (Some(a), Some(b)) = (lat, lon) {
        m.set_gps(a, b);
    }
}

// ------------------------------------------------------------- MP4 / MOV

/// ISO 6709 "+41.3851+002.1734+012.000/".
pub fn iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    let b = s.as_bytes();
    let starts: Vec<usize> = (0..b.len()).filter(|&i| b[i] == b'+' || b[i] == b'-').collect();
    if starts.len() < 2 || starts[0] != 0 {
        return None;
    }
    let lat: f64 = s[starts[0]..starts[1]].parse().ok()?;
    let lon: f64 = s[starts[1]..*starts.get(2).unwrap_or(&s.len())].parse().ok()?;
    Some((lat, lon))
}

/// Top-level boxes of a file head: find `moov`.
fn mp4_top(d: &[u8], m: &mut Meta) {
    let mut i = 0;
    while i + 8 <= d.len() {
        let size32 = u32be(d, i).unwrap_or(0) as u64;
        let (size, hdr) = match size32 {
            1 => (u64be(d, i + 8).unwrap_or(0), 16),
            0 => ((d.len() - i) as u64, 8),
            s => (s, 8),
        };
        if size < hdr as u64 {
            m.partial = true;
            return;
        }
        if &d[i + 4..i + 8] == b"moov" {
            match d.get(i + hdr..i.saturating_add(size as usize).min(d.len())) {
                Some(moov) => {
                    if (i as u64 + size) > d.len() as u64 {
                        m.partial = true;
                    }
                    mp4_boxes(moov, m, 0, "moov");
                }
                None => m.partial = true,
            }
            return;
        }
        i = match i.checked_add(size as usize) {
            Some(n) => n,
            None => return,
        };
    }
}

fn mp4_text_category(key: &str) -> Option<Category> {
    use Category::*;
    let k = key.rsplit('.').next().unwrap_or(key);
    Some(match (key, k) {
        (_, "ISO6709") | (_, "name") if key.contains("location") => Location,
        ("©xyz", _) => Location,
        (_, "make" | "model" | "camera.lens_model" | "camera.identifier") | ("©mak" | "©mod", _) => Device,
        (_, "software" | "encoder") | ("©swr" | "©too" | "©enc", _) => Software,
        (_, "author" | "artist" | "copyright" | "owner") | ("©aut" | "©cpy" | "©prd", _) => Person,
        (_, "comment" | "description" | "information" | "title") | ("©cmt" | "©des" | "©inf", _) => Comment,
        (_, "identifier") => Identifier,
        _ => return None,
    })
}

fn mp4_label(t: &[u8]) -> String {
    t.iter().map(|&c| if c == 0xA9 { '©' } else { c as char }).collect()
}

/// Children of an MP4 container box.
fn mp4_boxes(d: &[u8], m: &mut Meta, depth: usize, parent: &str) {
    if depth > MAX_DEPTH {
        m.partial = true;
        return;
    }
    let mut keys: Vec<String> = Vec::new();
    let mut i = 0;
    let mut count = 0;
    while i + 8 <= d.len() {
        count += 1;
        if count > 4096 {
            m.partial = true;
            return;
        }
        let size32 = u32be(d, i).unwrap_or(0) as usize;
        let (size, hdr) = match size32 {
            1 => (u64be(d, i + 8).unwrap_or(0).min(usize::MAX as u64) as usize, 16),
            0 => (d.len() - i, 8),
            s => (s, 8),
        };
        if size < hdr || i + size > d.len() {
            m.partial = true;
            return;
        }
        let t = &d[i + 4..i + 8];
        let body = &d[i + hdr..i + size];
        let name = mp4_label(t);
        match t {
            b"mvhd" if body.len() >= 20 => {
                let secs = if body[0] == 1 { u64be(body, 4) } else { u32be(body, 4).map(u64::from) };
                if let Some(s) = secs.filter(|s| *s > 2_082_844_800) {
                    m.push("QuickTime", "Created", &format_unix((s - 2_082_844_800) as i64), None);
                }
            }
            b"udta" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"moov" => {
                // Only metadata-bearing branches are of interest.
                if matches!(t, b"udta" | b"moov" | b"trak") {
                    mp4_boxes(body, m, depth + 1, &name);
                }
            }
            b"meta" => {
                // QuickTime `meta` has children directly; ISO `meta` is a full box (4 bytes of version/flags first).
                let off = if body.get(4..8) == Some(b"hdlr") { 0 } else { 4 };
                mp4_boxes(body.get(off..).unwrap_or(&[]), m, depth + 1, "meta");
            }
            b"keys" if body.len() >= 8 => {
                let n = u32be(body, 4).unwrap_or(0) as usize;
                let mut p = 8;
                for _ in 0..n.min(512) {
                    let Some(ks) = u32be(body, p).map(|s| s as usize) else { break };
                    if ks < 8 || p + ks > body.len() {
                        m.partial = true;
                        break;
                    }
                    keys.push(String::from_utf8_lossy(&body[p + 8..p + ks]).into_owned());
                    p += ks;
                }
            }
            b"ilst" => ilst(body, &keys, m),
            _ if t[0] == 0xA9 && parent == "udta" => {
                // QuickTime user data text: u16 length, u16 language, text.
                let text = if body.len() >= 4 && u16be(body, 0).is_some_and(|l| l as usize + 4 <= body.len()) {
                    let l = u16be(body, 0).unwrap() as usize;
                    String::from_utf8_lossy(&body[4..4 + l]).into_owned()
                } else if let Some(v) = data_atom(body) {
                    v
                } else {
                    String::from_utf8_lossy(body).into_owned()
                };
                if t == b"\xA9xyz" {
                    if let Some((a, b)) = iso6709(&text) {
                        m.set_gps(a, b);
                    }
                }
                m.push("QuickTime", &name, &text, mp4_text_category(&name));
            }
            _ => {}
        }
        i += size;
    }
}

/// The value of a `data` atom inside an `ilst` item.
fn data_atom(item: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 16 <= item.len() {
        let size = u32be(item, i)? as usize;
        if size < 16 || i + size > item.len() {
            return None;
        }
        if &item[i + 4..i + 8] == b"data" {
            let kind = u32be(item, i + 8)? & 0x00FF_FFFF;
            let v = &item[i + 16..i + size];
            return Some(match kind {
                1 => String::from_utf8_lossy(v).into_owned(),
                2 => utf16(v, true),
                13 | 14 | 27 => format!("Picture ({} bytes)", v.len()),
                21 | 22 if v.len() <= 8 => v.iter().fold(0u64, |a, b| (a << 8) | *b as u64).to_string(),
                _ => format!("{} bytes of data", v.len()),
            });
        }
        i += size;
    }
    None
}

fn ilst(d: &[u8], keys: &[String], m: &mut Meta) {
    let mut i = 0;
    let mut n = 0;
    while i + 8 <= d.len() && n < 1024 {
        n += 1;
        let Some(size) = u32be(d, i).map(|s| s as usize) else { return };
        if size < 8 || i + size > d.len() {
            m.partial = true;
            return;
        }
        let t = &d[i + 4..i + 8];
        // QuickTime `keys` items are numbered (1-based); iTunes items use four-character codes.
        let key = match u32be(t, 0).and_then(|k| keys.get((k as usize).wrapping_sub(1))) {
            Some(k) => k.clone(),
            None => mp4_label(t),
        };
        if let Some(v) = data_atom(&d[i + 8..i + size]) {
            let label = match key.as_str() {
                "©nam" => "Title",
                "©ART" => "Artist",
                "©alb" => "Album",
                "©day" => "Date",
                "©too" => "Encoder",
                "©cmt" => "Comment",
                "©wrt" => "Composer",
                "covr" => "Cover Art",
                k => k.strip_prefix("com.apple.quicktime.").unwrap_or(k),
            };
            if key.ends_with("location.ISO6709") {
                if let Some((a, b)) = iso6709(&v) {
                    m.set_gps(a, b);
                }
            }
            m.push("QuickTime", label, &v, mp4_text_category(&key));
        }
        i += size;
    }
}

fn format_unix(secs: i64) -> String {
    // Civil date from days since 1970 (UTC), no time-zone database needed.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

// ------------------------------------------------------------------ audio

fn syncsafe(b: &[u8]) -> usize {
    b.iter().take(4).fold(0usize, |a, &c| (a << 7) | (c & 0x7F) as usize)
}

fn id3_text(b: &[u8]) -> String {
    let Some((&enc, t)) = b.split_first() else { return String::new() };
    let s = match enc {
        0 => latin1(t),
        1 | 2 => {
            let (be, t) = match t {
                [0xFE, 0xFF, r @ ..] => (true, r),
                [0xFF, 0xFE, r @ ..] => (false, r),
                r => (enc == 2, r),
            };
            utf16(t, be)
        }
        _ => String::from_utf8_lossy(t).into_owned(),
    };
    s.split('\0').filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn id3(d: &[u8], m: &mut Meta) {
    if d.len() < 10 {
        m.partial = true;
        return;
    }
    let ver = d[3];
    let flags = d[5];
    let size = syncsafe(&d[6..10]);
    let end = (10 + size).min(d.len());
    if 10 + size > d.len() {
        m.partial = true;
    }
    m.push("ID3", "Version", &format!("ID3v2.{ver}"), None);
    if flags & 0x80 != 0 {
        m.push("ID3", "Note", "Unsynchronised tag: frames not listed", None);
        return;
    }
    let mut i = 10;
    if flags & 0x40 != 0 && ver >= 3 {
        // Extended header.
        let ext =
            if ver == 4 { syncsafe(d.get(10..14).unwrap_or(&[])) } else { u32be(d, 10).unwrap_or(0) as usize + 4 };
        i += ext;
    }
    let (id_len, hdr_len) = if ver == 2 { (3, 6) } else { (4, 10) };
    let mut n = 0;
    while i + hdr_len <= end && n < 2000 {
        n += 1;
        let id = &d[i..i + id_len];
        if id[0] == 0 {
            break; // padding
        }
        let fsize = match ver {
            2 => ((d[i + 3] as usize) << 16) | ((d[i + 4] as usize) << 8) | d[i + 5] as usize,
            4 => syncsafe(&d[i + 4..i + 8]),
            _ => u32be(d, i + 4).unwrap_or(0) as usize,
        };
        let Some(body) = d.get(i + hdr_len..(i + hdr_len).saturating_add(fsize)).filter(|_| i + hdr_len + fsize <= end)
        else {
            m.partial = true;
            return;
        };
        let id = String::from_utf8_lossy(id).into_owned();
        id3_frame(&id, body, m);
        i += hdr_len + fsize;
    }
}

fn id3_frame(id: &str, body: &[u8], m: &mut Meta) {
    use Category::*;
    let (label, cat): (&str, Option<Category>) = match id {
        "TIT2" | "TT2" => ("Title", None),
        "TPE1" | "TP1" => ("Artist", None),
        "TALB" | "TAL" => ("Album", None),
        "TYER" | "TDRC" | "TYE" => ("Year", None),
        "TCON" | "TCO" => ("Genre", None),
        "TENC" | "TEN" => ("Encoded by", Some(Software)),
        "TSSE" | "TSS" => ("Encoder settings", Some(Software)),
        "TOWN" => ("File owner", Some(Person)),
        "TCOP" | "TCR" => ("Copyright", Some(Person)),
        "COMM" | "COM" => ("Comment", Some(Comment)),
        "USLT" | "ULT" => ("Lyrics", None),
        "APIC" | "PIC" => ("Picture", None),
        "GEOB" | "GEO" => ("Embedded object", None),
        "PRIV" => ("Private data", Some(Identifier)),
        "UFID" | "UFI" => ("Unique file identifier", Some(Identifier)),
        "TXXX" | "TXX" => ("User text", None),
        _ if id.starts_with('W') => ("Link", None),
        _ if id.starts_with('T') => (id, None),
        _ => return,
    };
    let value = match id {
        "APIC" | "PIC" | "GEOB" | "GEO" => format!("{} bytes", body.len()),
        "PRIV" | "UFID" | "UFI" => {
            let owner = body.split(|b| *b == 0).next().unwrap_or(&[]);
            format!("{} ({} bytes)", String::from_utf8_lossy(owner), body.len())
        }
        "COMM" | "COM" | "USLT" | "ULT" if body.len() > 4 => {
            let mut v = vec![body[0]];
            v.extend_from_slice(&body[4..]);
            id3_text(&v)
        }
        _ if id.starts_with('W') && id != "WXXX" => latin1(body),
        _ => id3_text(body),
    };
    let cat = if id == "TXXX" && value.to_ascii_lowercase().contains("location") { Some(Location) } else { cat };
    m.push("ID3", label, &value, cat);
}

fn flac(d: &[u8], m: &mut Meta) {
    let mut i = 4;
    let mut n = 0;
    while i + 4 <= d.len() && n < 128 {
        n += 1;
        let last = d[i] & 0x80 != 0;
        let kind = d[i] & 0x7F;
        let len = ((d[i + 1] as usize) << 16) | ((d[i + 2] as usize) << 8) | d[i + 3] as usize;
        let Some(body) = d.get(i + 4..i + 4 + len) else {
            m.partial = true;
            return;
        };
        match kind {
            4 => vorbis_comments(body, m),
            6 => m.push("FLAC", "Picture", &format!("{} bytes", body.len()), None),
            _ => {}
        }
        if last {
            return;
        }
        i += 4 + len;
    }
}

fn vorbis_comments(d: &[u8], m: &mut Meta) {
    let Some(vlen) = u32le(d, 0).map(|v| v as usize) else { return };
    let Some(vendor) = d.get(4..4 + vlen) else {
        m.partial = true;
        return;
    };
    m.push("Vorbis", "Vendor", &String::from_utf8_lossy(vendor), Some(Category::Software));
    let mut p = 4 + vlen;
    let n = u32le(d, p).unwrap_or(0) as usize;
    p += 4;
    for _ in 0..n.min(1000) {
        let Some(l) = u32le(d, p).map(|l| l as usize) else { break };
        let Some(c) = d.get(p + 4..p + 4 + l) else {
            m.partial = true;
            break;
        };
        let c = String::from_utf8_lossy(c);
        if let Some((k, v)) = c.split_once('=') {
            let cat = match k.to_ascii_uppercase().as_str() {
                "ENCODER" | "ENCODED-BY" => Some(Category::Software),
                "COMMENT" | "DESCRIPTION" => Some(Category::Comment),
                "LOCATION" => Some(Category::Location),
                "COPYRIGHT" | "CONTACT" => Some(Category::Person),
                _ => None,
            };
            m.push("Vorbis", k, v, cat);
        }
        p += 4 + l;
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Little-endian TIFF with IFD0 (Make, Model, Software, Orientation,
    /// Artist) and a GPS IFD (41°23'6"N, 2°10'12"E).
    pub fn make_tiff(orientation: u16) -> Vec<u8> {
        let ascii = |s: &str| {
            let mut v = s.as_bytes().to_vec();
            v.push(0);
            v
        };
        let strings: Vec<(u16, Vec<u8>)> = vec![
            (0x010F, ascii("SynthCam")),
            (0x0110, ascii("Model 7")),
            (0x0131, ascii("Mori Test 1.0")),
            (0x013B, ascii("Jane Example")),
        ];
        // Layout: header(8) IFD0 at 8: n entries + next(4); then data.
        let ifd0_entries = strings.len() + 2; // + Orientation + GPS pointer
        let ifd0_size = 2 + ifd0_entries * 12 + 4;
        let gps_entries = 4;
        let gps_size = 2 + gps_entries * 12 + 4;
        let data_off = 8 + ifd0_size + gps_size;
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        let mut data = Vec::new();
        let mut ifd = Vec::new();
        ifd.extend((ifd0_entries as u16).to_le_bytes());
        let mut entries: Vec<[u8; 12]> = Vec::new();
        for (tag, s) in &strings {
            let mut e = [0u8; 12];
            e[0..2].copy_from_slice(&tag.to_le_bytes());
            e[2..4].copy_from_slice(&2u16.to_le_bytes());
            e[4..8].copy_from_slice(&(s.len() as u32).to_le_bytes());
            e[8..12].copy_from_slice(&((data_off + data.len()) as u32).to_le_bytes());
            data.extend(s);
            entries.push(e);
        }
        let mut e = [0u8; 12];
        e[0..2].copy_from_slice(&0x0112u16.to_le_bytes());
        e[2..4].copy_from_slice(&3u16.to_le_bytes());
        e[4..8].copy_from_slice(&1u32.to_le_bytes());
        e[8..10].copy_from_slice(&orientation.to_le_bytes());
        entries.push(e);
        let mut e = [0u8; 12];
        e[0..2].copy_from_slice(&0x8825u16.to_le_bytes());
        e[2..4].copy_from_slice(&4u16.to_le_bytes());
        e[4..8].copy_from_slice(&1u32.to_le_bytes());
        e[8..12].copy_from_slice(&((8 + ifd0_size) as u32).to_le_bytes());
        entries.push(e);
        entries.sort_by_key(|e| u16::from_le_bytes([e[0], e[1]]));
        for e in &entries {
            ifd.extend(e);
        }
        ifd.extend(0u32.to_le_bytes());
        // GPS IFD
        let rat = |vals: &[(u32, u32)]| {
            vals.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect::<Vec<u8>>()
        };
        let lat = rat(&[(41, 1), (23, 1), (6, 1)]);
        let lon = rat(&[(2, 1), (10, 1), (12, 1)]);
        let mut gps = Vec::new();
        gps.extend((gps_entries as u16).to_le_bytes());
        let mut g = |tag: u16, typ: u16, count: u32, val: [u8; 4]| {
            gps.extend(tag.to_le_bytes());
            gps.extend(typ.to_le_bytes());
            gps.extend(count.to_le_bytes());
            gps.extend(val);
        };
        let lat_off = (data_off + data.len()) as u32;
        let lon_off = lat_off + 24;
        g(1, 2, 2, *b"N\0\0\0");
        g(2, 5, 3, lat_off.to_le_bytes());
        g(3, 2, 2, *b"E\0\0\0");
        g(4, 5, 3, lon_off.to_le_bytes());
        gps.extend(0u32.to_le_bytes());
        data.extend(lat);
        data.extend(lon);
        out.extend(ifd);
        out.extend(gps);
        out.extend(data);
        out
    }

    pub fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut s = vec![0xFF, marker];
        s.extend(((payload.len() + 2) as u16).to_be_bytes());
        s.extend(payload);
        s
    }

    pub const XMP: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:CreatorTool="SynthEdit 2.0">
<dc:creator><rdf:Seq><rdf:li>Jane Example</rdf:li><rdf:li>Joe &amp; Co</rdf:li></rdf:Seq></dc:creator>
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">Family trip</rdf:li></rdf:Alt></dc:description>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

    fn iptc_block() -> Vec<u8> {
        let mut iim = Vec::new();
        for (ds, v) in [(80u8, "Jane Example"), (90, "Barcelona"), (120, "Beach day")] {
            iim.extend([0x1C, 2, ds]);
            iim.extend((v.len() as u16).to_be_bytes());
            iim.extend(v.as_bytes());
        }
        let mut ps = b"Photoshop 3.0\0".to_vec();
        ps.extend(b"8BIM");
        ps.extend(0x0404u16.to_be_bytes());
        ps.extend([0, 0]); // empty pascal name, padded
        ps.extend((iim.len() as u32).to_be_bytes());
        ps.extend(&iim);
        if iim.len() % 2 == 1 {
            ps.push(0);
        }
        ps
    }

    /// A tiny real JPEG (8×8) with EXIF, XMP, IPTC and a comment.
    pub fn make_jpeg(orientation: u16) -> Vec<u8> {
        let img = image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 30, y as u8 * 30, 90]));
        let mut plain = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Jpeg).unwrap();
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(make_tiff(orientation));
        let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        xmp.extend(XMP.as_bytes());
        let mut out = vec![0xFF, 0xD8];
        out.extend(segment(0xE1, &exif));
        out.extend(segment(0xE1, &xmp));
        out.extend(segment(0xED, &iptc_block()));
        out.extend(segment(0xFE, b"shot at grandma's house"));
        out.extend(&plain[2..]);
        out
    }

    fn file(d: &[u8]) -> Meta {
        let mut v = b"FILE".to_vec();
        v.extend(d);
        extract(&v).unwrap()
    }

    fn has(m: &Meta, name: &str, value: &str, cat: Option<Category>) -> bool {
        m.fields.iter().any(|f| f.name == name && f.value.contains(value) && f.sensitive == cat)
    }

    #[test]
    fn jpeg_exif_xmp_iptc_and_gps() {
        let m = file(&make_jpeg(6));
        assert_eq!(m.container, "JPEG");
        assert!(has(&m, "Make", "SynthCam", Some(Category::Device)), "{:#?}", m.fields);
        assert!(m.fields.iter().any(|f| f.name == "Make" && f.value == "SynthCam"), "no display quotes");
        assert!(has(&m, "Software", "Mori Test", Some(Category::Software)));
        assert!(has(&m, "Artist", "Jane Example", Some(Category::Person)));
        assert!(has(&m, "dc:creator", "Jane Example, Joe & Co", Some(Category::Person)));
        assert!(has(&m, "xmp:CreatorTool", "SynthEdit", Some(Category::Software)));
        assert!(has(&m, "dc:description", "Family trip", Some(Category::Comment)));
        assert!(has(&m, "City", "Barcelona", Some(Category::Location)));
        assert!(has(&m, "Comment", "grandma", Some(Category::Comment)));
        assert_eq!(m.orientation, Some(6));
        let [lat, lon] = m.gps.unwrap();
        assert!((lat - 41.385).abs() < 0.001 && (lon - 2.17).abs() < 0.001, "{lat} {lon}");
        assert!(m.categories().contains(&Category::Location));
        assert!(!m.partial);
    }

    #[test]
    fn png_text_chunks_and_exif() {
        let chunk = |t: &[u8], d: &[u8]| {
            let mut c = (d.len() as u32).to_be_bytes().to_vec();
            c.extend(t);
            c.extend(d);
            c.extend(crc32fast::hash(&[t, d].concat()).to_be_bytes());
            c
        };
        let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
        p.extend(chunk(b"IHDR", &[0, 0, 0, 4, 0, 0, 0, 3, 8, 2, 0, 0, 0]));
        p.extend(chunk(b"tEXt", b"Author\0Jane Example"));
        p.extend(chunk(b"tEXt", b"Software\0SynthPaint"));
        p.extend(chunk(b"eXIf", &make_tiff(1)));
        p.extend(chunk(b"IEND", b""));
        let m = file(&p);
        assert!(has(&m, "Author", "Jane", Some(Category::Person)));
        assert!(has(&m, "Software", "SynthPaint", Some(Category::Software)));
        assert!(has(&m, "Make", "SynthCam", Some(Category::Device)));
        assert!(has(&m, "Dimensions", "4 × 3", None));
        assert!(m.gps.is_some());
    }

    pub fn make_box(t: &[u8], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend(t);
        b.extend(body);
        b
    }

    /// moov with mvhd, udta/©xyz and a QuickTime keys/ilst `meta`.
    pub fn make_moov() -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        // 2024-03-01 = 1709251200 unix; +2082844800 (1904 epoch)
        mvhd[4..8].copy_from_slice(&((1_709_251_200u64 + 2_082_844_800) as u32).to_be_bytes());
        let xyz = "+41.3851+002.1734/";
        let mut t = (xyz.len() as u16).to_be_bytes().to_vec();
        t.extend([0x15, 0xC7]);
        t.extend(xyz.as_bytes());
        let udta = make_box(b"udta", &make_box(b"\xA9xyz", &t));
        let keys_list = ["com.apple.quicktime.make", "com.apple.quicktime.software"];
        let mut keys = vec![0, 0, 0, 0];
        keys.extend((keys_list.len() as u32).to_be_bytes());
        for k in keys_list {
            keys.extend(((k.len() + 8) as u32).to_be_bytes());
            keys.extend(b"mdta");
            keys.extend(k.as_bytes());
        }
        let item = |idx: u32, v: &str| {
            let mut data = 1u32.to_be_bytes().to_vec();
            data.extend([0, 0, 0, 0]);
            data.extend(v.as_bytes());
            make_box(&idx.to_be_bytes(), &make_box(b"data", &data))
        };
        let mut ilst = item(1, "Apple");
        ilst.extend(item(2, "17.1"));
        let mut meta = make_box(b"hdlr", &[0u8; 25]);
        meta.extend(make_box(b"keys", &keys));
        meta.extend(make_box(b"ilst", &ilst));
        let mut moov = make_box(b"mvhd", &mvhd);
        moov.extend(udta);
        moov.extend(make_box(b"meta", &meta));
        moov
    }

    #[test]
    fn mp4_location_device_and_dates() {
        let moov = make_moov();
        let mut v = b"MP4\0".to_vec();
        v.extend(&moov);
        let m = extract(&v).unwrap();
        assert!(has(&m, "©xyz", "+41.3851", Some(Category::Location)), "{:#?}", m.fields);
        assert!(has(&m, "make", "Apple", Some(Category::Device)));
        assert!(has(&m, "software", "17.1", Some(Category::Software)));
        assert!(has(&m, "Created", "2024-", None));
        let [lat, lon] = m.gps.unwrap();
        assert!((lat - 41.3851).abs() < 1e-6 && (lon - 2.1734).abs() < 1e-6);
        // The same moov at the top level of a file head.
        let mut f = make_box(b"ftyp", b"qt  \0\0\0\0qt  ");
        f.extend(make_box(b"moov", &moov));
        let m2 = file(&f);
        assert_eq!(m2.gps, m.gps);
    }

    #[test]
    fn id3_and_flac() {
        let frame = |id: &[u8], body: &[u8]| {
            let mut f = id.to_vec();
            f.extend((body.len() as u32).to_be_bytes());
            f.extend([0, 0]);
            f.extend(body);
            f
        };
        let mut frames = frame(b"TIT2", b"\x03Song");
        frames.extend(frame(b"TENC", b"\x03LAME 3.100"));
        frames.extend(frame(b"COMM", b"\x03engxx\0recorded at home"));
        frames.extend(frame(b"APIC", &[0u8; 300]));
        let mut d = b"ID3\x03\x00\x00".to_vec();
        let n = frames.len();
        d.extend([(n >> 21) as u8 & 0x7F, (n >> 14) as u8 & 0x7F, (n >> 7) as u8 & 0x7F, n as u8 & 0x7F]);
        d.extend(frames);
        let m = file(&d);
        assert!(has(&m, "Title", "Song", None));
        assert!(has(&m, "Encoded by", "LAME", Some(Category::Software)));
        assert!(has(&m, "Comment", "recorded at home", Some(Category::Comment)));
        assert!(has(&m, "Picture", "300 bytes", None));

        let mut vc = Vec::new();
        vc.extend(6u32.to_le_bytes());
        vc.extend(b"vendor");
        vc.extend(2u32.to_le_bytes());
        for c in ["TITLE=Track", "COMMENT=voice memo"] {
            vc.extend((c.len() as u32).to_le_bytes());
            vc.extend(c.as_bytes());
        }
        let mut f = b"fLaC".to_vec();
        f.extend([0x84, 0, 0, vc.len() as u8]);
        f.extend(&vc);
        let m = file(&f);
        assert!(has(&m, "COMMENT", "voice memo", Some(Category::Comment)));
        assert!(has(&m, "Vendor", "vendor", Some(Category::Software)));
    }

    #[test]
    fn hostile_inputs_are_bounded() {
        // Truncated everywhere.
        let j = make_jpeg(1);
        for cut in [3, 10, 40, 120, j.len() / 2] {
            let _ = file(&j[..cut]);
        }
        // Segment lengths pointing past the end, zero-length boxes, absurd counts.
        let mut bad = vec![0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF, b'E', b'x'];
        bad.extend([0u8; 16]);
        assert!(file(&bad).partial);
        let mut boxes = Vec::new();
        for _ in 0..20 {
            boxes = make_box(b"udta", &boxes);
        }
        let mut v = b"MP4\0".to_vec();
        v.extend(&boxes);
        assert!(extract(&v).unwrap().partial, "nesting is capped");
        let mut keys = make_box(b"keys", &[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0x40]);
        keys = make_box(b"meta", &keys);
        let mut v = b"MP4\0".to_vec();
        v.extend(&keys);
        let _ = extract(&v);
        // XMP with an entity definition: never expanded.
        let lol =
            br#"<x:xmpmeta><!DOCTYPE x [<!ENTITY a "aaaaaaaa">]><rdf:Description dc:title="&a;&a;"/></x:xmpmeta>"#;
        let mut v = b"FILE".to_vec();
        v.extend(b"II*\0\x08\0\0\0\0\0");
        v.extend(lol);
        let m = extract(&v).unwrap();
        assert!(m.fields.iter().all(|f| !f.value.contains("aaaa")));
        // Control and bidi characters never reach the UI.
        let mut d = vec![0xFF, 0xD8];
        d.extend(segment(0xFE, "evil\u{202E}gpj.exe\x07".as_bytes()));
        let m = file(&d);
        assert!(m.fields.iter().all(|f| !f.value.contains('\u{202E}') && !f.value.contains('\x07')));
        // Unknown and empty input.
        assert_eq!(file(b"").container, "Unknown");
        assert!(extract(b"XX").is_none());
    }

    #[test]
    fn coordinates() {
        assert_eq!(iso6709("+41.3851+002.1734+012.000/"), Some((41.3851, 2.1734)));
        assert_eq!(iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(iso6709("garbage"), None);
        assert!((xmp_coord("41,23.1N").unwrap() - 41.385).abs() < 1e-9);
        assert!((xmp_coord("2,10,12W").unwrap() + 2.17).abs() < 1e-9);
        assert!(!valid_position(0.0, 0.0) && !valid_position(91.0, 0.0) && valid_position(-33.0, 151.0));
    }
}
