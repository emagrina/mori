//! Sanitized copies: the same image with its metadata removed, produced
//! **inside the sandboxed worker** by rewriting the container, never by
//! re-encoding. Pixels are not touched (no generation loss).
//!
//! - JPEG: drops EXIF, XMP, IPTC/Photoshop, comments, MPF and every other
//!   APPn segment except JFIF, ICC profiles and Adobe's colour-transform
//!   marker; anything after the end of the main image (MPF secondary images,
//!   depth/gain maps, appended data) is dropped too.
//! - PNG: keeps only image-defining chunks (IHDR, PLTE, IDAT, IEND, colour,
//!   transparency, physical size and animation chunks).
//! - WebP: keeps only image chunks; the VP8X flags are updated.
//!
//! The EXIF orientation is the one fact kept (as a minimal EXIF block with a
//! single Orientation field) so the copy isn't displayed rotated.
//! The host verifies the output before writing it (see `metascan.rs`).

/// Formats Mori can sanitize.
pub fn supported(head: &[u8]) -> bool {
    head.starts_with(&[0xFF, 0xD8, 0xFF])
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || (head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP")
}

pub fn sanitize(d: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut tagged = b"FILE".to_vec();
    tagged.extend_from_slice(&d[..d.len().min(4 * 1024 * 1024)]);
    let orientation = crate::metadata::extract(&tagged).and_then(|m| m.orientation).filter(|o| *o != 1);
    if d.starts_with(&[0xFF, 0xD8, 0xFF]) {
        jpeg(d, orientation)
    } else if d.starts_with(b"\x89PNG\r\n\x1a\n") {
        png(d, orientation)
    } else if d.len() >= 12 && &d[0..4] == b"RIFF" && &d[8..12] == b"WEBP" {
        webp(d, orientation)
    } else {
        Err("unsupported")
    }
}

/// Big-endian TIFF with one IFD holding only Orientation.
pub fn orientation_tiff(o: u16) -> Vec<u8> {
    let mut t = b"MM\0*".to_vec();
    t.extend(8u32.to_be_bytes());
    t.extend(1u16.to_be_bytes());
    t.extend(0x0112u16.to_be_bytes());
    t.extend(3u16.to_be_bytes());
    t.extend(1u32.to_be_bytes());
    t.extend(o.to_be_bytes());
    t.extend([0, 0]);
    t.extend(0u32.to_be_bytes());
    t
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?) as usize)
}

fn jpeg(d: &[u8], orientation: Option<u16>) -> Result<Vec<u8>, &'static str> {
    let mut out = vec![0xFF, 0xD8];
    let mut wrote_exif = false;
    let add_exif = |out: &mut Vec<u8>, wrote: &mut bool| {
        if let (Some(o), false) = (orientation, *wrote) {
            let mut p = b"Exif\0\0".to_vec();
            p.extend(orientation_tiff(o));
            out.extend([0xFF, 0xE1]);
            out.extend(((p.len() + 2) as u16).to_be_bytes());
            out.extend(p);
        }
        *wrote = true;
    };
    let keep_app = |marker: u8, seg: &[u8]| match marker {
        0xE0 => seg.starts_with(b"JFIF\0") || seg.starts_with(b"JFXX\0"),
        0xE2 => seg.starts_with(b"ICC_PROFILE\0"),
        0xEE => seg.starts_with(b"Adobe"),
        _ => false,
    };
    let mut i = 2;
    // Header segments, up to the first scan.
    loop {
        if i + 4 > d.len() || d[i] != 0xFF {
            return Err("damaged JPEG");
        }
        let marker = d[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = be16(d, i + 2).ok_or("damaged JPEG")?;
        if len < 2 || i + 2 + len > d.len() {
            return Err("damaged JPEG");
        }
        let seg = &d[i + 4..i + 2 + len];
        let is_app = (0xE0..=0xEF).contains(&marker) || marker == 0xFE;
        if !(marker == 0xE0 && keep_app(marker, seg)) {
            add_exif(&mut out, &mut wrote_exif);
        }
        if !is_app || keep_app(marker, seg) {
            out.extend(&d[i..i + 2 + len]);
        }
        i += 2 + len;
        if marker == 0xDA {
            break;
        }
    }
    // Entropy-coded data and any further scans, up to the end of the main image.
    loop {
        let rel = d[i..].iter().position(|b| *b == 0xFF).ok_or("truncated JPEG")?;
        out.extend(&d[i..i + rel]);
        i += rel;
        let next = *d.get(i + 1).ok_or("truncated JPEG")?;
        match next {
            0x00 | 0xD0..=0xD7 => {
                out.extend(&d[i..i + 2]);
                i += 2;
            }
            0xFF => i += 1,
            0xD9 => {
                out.extend([0xFF, 0xD9]);
                return Ok(out);
            }
            m => {
                let len = be16(d, i + 2).ok_or("truncated JPEG")?;
                if len < 2 || i + 2 + len > d.len() {
                    return Err("truncated JPEG");
                }
                let seg = &d[i + 4..i + 2 + len];
                let is_app = (0xE0..=0xEF).contains(&m) || m == 0xFE;
                if !is_app || keep_app(m, seg) {
                    out.extend(&d[i..i + 2 + len]);
                }
                i += 2 + len;
            }
        }
    }
}

const PNG_KEEP: &[&[u8; 4]] = &[
    b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"iCCP", b"sBIT", b"pHYs", b"bKGD",
    b"hIST", b"acTL", b"fcTL", b"fdAT", b"cICP", b"mDCv", b"cLLi",
];

fn png_chunk(out: &mut Vec<u8>, t: &[u8], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    out.extend(t);
    out.extend(data);
    let mut h = crc32fast::Hasher::new();
    h.update(t);
    h.update(data);
    out.extend(h.finalize().to_be_bytes());
}

fn png(d: &[u8], orientation: Option<u16>) -> Result<Vec<u8>, &'static str> {
    let mut out = d[..8].to_vec();
    let mut i = 8;
    loop {
        if i + 12 > d.len() {
            return Err("truncated PNG");
        }
        let len = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as usize;
        let t = &d[i + 4..i + 8];
        if i + 12 + len > d.len() {
            return Err("truncated PNG");
        }
        if PNG_KEEP.iter().any(|k| *k == t) {
            out.extend(&d[i..i + 12 + len]);
        }
        if t == b"IHDR" {
            if let Some(o) = orientation {
                png_chunk(&mut out, b"eXIf", &orientation_tiff(o));
            }
        }
        i += 12 + len;
        if t == b"IEND" {
            return Ok(out);
        }
    }
}

const WEBP_KEEP: &[&[u8; 4]] = &[b"VP8X", b"VP8 ", b"VP8L", b"ALPH", b"ICCP", b"ANIM", b"ANMF"];

fn webp(d: &[u8], orientation: Option<u16>) -> Result<Vec<u8>, &'static str> {
    let mut body = b"WEBP".to_vec();
    let mut i = 12;
    let mut vp8x_at = None;
    while i + 8 <= d.len() {
        let len = u32::from_le_bytes(d[i + 4..i + 8].try_into().unwrap()) as usize;
        let end = i + 8 + len + (len & 1);
        if i + 8 + len > d.len() {
            return Err("truncated WebP");
        }
        let t = &d[i..i + 4];
        if WEBP_KEEP.iter().any(|k| *k == t) {
            if t == b"VP8X" {
                vp8x_at = Some(body.len());
            }
            body.extend(&d[i..end.min(d.len())]);
            if end > d.len() {
                body.push(0);
            }
        }
        i = end;
    }
    if let Some(at) = vp8x_at {
        // Flags byte: clear EXIF (0x08) and XMP (0x04); set EXIF again if orientation is kept.
        let flags = &mut body[at + 8];
        *flags &= !(0x08 | 0x04);
        if let Some(o) = orientation {
            *flags |= 0x08;
            let t = orientation_tiff(o);
            body.extend(b"EXIF");
            body.extend((t.len() as u32).to_le_bytes());
            body.extend(&t);
            if t.len() % 2 == 1 {
                body.push(0);
            }
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{self, tests::make_jpeg, Category};

    fn meta(d: &[u8]) -> metadata::Meta {
        let mut v = b"FILE".to_vec();
        v.extend(d);
        metadata::extract(&v).unwrap()
    }

    #[test]
    fn jpeg_loses_metadata_keeps_pixels_and_orientation() {
        let src = make_jpeg(6);
        assert!(!meta(&src).categories().is_empty());
        // Data after the main image (like MPF secondary images) goes too.
        let mut with_tail = src.clone();
        with_tail.extend(make_jpeg(1));
        let out = sanitize(&with_tail).unwrap();
        let m = meta(&out);
        assert!(m.categories().is_empty(), "{:#?}", m.fields);
        assert!(m.gps.is_none());
        assert_eq!(m.orientation, Some(6));
        assert!(!out.windows(8).any(|w| w == b"SynthCam") && !out.windows(9).any(|w| w == b"Barcelona"));
        let a = image::load_from_memory(&src).unwrap().to_rgb8();
        let b = image::load_from_memory(&out).unwrap().to_rgb8();
        assert_eq!(a, b, "same pixels");
        assert!(out.ends_with(&[0xFF, 0xD9]) && out.len() < with_tail.len() / 2 + 200);
    }

    #[test]
    fn upright_jpeg_gets_no_exif_at_all() {
        let out = sanitize(&make_jpeg(1)).unwrap();
        assert!(!out.windows(4).any(|w| w == b"Exif"));
    }

    #[test]
    fn png_and_webp() {
        let img = image::RgbaImage::from_fn(6, 4, |x, y| image::Rgba([x as u8 * 40, y as u8 * 60, 7, 255]));
        let mut p = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut p), image::ImageFormat::Png).unwrap();
        // Insert text + eXIf chunks after IHDR (33 bytes in).
        let mut tagged = p[..33].to_vec();
        png_chunk(&mut tagged, b"tEXt", b"Author\0Jane Example");
        png_chunk(&mut tagged, b"eXIf", &metadata::tests::make_tiff(3));
        tagged.extend(&p[33..]);
        assert!(meta(&tagged).categories().contains(&Category::Person));
        let out = sanitize(&tagged).unwrap();
        let m = meta(&out);
        assert!(m.categories().is_empty() && m.orientation == Some(3), "{:#?}", m);
        assert_eq!(image::load_from_memory(&out).unwrap().to_rgba8(), img);

        let mut w = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut w)
            .encode(img.as_raw(), 6, 4, image::ExtendedColorType::Rgba8)
            .unwrap();
        let out = sanitize(&w).unwrap();
        assert_eq!(image::load_from_memory(&out).unwrap().to_rgba8(), img);
    }

    #[test]
    fn damaged_inputs_are_refused() {
        let j = make_jpeg(1);
        assert!(sanitize(&j[..j.len() - 40]).is_err(), "no end of image");
        assert!(sanitize(&j[..50]).is_err());
        assert!(sanitize(b"GIF89a").is_err());
        let mut bad = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x01];
        bad.extend([0u8; 10]);
        assert!(sanitize(&bad).is_err());
    }
}
