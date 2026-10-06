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
        b"\x00\x00\x00\x18ftypheic",
        b"",
    ] {
        let (code, out, _) = run(&["thumb", "256"], input);
        assert_eq!(code, Some(2), "{:?}", String::from_utf8_lossy(input));
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
