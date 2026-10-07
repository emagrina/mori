//! Minimal, strictly bounded video container probing.
//!
//! Runs ONLY inside the sandboxed worker. It reads just enough of an MP4/MOV
//! `moov` box or the head of a WebM file to learn the codecs, dimensions and
//! duration, so Mori can refuse codecs the system player can't handle *before*
//! the webview ever sees the file. Any structural inconsistency (a box larger
//! than its parent, absurd counts, truncated elements) is reported as corrupt.

#[derive(Debug, Default, PartialEq)]
pub struct Probe {
    pub video: Option<String>,
    pub audio: Option<String>,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
}

impl Probe {
    /// Line-based wire format; values are restricted to a safe charset.
    pub fn encode(&self) -> Vec<u8> {
        format!(
            "video={}\naudio={}\nwidth={}\nheight={}\nduration_ms={}\n",
            self.video.as_deref().unwrap_or(""),
            self.audio.as_deref().unwrap_or(""),
            self.width,
            self.height,
            self.duration_ms
        )
        .into_bytes()
    }
}

/// Keep codec identifiers printable and short (they come from the file).
pub fn clean_codec(raw: &[u8]) -> String {
    raw.iter()
        .take(32)
        .map(|&b| if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-') { b as char } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

// ---------------------------------------------------------------- MP4 / MOV

const MAX_BOXES: usize = 4096;
const MAX_TRACKS: usize = 32;

fn be32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()))
}

fn be64(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..at + 8).map(|b| u64::from_be_bytes(b.try_into().unwrap()))
}

/// A child box: its four-character type and its payload.
type Mp4Box<'a> = ([u8; 4], &'a [u8]);

/// Split a box payload into child boxes. Strict: every child must fit exactly.
fn children(data: &[u8]) -> Result<Vec<Mp4Box<'_>>, ()> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() {
        if out.len() >= MAX_BOXES || data.len() - pos < 8 {
            return Err(());
        }
        let size = be32(data, pos).ok_or(())? as u64;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let (header, size) = match size {
            1 => (16u64, be64(data, pos + 8).ok_or(())?),
            0 => (8, (data.len() - pos) as u64), // "to end of parent"
            s => (8, s),
        };
        if size < header || size > (data.len() - pos) as u64 {
            return Err(());
        }
        out.push((kind, &data[pos + header as usize..pos + size as usize]));
        pos += size as usize;
    }
    Ok(out)
}

fn child<'a>(boxes: &[Mp4Box<'a>], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes.iter().find(|(k, _)| k == kind).map(|(_, b)| *b)
}

/// Probe the payload of an MP4/MOV `moov` box.
pub fn mp4_moov(moov: &[u8]) -> Result<Probe, ()> {
    let top = children(moov)?;
    let mut p = Probe::default();
    if let Some(mvhd) = child(&top, b"mvhd") {
        let (timescale, duration) = match mvhd.first() {
            Some(1) => (be32(mvhd, 20).ok_or(())? as u64, be64(mvhd, 24).ok_or(())?),
            Some(_) => (be32(mvhd, 12).ok_or(())? as u64, be32(mvhd, 16).ok_or(())? as u64),
            None => return Err(()),
        };
        if timescale > 0 && duration != u64::MAX && duration != u32::MAX as u64 {
            p.duration_ms = duration.saturating_mul(1000) / timescale;
        }
    }
    let traks: Vec<_> = top.iter().filter(|(k, _)| k == b"trak").collect();
    if traks.is_empty() || traks.len() > MAX_TRACKS {
        return Err(());
    }
    for (_, trak) in traks {
        let t = children(trak)?;
        let mdia = children(child(&t, b"mdia").ok_or(())?)?;
        let handler = child(&mdia, b"hdlr").and_then(|h| h.get(8..12)).ok_or(())?;
        let minf = children(child(&mdia, b"minf").ok_or(())?)?;
        let stbl = children(child(&minf, b"stbl").ok_or(())?)?;
        let stsd = child(&stbl, b"stsd").ok_or(())?;
        let count = be32(stsd, 4).ok_or(())?;
        if count == 0 || count > 16 {
            return Err(());
        }
        let entries = children(stsd.get(8..).ok_or(())?)?;
        let (codec, entry) = entries.first().ok_or(())?;
        match handler {
            b"vide" if p.video.is_none() => {
                p.video = Some(clean_codec(codec));
                // Visual sample entry: width/height at offsets 24/26 of the entry payload.
                p.width = entry.get(24..26).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as u32);
                p.height = entry.get(26..28).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as u32);
                // Phone videos are often stored landscape with a 90°/270° rotation in the
                // track header: report the size as displayed, like the player shows it.
                if child(&t, b"tkhd").is_some_and(quarter_turn) {
                    std::mem::swap(&mut p.width, &mut p.height);
                }
            }
            b"soun" if p.audio.is_none() => p.audio = Some(clean_codec(codec)),
            _ => {}
        }
    }
    Ok(p)
}

/// The track header's transformation matrix rotates by 90° or 270° (so the
/// displayed width and height are the stored ones swapped). The matrix is
/// `[a b u; c d v; x y w]` in 16.16 fixed point; a quarter turn has `a = d = 0`
/// and `b`, `c` of opposite signs.
fn quarter_turn(tkhd: &[u8]) -> bool {
    let at = match tkhd.first() {
        Some(1) => 52,
        Some(0) => 40,
        _ => return false,
    };
    let m = |i: usize| be32(tkhd, at + i * 4).map(|v| v as i32);
    match (m(0), m(1), m(3), m(4)) {
        (Some(0), Some(b), Some(c), Some(0)) => b != 0 && c != 0 && (b > 0) != (c > 0),
        _ => false,
    }
}

// --------------------------------------------------------------------- WebM

const EBML: u32 = 0x1A45DFA3;
const SEGMENT: u32 = 0x18538067;
const INFO: u32 = 0x1549A966;
const TRACKS: u32 = 0x1654AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const VIDEO: u32 = 0xE0;
const CLUSTER: u32 = 0x1F43B675;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const PIXEL_W: u32 = 0xB0;
const PIXEL_H: u32 = 0xBA;
const TIMECODE_SCALE: u32 = 0x2AD7B1;
const DURATION: u32 = 0x4489;

fn read_id(d: &[u8], pos: &mut usize) -> Option<u32> {
    let first = *d.get(*pos)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return None;
    }
    let bytes = d.get(*pos..*pos + len)?;
    *pos += len;
    Some(bytes.iter().fold(0u32, |a, &b| (a << 8) | b as u32))
}

/// Element size; `Ok(None)` = "unknown size".
fn read_size(d: &[u8], pos: &mut usize) -> Option<Option<u64>> {
    let first = *d.get(*pos)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let bytes = d.get(*pos..*pos + len)?;
    *pos += len;
    let mut v = (first & (0xFFu16 >> len) as u8) as u64;
    for &b in &bytes[1..] {
        v = (v << 8) | b as u64;
    }
    let all_ones = (1u64 << (7 * len)) - 1;
    Some(if v == all_ones { None } else { Some(v) })
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0u64, |a, &x| (a << 8) | x as u64)
}

#[derive(Default)]
struct Track {
    kind: u64,
    codec: Option<String>,
    w: u32,
    h: u32,
}

struct Walk<'a> {
    d: &'a [u8],
    budget: usize,
    tracks: Vec<Track>,
    found_tracks: bool,
    timecode_scale: u64,
    duration: f64,
}

impl Walk<'_> {
    fn walk(&mut self, mut pos: usize, end: usize, depth: u8, track: Option<usize>) -> Result<(), ()> {
        if depth > 8 {
            return Err(());
        }
        while pos < end {
            self.budget = self.budget.checked_sub(1).ok_or(())?;
            let Some(id) = read_id(self.d, &mut pos) else { return Ok(()) }; // window ended
            let Some(size) = read_size(self.d, &mut pos) else { return Ok(()) };
            let body_end = match size {
                Some(s) => pos.checked_add(s as usize).ok_or(())?,
                None => end,
            };
            let is_master = matches!(id, SEGMENT | INFO | TRACKS | TRACK_ENTRY | VIDEO);
            if body_end > end && !is_master {
                return Ok(()); // element continues past our window
            }
            let body_end = body_end.min(end);
            match id {
                CLUSTER => return Ok(()), // media data begins: headers are done
                SEGMENT | INFO | VIDEO => self.walk(pos, body_end, depth + 1, track)?,
                TRACKS => {
                    self.found_tracks = true;
                    self.walk(pos, body_end, depth + 1, None)?;
                }
                TRACK_ENTRY => {
                    if self.tracks.len() >= MAX_TRACKS {
                        return Err(());
                    }
                    self.tracks.push(Track::default());
                    let idx = self.tracks.len() - 1;
                    self.walk(pos, body_end, depth + 1, Some(idx))?;
                }
                _ => {
                    let body = &self.d[pos..body_end];
                    match (id, track) {
                        (TRACK_TYPE, Some(t)) => self.tracks[t].kind = uint(body),
                        (CODEC_ID, Some(t)) => self.tracks[t].codec = Some(clean_codec(body)),
                        (PIXEL_W, Some(t)) => self.tracks[t].w = uint(body).min(100_000) as u32,
                        (PIXEL_H, Some(t)) => self.tracks[t].h = uint(body).min(100_000) as u32,
                        (TIMECODE_SCALE, _) => self.timecode_scale = uint(body),
                        (DURATION, _) => {
                            self.duration = match body.len() {
                                4 => f32::from_be_bytes(body.try_into().unwrap()) as f64,
                                8 => f64::from_be_bytes(body.try_into().unwrap()),
                                _ => 0.0,
                            }
                        }
                        _ => {}
                    }
                }
            }
            pos = body_end;
        }
        Ok(())
    }
}

/// Probe the first bytes of a WebM file.
pub fn webm(head: &[u8]) -> Result<Probe, ()> {
    let mut pos = 0;
    if read_id(head, &mut pos) != Some(EBML) {
        return Err(());
    }
    let size = read_size(head, &mut pos).flatten().ok_or(())? as usize;
    let mut w = Walk {
        d: head,
        budget: 100_000,
        tracks: Vec::new(),
        found_tracks: false,
        timecode_scale: 1_000_000,
        duration: 0.0,
    };
    w.walk(pos.checked_add(size).ok_or(())?, head.len(), 0, None)?;
    if !w.found_tracks || w.tracks.is_empty() {
        return Err(());
    }
    let mut p = Probe::default();
    for t in &w.tracks {
        match t.kind {
            1 if p.video.is_none() => {
                p.video = t.codec.clone();
                p.width = t.w;
                p.height = t.h;
            }
            2 if p.audio.is_none() => p.audio = t.codec.clone(),
            _ => {}
        }
    }
    let ms = w.duration * w.timecode_scale as f64 / 1_000_000.0;
    if ms.is_finite() && ms > 0.0 && ms < 1e12 {
        p.duration_ms = ms as u64;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn moov(codec: &[u8; 4], handler: &[u8; 4]) -> Vec<u8> {
        moov_rotated(codec, handler, None)
    }

    /// A track header (version 0) with the given matrix entries a, b, c, d (16.16).
    fn tkhd(abcd: [i32; 4]) -> Vec<u8> {
        let mut t = vec![0u8; 84];
        for (i, v) in [(0usize, abcd[0]), (1, abcd[1]), (3, abcd[2]), (4, abcd[3])] {
            t[40 + i * 4..44 + i * 4].copy_from_slice(&v.to_be_bytes());
        }
        t[72..76].copy_from_slice(&0x4000_0000i32.to_be_bytes()); // w = 1.0 (2.30)
        t
    }

    fn moov_rotated(codec: &[u8; 4], handler: &[u8; 4], matrix: Option<[i32; 4]>) -> Vec<u8> {
        let mut entry = vec![0u8; 78];
        entry[24..26].copy_from_slice(&1920u16.to_be_bytes());
        entry[26..28].copy_from_slice(&1080u16.to_be_bytes());
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd.extend(bx(codec, &entry));
        let stbl = bx(b"stbl", &bx(b"stsd", &stsd));
        let minf = bx(b"minf", &stbl);
        let mut hdlr = vec![0u8; 8];
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 12]);
        let mdia = bx(b"mdia", &[bx(b"hdlr", &hdlr), minf].concat());
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&4500u32.to_be_bytes());
        let trak = match matrix {
            Some(m) => [bx(b"tkhd", &tkhd(m)), bx(b"mdia", &mdia[8..])].concat(),
            None => mdia,
        };
        [bx(b"mvhd", &mvhd), bx(b"trak", &trak)].concat()
    }

    #[test]
    fn rotated_phone_videos_report_their_displayed_size() {
        const ONE: i32 = 0x0001_0000;
        let dims = |m: Option<[i32; 4]>| {
            let p = mp4_moov(&moov_rotated(b"avc1", b"vide", m)).unwrap();
            (p.width, p.height)
        };
        assert_eq!(dims(None), (1920, 1080));
        assert_eq!(dims(Some([ONE, 0, 0, ONE])), (1920, 1080), "identity");
        assert_eq!(dims(Some([0, ONE, -ONE, 0])), (1080, 1920), "90°");
        assert_eq!(dims(Some([0, -ONE, ONE, 0])), (1080, 1920), "270°");
        assert_eq!(dims(Some([-ONE, 0, 0, -ONE])), (1920, 1080), "180°");
        // A nonsense matrix (a shear, not a rotation) changes nothing.
        assert_eq!(dims(Some([0, ONE, ONE, 0])), (1920, 1080));
        assert!(!quarter_turn(&[]) && !quarter_turn(&[0; 20]) && !quarter_turn(&[9; 90]));
    }

    #[test]
    fn reads_mp4_codec_dims_duration() {
        let p = mp4_moov(&moov(b"avc1", b"vide")).unwrap();
        assert_eq!(p.video.as_deref(), Some("avc1"));
        assert_eq!((p.width, p.height, p.duration_ms), (1920, 1080, 4500));
        assert_eq!(mp4_moov(&moov(b"mp4a", b"soun")).unwrap().video, None);
    }

    #[test]
    fn rejects_inconsistent_boxes() {
        let mut m = moov(b"avc1", b"vide");
        let i = m.windows(4).position(|w| w == b"stsd").unwrap();
        m[i - 4..i].copy_from_slice(&0x7fff_ffffu32.to_be_bytes());
        assert!(mp4_moov(&m).is_err());
        assert!(mp4_moov(&[0, 0, 0, 4, b'f', b'r', b'e', b'e']).is_err());
        assert!(mp4_moov(b"").is_err());
        // Deeply hostile input never panics.
        for n in 0..300 {
            let junk: Vec<u8> = (0..n).map(|i| (i * 37 % 251) as u8).collect();
            let _ = mp4_moov(&junk);
            let _ = webm(&junk);
        }
    }

    #[test]
    fn sanitises_codec_names() {
        assert_eq!(clean_codec(b"hvc1"), "hvc1");
        assert_eq!(clean_codec(b"V_VP9"), "V_VP9");
        assert_eq!(clean_codec(b"<b>\n"), "b");
    }

    /// Real files written by ffmpeg with display-rotation metadata (64×36 stored).
    #[test]
    fn real_rotated_mp4_files() {
        for name in ["rotated-90.mp4", "rotated-270.mp4"] {
            let data = std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            let top = children(&data).unwrap();
            let p = mp4_moov(child(&top, b"moov").unwrap()).unwrap();
            assert_eq!((p.width, p.height), (36, 64), "{name} is displayed portrait");
        }
    }
}
