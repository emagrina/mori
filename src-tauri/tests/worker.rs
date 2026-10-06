//! Runs the real Mori binary in worker mode against hostile inputs.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run(args: &[&str], input: &[u8]) -> (Option<i32>, Vec<u8>, Duration) {
    let start = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_mori"))
        .arg("--mori-worker")
        .args(args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let w = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    let _ = w.join();
    (out.status.code(), out.stdout, start.elapsed())
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).unwrap();
    buf
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A tiny PNG whose header claims enormous dimensions (decompression bomb).
fn bomb_png(w: u32, h: u32) -> Vec<u8> {
    let mut p = png(8, 8);
    // IHDR data starts at byte 16 (8 signature + 4 length + 4 type).
    p[16..20].copy_from_slice(&w.to_be_bytes());
    p[20..24].copy_from_slice(&h.to_be_bytes());
    let crc = crc32(&p[12..29]);
    p[29..33].copy_from_slice(&crc.to_be_bytes());
    p
}

#[cfg(target_os = "macos")]
#[test]
fn sandbox_denies_filesystem_network_and_processes() {
    let (code, out, _) = run(&["selftest"], b"");
    assert_eq!(code, Some(0));
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.trim(), "fs_denied=true write_denied=true net_denied=true spawn_denied=true");
}

/// HEIC jobs use their own profile (Apple's decoder service allowed); user
/// files, writes, the network and new processes must still be denied.
#[cfg(target_os = "macos")]
#[test]
fn heif_sandbox_still_denies_files_network_and_processes() {
    let (code, out, _) = run(&["selftest", "heif"], b"");
    assert_eq!(code, Some(0));
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.trim(), "fs_denied=true write_denied=true net_denied=true spawn_denied=true");
}

/// PDF jobs may read system fonts, and nothing else of the user's.
#[cfg(target_os = "macos")]
#[test]
fn pdf_sandbox_still_denies_files_network_and_processes() {
    let (code, out, _) = run(&["selftest", "pdf"], b"");
    assert_eq!(code, Some(0));
    let out = String::from_utf8(out).unwrap();
    assert_eq!(out.trim(), "fs_denied=true write_denied=true net_denied=true spawn_denied=true");
}

/// Fingerprints: fixed-size grayscale miniatures plus the source size.
#[test]
fn fingerprints_stills_and_rejects_junk() {
    let (code, out, _) = run(&["fingerprint", "64"], &png(1200, 800));
    assert_eq!(code, Some(0));
    assert_eq!(&out[0..4], b"MORI");
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 1200);
    assert_eq!(out[12], 5, "raw miniatures");
    assert_eq!(out.len(), 16 + 64 * 64 * 2);
    for junk in [&b"\0\0\0\x18ftypheic\0\0\0\0garbage"[..], b"<svg onload=alert(1)>", b""] {
        let (code, out, _) = run(&["fingerprint", "64"], junk);
        assert_ne!(code, Some(0));
        assert!(out.is_empty());
    }
}

#[cfg(target_os = "macos")]
#[test]
fn fingerprints_heic_inside_the_sandbox() {
    let heic = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/tiny.heic")).unwrap();
    let (code, out, _) = run(&["fingerprint", "64", "heif"], &heic);
    assert_eq!(code, Some(0));
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 16);
    assert_eq!(out.len(), 16 + 64 * 64 * 2);
}

#[test]
fn reencodes_a_valid_image() {
    let (code, out, _) = run(&["thumb", "256"], &png(1200, 800));
    assert_eq!(code, Some(0));
    assert_eq!(&out[0..4], b"MORI");
    assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 1200);
    assert_eq!(out[12], 1, "JPEG output");
    let thumb = image::load_from_memory(&out[16..]).unwrap();
    assert_eq!((thumb.width(), thumb.height()), (256, 171));
}

#[test]
fn rejects_decompression_bombs_quickly() {
    let (code, out, took) = run(&["preview", "2048"], &bomb_png(60_000, 60_000));
    assert_eq!(code, Some(3), "limits exceeded");
    assert!(out.is_empty());
    assert!(took < Duration::from_secs(3), "{took:?}");
    let (code, ..) = run(&["preview", "2048"], &bomb_png(19_000, 19_000));
    assert_eq!(code, Some(3), "361 MP is over the pixel budget");
}

#[test]
fn rejects_active_and_unknown_content() {
    for input in [
        &b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>"[..],
        b"<html><script>fetch('https://evil')</script></html>",
        b"%PDF-1.7 /JavaScript",
        b"MZ\x90\x00\x03",
        b"",
    ] {
        let (code, out, _) = run(&["thumb", "256"], input);
        assert_eq!(code, Some(2), "{:?}", String::from_utf8_lossy(input));
        assert!(out.is_empty());
    }
    // A truncated HEIC: unsupported off macOS, a decode failure on macOS.
    for args in [&["thumb", "256"][..], &["thumb", "256", "heif"]] {
        let (code, out, _) = run(args, b"\x00\x00\x00\x18ftypheic");
        assert_ne!(code, Some(0));
        assert!(out.is_empty());
    }
}

#[test]
fn survives_truncated_and_corrupt_images() {
    let mut jpeg = Vec::new();
    image::RgbImage::new(640, 480).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
    for bad in [jpeg[..jpeg.len() / 3].to_vec(), {
        let mut p = png(64, 64);
        let n = p.len();
        p[40..n - 12].iter_mut().for_each(|b| *b ^= 0x5A);
        p
    }] {
        let (code, out, _) = run(&["preview", "1024"], &bad);
        // Either a clean refusal or a (partially) decoded, freshly encoded image — never a crash.
        assert!(matches!(code, Some(0) | Some(4)), "{code:?}");
        if code == Some(0) {
            assert_eq!(&out[0..4], b"MORI");
        }
    }
}

#[test]
fn frame_op_only_accepts_png() {
    let mut jpeg = Vec::new();
    image::RgbImage::new(32, 32).write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
    assert_eq!(run(&["frame", "256"], &jpeg).0, Some(2));
    assert_eq!(run(&["frame", "256"], &png(320, 180)).0, Some(0));
}

#[test]
fn reencodes_animated_gif() {
    use image::codecs::gif::GifEncoder;
    let mut gif = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut gif);
        for i in 0..3u8 {
            let f = image::RgbaImage::from_pixel(1000, 500, image::Rgba([i * 80, 0, 0, 255]));
            enc.encode_frame(image::Frame::new(f)).unwrap();
        }
    }
    let (code, out, _) = run(&["preview", "3072"], &gif);
    assert_eq!(code, Some(0));
    assert_eq!(out[12], 3, "GIF output");
    let frames = image::AnimationDecoder::into_frames(
        image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&out[16..])).unwrap(),
    )
    .count();
    assert_eq!(frames, 3);
}

#[test]
fn refuses_bad_arguments() {
    assert_eq!(run(&["thumb", "999999"], &png(8, 8)).0, Some(6));
    assert_eq!(run(&["format-c", "256"], &png(8, 8)).0, Some(6));
}

/// Same synthetic one-page PDF as the unit tests (see src/pdf.rs).
#[cfg(target_os = "macos")]
fn pdf(extra: &str) -> Vec<u8> {
    let objs = [
        format!("<< /Type /Catalog /Pages 2 0 R {extra} >>"),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        "<< /Length 34 >>\nstream\nBT /F1 40 Tf 10 30 Td (Mori) Tj ET\nendstream".to_string(),
        // A standard font the document references without embedding it.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    out
}

/// PDFs are read and rasterised inside the strict sandbox; active content is
/// only reported.
#[cfg(target_os = "macos")]
#[test]
fn pdf_info_and_pages_inside_the_sandbox() {
    let (code, out, _) = run(&["pdfinfo", "64"], &pdf("/OpenAction << /S /JavaScript /JS (app.alert(1)) >>"));
    assert_eq!(code, Some(0));
    let text = String::from_utf8_lossy(&out[16..]).into_owned();
    assert!(text.starts_with("pages=1"), "{text}");
    assert!(text.contains("openaction=true"));

    let mut input = 1u32.to_le_bytes().to_vec();
    input.extend(pdf(""));
    let (code, out, _) = run(&["pdfpage", "300"], &input);
    assert_eq!(code, Some(0));
    assert_eq!(&out[..4], b"MORI");
    assert!(u32::from_le_bytes(out[4..8].try_into().unwrap()) > 0);
    // The text was drawn: the PDF profile can read the system fonts.
    let img = image::load_from_memory(&out[16..]).unwrap().to_luma8();
    assert!(img.pixels().filter(|p| p[0] < 80).count() > 200, "standard-font text missing");

    // A missing page, junk and an empty input all fail cleanly.
    let mut input = 7u32.to_le_bytes().to_vec();
    input.extend(pdf(""));
    assert_ne!(run(&["pdfpage", "300"], &input).0, Some(0));
    assert_ne!(run(&["pdfpage", "300"], b"\x01\0\0\0%PDF-1.4 junk").0, Some(0));
    assert_ne!(run(&["pdfinfo", "64"], b"").0, Some(0));
}

/// A JPEG with a comment and a minimal EXIF block (Make = "SynthCam").
fn jpeg_with_metadata() -> Vec<u8> {
    let img = image::RgbImage::from_fn(16, 8, |x, y| image::Rgb([x as u8 * 15, y as u8 * 30, 60]));
    let mut plain = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Jpeg).unwrap();
    let mut tiff = b"MM\0*\0\0\0\x08\0\x01".to_vec();
    tiff.extend([0x01, 0x0F, 0, 2, 0, 0, 0, 9, 0, 0, 0, 26, 0, 0, 0, 0]);
    tiff.extend(b"SynthCam\0");
    let seg = |m: u8, p: &[u8]| {
        let mut s = vec![0xFF, m];
        s.extend(((p.len() + 2) as u16).to_be_bytes());
        s.extend(p);
        s
    };
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend(tiff);
    let mut out = vec![0xFF, 0xD8];
    out.extend(seg(0xE1, &exif));
    out.extend(seg(0xFE, b"secret note"));
    out.extend(&plain[2..]);
    out
}

#[test]
fn metadata_is_read_in_the_worker_and_one_bad_item_costs_only_itself() {
    let mut input = b"FILE".to_vec();
    input.extend(jpeg_with_metadata());
    let (code, out, _) = run(&["meta", "64"], &input);
    assert_eq!(code, Some(0));
    let json = String::from_utf8(out[16..].to_vec()).unwrap();
    assert!(json.contains("SynthCam") && json.contains("secret note"), "{json}");

    // Batch: good item, garbage item, truncated item.
    let mut batch = Vec::new();
    for item in [input.clone(), b"FILE\xFF\xD8\xFF\xE1\xFF\xFFgarbage".to_vec(), input[..60].to_vec()] {
        batch.extend((item.len() as u32).to_le_bytes());
        batch.extend(item);
    }
    let (code, out, _) = run(&["metabatch", "64"], &batch);
    assert_eq!(code, Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out[16..]).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 3);
    assert!(v[0]["fields"].as_array().unwrap().len() >= 2);

    // A batch whose framing lies is refused, not misread.
    assert_ne!(run(&["metabatch", "64"], b"\xFF\xFF\xFF\x00FILE").0, Some(0));
}

#[test]
fn sanitize_strips_metadata_in_the_worker() {
    let (code, out, _) = run(&["sanitize", "64"], &jpeg_with_metadata());
    assert_eq!(code, Some(0));
    let clean = &out[16..];
    assert!(clean.starts_with(&[0xFF, 0xD8]));
    assert!(!clean.windows(8).any(|w| w == b"SynthCam") && !clean.windows(6).any(|w| w == b"secret"));
    assert!(image::load_from_memory(clean).is_ok());
    // Unsupported and damaged input.
    assert_ne!(run(&["sanitize", "64"], b"GIF89a....").0, Some(0));
    assert_ne!(run(&["sanitize", "64"], &jpeg_with_metadata()[..100]).0, Some(0));
}
