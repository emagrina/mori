//! Privacy regression test: a Temporary Session must not leave session-derived
//! records in anything Mori writes.
//!
//! Runs the real (debug) Mori twice with an isolated HOME and TMPDIR:
//! 1. a baseline launch that quits after startup;
//! 2. a Private Inspection over a fixture folder that browses, searches,
//!    makes thumbnails and previews, renders a PDF page, reads metadata,
//!    computes checksums, lists an archive, tries writes the session must
//!    refuse, ends the session and quits.
//!
//! Then every file under the isolated HOME and TMPDIR is compared with the
//! baseline, and every file is searched for the fixture names and the search
//! term. Mori's WebKit folders in the real home (outside HOME's reach) must
//! not change either.
//!
//! macOS only (it opens Mori's window for a few seconds). Set
//! `MORI_SKIP_GUI_TESTS=1` to skip.

#![cfg(target_os = "macos")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

type Snap = BTreeMap<PathBuf, (u64, std::time::SystemTime)>;

fn snapshot(root: &Path, skip: &Path) -> Snap {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file() && !e.path().starts_with(skip))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path().to_path_buf(), (m.len(), m.modified().ok()?)))
        })
        .collect()
}

fn run(home: &Path, tmp: &Path, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mori"));
    cmd.env("HOME", home).env("TMPDIR", tmp);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out_file = tmp.join("..").join("mori-test-stderr.txt");
    let f = fs::File::create(&out_file).unwrap();
    let mut child = cmd.stderr(f).stdout(std::process::Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(120) {
            let _ = child.kill();
            panic!("Mori didn't finish:\n{}", fs::read_to_string(&out_file).unwrap_or_default());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    fs::read_to_string(&out_file).unwrap_or_default()
}

fn zip_with(name: &str, data: &[u8]) -> Vec<u8> {
    // Minimal stored ZIP (one entry).
    let crc = {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
        }
        !c
    };
    let mut local = b"PK\x03\x04\x14\0\0\0\0\0\0\0\0\0".to_vec();
    local.extend(crc.to_le_bytes());
    local.extend((data.len() as u32).to_le_bytes());
    local.extend((data.len() as u32).to_le_bytes());
    local.extend((name.len() as u16).to_le_bytes());
    local.extend([0, 0]);
    local.extend(name.as_bytes());
    local.extend(data);
    let mut cd = b"PK\x01\x02\x14\0\x14\0\0\0\0\0\0\0\0\0".to_vec();
    cd.extend(crc.to_le_bytes());
    cd.extend((data.len() as u32).to_le_bytes());
    cd.extend((data.len() as u32).to_le_bytes());
    cd.extend((name.len() as u16).to_le_bytes());
    cd.extend([0u8; 12]);
    cd.extend(0u32.to_le_bytes());
    cd.extend(name.as_bytes());
    let mut out = local.clone();
    let off = out.len() as u32;
    out.extend(&cd);
    out.extend(b"PK\x05\x06\0\0\0\0\x01\0\x01\0");
    out.extend((cd.len() as u32).to_le_bytes());
    out.extend(off.to_le_bytes());
    out.extend([0, 0]);
    out
}

#[test]
fn temporary_session_leaves_no_session_records() {
    if std::env::var_os("MORI_SKIP_GUI_TESTS").is_some() {
        eprintln!("skipped (MORI_SKIP_GUI_TESTS)");
        return;
    }
    let base = fs::canonicalize(std::env::temp_dir()).unwrap().join(format!("mori-ephemeral-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let home = base.join("home");
    let tmp = base.join("tmp");
    let fixtures = base.join("Fixtures Drive");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    fs::create_dir_all(fixtures.join("Documents")).unwrap();
    let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 90]));
    img.save(fixtures.join("secret-passport.jpg")).unwrap();
    img.save(fixtures.join("family-photo.png")).unwrap();
    let pdf = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>\nendobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n183\n%%EOF\n";
    fs::write(fixtures.join("Documents/bank-statement.pdf"), pdf).unwrap();
    fs::write(fixtures.join("Documents/passport-scans.zip"), zip_with("passport-scan.txt", b"scan")).unwrap();

    // Mori-owned folders in the real home that an isolated HOME can't redirect.
    let real_home = PathBuf::from(std::env::var("HOME").unwrap());
    let outside: Vec<PathBuf> = ["Library/WebKit/app.mori.viewer", "Library/HTTPStorages/app.mori.viewer"]
        .iter()
        .map(|p| real_home.join(p))
        .collect();
    let snap_outside = || outside.iter().map(|p| snapshot(p, Path::new("/nonexistent"))).collect::<Vec<_>>();

    let log0 = run(&home, &tmp, &[("MORI_DEBUG_QUIT", "1")]);
    assert!(log0.contains("baseline done"), "{log0}");
    let before = snapshot(&base, &fixtures);
    let before_tmp = snapshot(&tmp, &fixtures);
    let outside_before = snap_outside();

    let log = run(&home, &tmp, &[("MORI_DEBUG_EPHEMERAL", fixtures.to_str().unwrap())]);
    assert!(log.contains("DEBUG ephemeral done"), "{log}");
    // The session really exercised the read paths.
    assert!(log.contains("search-hits=2"), "{log}");
    assert!(
        log.contains("private start: safe=true read_only=true temporary=true"),
        "Private Inspection starts read-only with decoding off: {log}"
    );
    assert!(log.contains("thumb=200 preview=200 iso=200"), "{log}");
    assert!(log.contains("metadata=true"), "{log}");
    assert!(log.contains("pdf-info=true pdf-page=200"), "{log}");
    assert!(log.contains("archive=true"), "{log}");
    assert!(!log.contains("checksum=false"), "{log}");
    assert!(log.contains("refused: favorite=true snapshot=true tag=true open=true"), "{log}");
    assert!(log.contains("read-only rename refused=true"), "{log}");
    assert!(log.contains("after end: temporary=false root=false"), "{log}");
    // The saved Read-only setting was never touched by the session.
    let settings =
        fs::read_to_string(home.join("Library/Application Support/app.mori.viewer/settings.json")).unwrap_or_default();
    assert!(!settings.contains("\"read_only\": true"), "{settings}");

    let after = snapshot(&base, &fixtures);
    let changed: Vec<&PathBuf> = after
        .iter()
        .filter(|(p, v)| before.get(*p) != Some(v))
        .map(|(p, _)| p)
        .filter(|p| !p.ends_with("mori-test-stderr.txt"))
        .collect();
    assert!(changed.is_empty(), "files written or changed during the temporary session: {changed:#?}");
    assert_eq!(snapshot(&tmp, &fixtures), before_tmp, "nothing left in TMPDIR");
    assert_eq!(snap_outside(), outside_before, "Mori's WebKit folders in the real home changed");

    // Nothing Mori keeps mentions what was inspected.
    for p in after.keys().filter(|p| !p.ends_with("mori-test-stderr.txt")) {
        let bytes = fs::read(p).unwrap_or_default();
        for needle in ["secret-passport", "passport", "bank-statement", "Fixtures Drive", "family-photo"] {
            assert!(!bytes.windows(needle.len()).any(|w| w == needle.as_bytes()), "{p:?} contains “{needle}”");
        }
    }
    let _ = fs::remove_dir_all(&base);
}
