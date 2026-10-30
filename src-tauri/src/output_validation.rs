//! Host-side structural checks. No codec, filesystem, XPC or platform decoder calls.
//! The producer's metadata/dimension claims are never sufficient to approve its bytes.

fn be32(b: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(b.try_into().ok()?))
}
fn le32(b: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(b.try_into().ok()?))
}

/// Store display output is deliberately just a static, 8-bit RGBA PNG. No APNG,
/// profiles, text, EXIF, alternate images or trailing data can reach WebKit here.
#[cfg(any(feature = "mac-app-store", test))]
pub fn static_png(bytes: &[u8], edge: u32) -> bool {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || !(32..=4096).contains(&edge) {
        return false;
    }
    let mut at = 8usize;
    let mut count = 0;
    let mut idat = false;
    while at < bytes.len() {
        count += 1;
        if count > 4096 {
            return false;
        }
        let Some(header) = bytes.get(at..at + 8) else { return false };
        let len = be32(&header[..4]).unwrap() as usize;
        let Some(end) = at.checked_add(12).and_then(|n| n.checked_add(len)) else { return false };
        let Some(chunk) = bytes.get(at..end) else { return false };
        if crc32fast::hash(&chunk[4..8 + len]) != be32(&chunk[8 + len..]).unwrap() {
            return false;
        }
        match &header[4..8] {
            b"IHDR" if count == 1 && len == 13 => {
                let w = be32(&chunk[8..12]).unwrap();
                let h = be32(&chunk[12..16]).unwrap();
                if w == 0 || h == 0 || w > edge || h > edge || chunk[16..21] != [8, 6, 0, 0, 0] {
                    return false;
                }
            }
            b"IDAT" if count > 1 => {
                idat = true;
            }
            b"IEND" if idat && len == 0 => return end == bytes.len(),
            _ => return false,
        }
        at = end;
    }
    false
}

// Independent allowlist verifier for lossless metadata stripping. Kept compressed
// image chunks/segments must be byte-identical to the ORIGINAL, not merely decode
// to dimensions reported by the same potentially compromised producer.
#[derive(PartialEq, Eq)]
struct Summary {
    digest: blake3::Hash,
    orientation: Option<u16>,
}
struct Record {
    hash: blake3::Hasher,
    orientation: Option<u16>,
}
impl Record {
    fn new() -> Self {
        Self { hash: blake3::Hasher::new(), orientation: None }
    }
    fn keep(&mut self, kind: &[u8], data: &[u8]) {
        self.hash.update(&(kind.len() as u64).to_le_bytes());
        self.hash.update(kind);
        self.hash.update(&(data.len() as u64).to_le_bytes());
        self.hash.update(data);
    }
    fn orient(&mut self, tiff: &[u8], cleaned: bool) -> Option<()> {
        let o = orientation(tiff)?;
        if cleaned {
            let mut canonical = b"MM\0*\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01".to_vec();
            canonical.extend(o?.to_be_bytes());
            canonical.extend([0u8; 6]);
            if tiff != canonical {
                return None;
            }
        }
        if let Some(o) = o.filter(|o| *o != 1) {
            if self.orientation.is_some_and(|old| old != o) {
                return None;
            }
            self.orientation = Some(o);
        }
        Some(())
    }
    fn finish(self) -> Summary {
        Summary { digest: self.hash.finalize(), orientation: self.orientation }
    }
}
/// Read only the scalar IFD0 orientation; do not follow EXIF/GPS pointers or values.
fn orientation(t: &[u8]) -> Option<Option<u16>> {
    let little = match t.get(..4)? {
        b"II\x2a\0" => true,
        b"MM\0\x2a" => false,
        _ => return None,
    };
    let u16at = |i| -> Option<u16> {
        let b = t.get(i..i + 2)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32at = |i| -> Option<u32> {
        let b = t.get(i..i + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let at = u32at(4)? as usize;
    let n = u16at(at)? as usize;
    if n > 4096 {
        return None;
    }
    let mut found = None;
    for i in 0..n {
        let at = at.checked_add(2 + i * 12)?;
        t.get(at..at + 12)?;
        if u16at(at)? == 0x112 {
            if u16at(at + 2)? != 3 || u32at(at + 4)? != 1 || found.is_some() {
                return None;
            }
            let o = u16at(at + 8)?;
            if !(1..=8).contains(&o) {
                return None;
            }
            found = Some(o);
        }
    }
    Some(found)
}
fn png_summary(b: &[u8], cleaned: bool) -> Option<Summary> {
    if !b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut r = Record::new();
    let mut at = 8usize;
    let mut count = 0;
    let mut image = false;
    while at < b.len() {
        count += 1;
        if count > 65536 {
            return None;
        }
        let h = b.get(at..at + 8)?;
        let n = be32(&h[..4])? as usize;
        let end = at.checked_add(12)?.checked_add(n)?;
        let chunk = b.get(at..end)?;
        let data = &chunk[8..8 + n];
        let kind = &h[4..8];
        if crc32fast::hash(&chunk[4..8 + n]) != be32(&chunk[8 + n..])? {
            return None;
        }
        if count == 1 && (kind != b"IHDR" || n != 13) {
            return None;
        }
        if kind == b"IHDR" && count != 1 {
            return None;
        }
        match kind {
            b"eXIf" => r.orient(data, cleaned)?,
            b"IHDR" | b"PLTE" | b"IDAT" | b"IEND" | b"tRNS" | b"gAMA" | b"cHRM" | b"sRGB" | b"iCCP" | b"sBIT"
            | b"pHYs" | b"bKGD" | b"hIST" | b"acTL" | b"fcTL" | b"fdAT" | b"cICP" | b"mDCv" | b"cLLi" => {
                r.keep(kind, chunk)
            }
            _ if cleaned => return None,
            _ => {}
        }
        if kind == b"IDAT" {
            image = true;
        }
        if kind == b"IEND" {
            if !image || n != 0 || (cleaned && end != b.len()) {
                return None;
            }
            return Some(r.finish());
        }
        at = end;
    }
    None
}
fn jpeg_summary(b: &[u8], cleaned: bool) -> Option<Summary> {
    if !b.starts_with(&[255, 216]) {
        return None;
    }
    let mut r = Record::new();
    let mut at = 2usize;
    let mut scan = false;
    let mut scans = 0;
    let mut count = 0;
    let mut sof = false;
    loop {
        count += 1;
        if count > 65536 {
            return None;
        }
        if scan {
            let start = at;
            loop {
                if *b.get(at)? != 255 {
                    at += 1;
                    continue;
                }
                match *b.get(at + 1)? {
                    0 | 0xd0..=0xd7 => at += 2,
                    255 => {
                        at += 1;
                    }
                    _ => break,
                }
            }
            r.keep(b"entropy", b.get(start..at)?);
            scan = false;
        }
        if *b.get(at)? != 255 {
            return None;
        }
        let m = *b.get(at + 1)?;
        if m == 255 {
            at += 1;
            continue;
        }
        if m == 0xd9 {
            if scans == 0 || !sof || (cleaned && at + 2 != b.len()) {
                return None;
            }
            return Some(r.finish());
        }
        let len = u16::from_be_bytes(b.get(at + 2..at + 4)?.try_into().ok()?) as usize;
        if len < 2 {
            return None;
        }
        let end = at.checked_add(2)?.checked_add(len)?;
        let data = b.get(at + 4..end)?;
        match m {
            0xc0..=0xc2 | 0xc4 | 0xdb | 0xdd | 0xda => {
                if matches!(m, 0xc0..=0xc2) {
                    if sof {
                        return None;
                    }
                    sof = true;
                }
                r.keep(&[m], data);
            }
            0xe0 if data.starts_with(b"JFIF\0") || data.starts_with(b"JFXX\0") => r.keep(&[m], data),
            0xe2 if data.starts_with(b"ICC_PROFILE\0") => r.keep(&[m], data),
            0xee if data.starts_with(b"Adobe") => r.keep(&[m], data),
            0xe1 if data.starts_with(b"Exif\0\0") => r.orient(&data[6..], cleaned)?,
            0xe0..=0xef | 0xfe if !cleaned => {}
            _ => return None,
        }
        if m == 0xda {
            scans += 1;
            if scans > 1024 {
                return None;
            }
            scan = true;
        }
        at = end;
    }
}
fn webp_summary(b: &[u8], cleaned: bool) -> Option<Summary> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"WEBP" {
        return None;
    }
    let size = le32(b.get(4..8)?)? as usize + 8;
    if size > b.len() || (cleaned && size != b.len()) {
        return None;
    }
    let mut r = Record::new();
    let mut at = 12usize;
    let mut count = 0;
    let mut image = false;
    let mut flags = None;
    while at < size {
        count += 1;
        if count > 65536 {
            return None;
        }
        let h = b.get(at..at + 8)?;
        let n = le32(&h[4..])? as usize;
        let end = at.checked_add(8)?.checked_add(n)?.checked_add(n & 1)?;
        if end > size {
            return None;
        }
        let data = b.get(at + 8..at + 8 + n)?;
        match &h[..4] {
            b"EXIF" => r.orient(data, cleaned)?,
            b"VP8X" => {
                if n != 10 || flags.is_some() {
                    return None;
                }
                flags = Some(data[0]);
                if cleaned && data[0] & 4 != 0 {
                    return None;
                }
                let mut normalized = data.to_vec();
                normalized[0] &= !(8 | 4);
                r.keep(b"VP8X", &normalized);
            }
            b"VP8 " | b"VP8L" | b"ALPH" | b"ICCP" | b"ANIM" | b"ANMF" => {
                if matches!(&h[..4], b"VP8 " | b"VP8L" | b"ANMF") {
                    image = true;
                }
                if &h[..4] == b"ANMF" {
                    if n < 16 {
                        return None;
                    }
                    let mut sub = 16;
                    while sub < n {
                        let head = data.get(sub..sub + 8)?;
                        let len = le32(&head[4..])? as usize;
                        if !matches!(&head[..4], b"ALPH" | b"VP8 " | b"VP8L") {
                            return None;
                        }
                        sub = sub.checked_add(8)?.checked_add(len)?.checked_add(len & 1)?;
                        if sub > n {
                            return None;
                        }
                    }
                }
                r.keep(&h[..4], data);
            }
            _ if cleaned => return None,
            _ => {}
        }
        if cleaned && n & 1 != 0 && *b.get(end - 1)? != 0 {
            return None;
        }
        at = end;
    }
    if !image
        || (cleaned && r.orientation.is_some() && flags.is_none())
        || (cleaned && flags.is_some_and(|f| (f & 8 != 0) != r.orientation.is_some()))
    {
        return None;
    }
    Some(r.finish())
}

pub fn verify_sanitized(original: &[u8], output: &[u8]) -> bool {
    if original.len() > 60 * 1024 * 1024 || output.len() > 64 * 1024 * 1024 {
        return false;
    }
    let parse = if original.starts_with(&[255, 216]) {
        jpeg_summary
    } else if original.starts_with(b"\x89PNG") {
        png_summary
    } else {
        webp_summary
    };
    match (parse(original, false), parse(output, true)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> Vec<u8> {
        let img =
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(32, 16, image::Rgba([200, 10, 30, 255])));
        let mut b = std::io::Cursor::new(Vec::new());
        img.write_to(&mut b, image::ImageFormat::Png).unwrap();
        b.into_inner()
    }
    #[test]
    fn static_output_checks_actual_dimensions_crc_and_no_animation_or_trailing_data() {
        let b = png();
        assert!(static_png(&b, 32));
        assert!(!static_png(&b, 16));
        let mut huge = b.clone();
        huge[16..20].copy_from_slice(&5000u32.to_be_bytes());
        let crc = crc32fast::hash(&huge[12..29]);
        huge[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(!static_png(&huge, 4096));
        huge[16..20].copy_from_slice(&64u32.to_be_bytes());
        let crc = crc32fast::hash(&huge[12..29]);
        huge[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(!static_png(&huge, 32));
        let mut animated = b.clone();
        let mut actl = 8u32.to_be_bytes().to_vec();
        actl.extend(b"acTL");
        actl.extend([0, 0, 0, 2, 0, 0, 0, 0]);
        actl.extend(crc32fast::hash(&actl[4..]).to_be_bytes());
        animated.splice(33..33, actl);
        assert!(!static_png(&animated, 32));
        let mut trailing = b.clone();
        trailing.push(0);
        assert!(!static_png(&trailing, 32));
        let mut bad = b;
        bad[29] ^= 1;
        assert!(!static_png(&bad, 32));
    }
    #[test]
    fn independently_verifies_png_and_rejects_injected_metadata_and_changed_pixels() {
        let b = png();
        let clean = crate::sanitize::sanitize(&b).unwrap();
        assert!(verify_sanitized(&b, &clean));
        let mut changed = clean.clone();
        changed[19] ^= 1;
        assert!(!verify_sanitized(&b, &changed));
        // A valid chunk CRC must not make changed compressed image content acceptable.
        let mut altered = clean.clone();
        let mut at = 8;
        loop {
            let n = be32(&altered[at..at + 4]).unwrap() as usize;
            if &altered[at + 4..at + 8] == b"IDAT" {
                altered[at + 8] ^= 1;
                let crc = crc32fast::hash(&altered[at + 4..at + 8 + n]);
                altered[at + 8 + n..at + 12 + n].copy_from_slice(&crc.to_be_bytes());
                break;
            }
            at += 12 + n;
        }
        assert!(!verify_sanitized(&b, &altered));
        let mut extra = clean;
        let payload = b"Author\0secret";
        let mut chunk = (payload.len() as u32).to_be_bytes().to_vec();
        chunk.extend(b"tEXt");
        chunk.extend(payload);
        chunk.extend(crc32fast::hash(&chunk[4..]).to_be_bytes());
        extra.splice(33..33, chunk);
        assert!(!verify_sanitized(&b, &extra));
    }
    #[test]
    fn independently_verifies_jpeg_and_orientation() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(32, 16));
        let mut c = std::io::Cursor::new(Vec::new());
        img.write_to(&mut c, image::ImageFormat::Jpeg).unwrap();
        let jpeg = c.into_inner();
        assert!(verify_sanitized(&jpeg, &crate::sanitize::sanitize(&jpeg).unwrap()));
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend(crate::sanitize::orientation_tiff(6));
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend([255, 225]);
        tagged.extend(((exif.len() + 2) as u16).to_be_bytes());
        tagged.extend(exif);
        tagged.extend(&jpeg[2..]);
        let clean = crate::sanitize::sanitize(&tagged).unwrap();
        assert!(verify_sanitized(&tagged, &clean));
        assert!(!verify_sanitized(&tagged, &jpeg));
    }
    #[test]
    fn independently_verifies_webp() {
        let img = image::DynamicImage::ImageRgba8(image::RgbaImage::new(32, 16));
        let mut c = std::io::Cursor::new(Vec::new());
        img.write_to(&mut c, image::ImageFormat::WebP).unwrap();
        let b = c.into_inner();
        assert!(verify_sanitized(&b, &crate::sanitize::sanitize(&b).unwrap()));
    }
}
