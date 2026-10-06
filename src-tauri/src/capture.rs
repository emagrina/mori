//! Screenshots and screen recordings, recognised with local heuristics only:
//!
//! - the name the system gave the file ("Screenshot 2024-…", "Captura de
//!   pantalla…", "Screen Recording…", Android "Screenshot_2024…", …);
//! - on macOS, the `com.apple.metadata:kMDItemIsScreenCapture` extended
//!   attribute that the system screenshot tool sets (read with
//!   `XATTR_NOFOLLOW`; nothing is opened).
//!
//! It is a guess, so the user can correct it ("Not a Screenshot" / "Mark as
//! Screenshot"); corrections are stored like private folders (per volume)
//! and applied when the index is published.

use crate::index::Kind;
use std::path::Path;

pub const NONE: u8 = 0;
pub const SCREENSHOT: u8 = 1;
pub const RECORDING: u8 = 2;

const SCREENSHOT_NAMES: &[&str] = &[
    "screenshot",
    "screen shot",
    "captura de pantalla",
    "captura de tela",
    "capture d’écran",
    "capture d'écran",
    "bildschirmfoto",
    "schermata",
    "schermafbeelding",
    "skärmavbild",
    "zrzut ekranu",
    "снимок экрана",
    "スクリーンショット",
    "屏幕截图",
    "截屏",
    "螢幕截圖",
    "스크린샷",
];

const RECORDING_NAMES: &[&str] = &[
    "screen recording",
    "screenrecording",
    "screen_recording",
    "screenrecorder",
    "grabación de pantalla",
    "gravação de tela",
    "enregistrement de l’écran",
    "enregistrement de l'écran",
    "bildschirmaufnahme",
    "registrazione schermo",
    "schermopname",
    "skärminspelning",
    "nagranie ekranu",
    "запись экрана",
    "画面収録",
    "屏幕录制",
    "螢幕錄影",
    "화면 기록",
];

/// What the file name alone suggests.
pub fn by_name(name: &str, kind: Kind) -> u8 {
    let n = name.to_lowercase();
    match kind {
        Kind::Photo if SCREENSHOT_NAMES.iter().any(|p| n.starts_with(p)) || n.starts_with("scr_") => SCREENSHOT,
        Kind::Video if RECORDING_NAMES.iter().any(|p| n.starts_with(p)) => RECORDING,
        // Some recorders name files "Screenshot…" too, and vice versa.
        Kind::Video if SCREENSHOT_NAMES.iter().any(|p| n.starts_with(p)) => RECORDING,
        _ => NONE,
    }
}

/// Name first, then (macOS) the screen-capture attribute.
pub fn detect(path: &Path, name: &str, kind: Kind) -> u8 {
    if !matches!(kind, Kind::Photo | Kind::Video) {
        return NONE;
    }
    let guess = by_name(name, kind);
    if guess != NONE {
        return guess;
    }
    if has_capture_xattr(path) {
        return if kind == Kind::Video { RECORDING } else { SCREENSHOT };
    }
    NONE
}

#[cfg(target_os = "macos")]
fn has_capture_xattr(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // Size query only: the value itself is not needed.
    let n = unsafe {
        libc::getxattr(
            p.as_ptr(),
            c"com.apple.metadata:kMDItemIsScreenCapture".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    n > 0
}

#[cfg(not(target_os = "macos"))]
fn has_capture_xattr(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(by_name("Screenshot 2024-05-01 at 10.00.00.png", Kind::Photo), SCREENSHOT);
        assert_eq!(by_name("Captura de pantalla 2024-05-01 a las 10.00.00.png", Kind::Photo), SCREENSHOT);
        assert_eq!(by_name("Screenshot_20240501-100000_Chrome.jpg", Kind::Photo), SCREENSHOT);
        assert_eq!(by_name("Screen Recording 2024-05-01 at 10.00.00.mov", Kind::Video), RECORDING);
        assert_eq!(by_name("Grabación de pantalla 2024-05-01.mov", Kind::Video), RECORDING);
        assert_eq!(by_name("IMG_1234.PNG", Kind::Photo), NONE);
        assert_eq!(by_name("my screenshot ideas.png", Kind::Photo), NONE, "only the start of the name counts");
        assert_eq!(by_name("Screenshot.pdf", Kind::Document), NONE);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn capture_attribute() {
        use std::os::unix::ffi::OsStrExt;
        let d = std::env::temp_dir().join(format!("mori-capture-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("IMG_0001.png");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(detect(&f, "IMG_0001.png", Kind::Photo), NONE);
        let p = std::ffi::CString::new(f.as_os_str().as_bytes()).unwrap();
        let v = b"bplist00\x09\x08\x00\x00\x00\x00\x00\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x09";
        let rc = unsafe {
            libc::setxattr(
                p.as_ptr(),
                c"com.apple.metadata:kMDItemIsScreenCapture".as_ptr(),
                v.as_ptr() as *const _,
                v.len(),
                0,
                0,
            )
        };
        assert_eq!(rc, 0);
        assert_eq!(detect(&f, "IMG_0001.png", Kind::Photo), SCREENSHOT);
        // A link to it is not inspected through the link.
        let l = d.join("link.png");
        std::os::unix::fs::symlink(&f, &l).unwrap();
        assert_eq!(detect(&l, "link.png", Kind::Photo), NONE);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
