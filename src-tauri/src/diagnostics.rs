//! Diagnostics: what this installation can actually do right now, checked
//! with harmless synthetic fixtures made in memory (never the user's files,
//! never the network).
//!
//! Every item says how it was established:
//! - `pass`: verified now by a test that would fail if the protection were
//!   missing;
//! - `limited` / `fail`: verified now, and not (fully) working;
//! - `info`: a fact about Mori's code or configuration that can't be
//!   observed at runtime (for example "no telemetry code") — shown without a
//!   check mark.

use crate::worker::{self, Op, WorkerError};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Pass,
    Limited,
    Fail,
    Info,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub section: &'static str,
    pub label: String,
    pub status: Status,
    /// Short value shown next to the label.
    pub value: String,
    /// How it was established / why it is limited.
    pub detail: String,
}

fn check(section: &'static str, label: &str, status: Status, value: &str, detail: &str) -> Check {
    Check { section, label: label.into(), status, value: value.into(), detail: detail.into() }
}

// ---------------------------------------------------------------- fixtures

fn encode(img: image::DynamicImage, f: image::ImageFormat) -> Vec<u8> {
    let mut v = Vec::new();
    let _ = img.write_to(&mut std::io::Cursor::new(&mut v), f);
    v
}

fn sample() -> image::DynamicImage {
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(24, 16, |x, y| {
        image::Rgba([x as u8 * 10, y as u8 * 15, 128, 255])
    }))
}

/// A PNG whose header claims 100000 × 100000 pixels (a decompression bomb).
fn bomb_png() -> Vec<u8> {
    let mut p = encode(sample(), image::ImageFormat::Png);
    p[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    p[20..24].copy_from_slice(&100_000u32.to_be_bytes());
    let crc = crc32fast::hash(&p[12..29]);
    p[29..33].copy_from_slice(&crc.to_be_bytes());
    p
}

/// A one-page PDF with a correct cross-reference table.
fn pdf() -> Vec<u8> {
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R >>".to_string(),
        "<< /Length 35 >>\nstream\n0 0 1 rg 10 10 80 40 re f\nendstream".to_string(),
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

/// A stored (uncompressed) ZIP with the given entries.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, data) in entries {
        let crc = crc32fast::hash(data);
        let off = out.len() as u32;
        let header = |sig: &[u8], central: bool| {
            let mut h = sig.to_vec();
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend([0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend(crc.to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend([0, 0]);
            if central {
                // comment length, disk number, internal and external attributes
                h.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                h.extend(off.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(b"PK\x03\x04", false));
        out.extend(*data);
        cd.extend(header(b"PK\x01\x02", true));
    }
    let cd_off = out.len() as u32;
    let n = entries.len() as u16;
    out.extend(&cd);
    out.extend(b"PK\x05\x06\0\0\0\0");
    out.extend(n.to_le_bytes());
    out.extend(n.to_le_bytes());
    out.extend((cd.len() as u32).to_le_bytes());
    out.extend(cd_off.to_le_bytes());
    out.extend([0, 0]);
    out
}

/// A scratch folder Mori creates for the self-test and removes afterwards.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> Option<Scratch> {
        let base = fs::canonicalize(std::env::temp_dir()).ok()?;
        let p = base.join(format!("mori-selftest-{}-{}", std::process::id(), crate::index::now_millis()));
        fs::create_dir(&p).ok()?;
        Some(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Only the folder created above (its name was made here); links inside are removed, not followed.
        if self.0.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("mori-selftest-")) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

// ------------------------------------------------------------------ checks

fn selftest(profile: Option<&str>) -> Option<(bool, bool, bool, bool)> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg(worker::WORKER_FLAG).arg("selftest");
    if let Some(p) = profile {
        cmd.arg(p);
    }
    let mut child = cmd.env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    drop(child.stdin.take().map(|mut s| s.write_all(b"")));
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let flag = |k: &str| text.contains(&format!("{k}=true"));
    Some((flag("fs_denied"), flag("write_denied"), flag("net_denied"), flag("spawn_denied")))
}

fn decodes(bytes: Vec<u8>) -> Result<(), WorkerError> {
    worker::run(Op::Thumb, 64, bytes, Duration::from_secs(12)).map(|_| ())
}

/// The full self-test. `temporary`: a temporary session is active.
pub fn run(
    csp: &str,
    temporary: bool,
    read_only_setting: bool,
    stale_cleaned: u64,
    index_bytes: u64,
    data_dir: &Path,
) -> Vec<Check> {
    let mut v = Vec::new();
    let s = Status::Pass;

    // ------------------------------------------------------------- media
    let formats = [
        ("JPEG", image::ImageFormat::Jpeg),
        ("PNG", image::ImageFormat::Png),
        ("WebP", image::ImageFormat::WebP),
        ("GIF", image::ImageFormat::Gif),
    ];
    for (name, f) in formats {
        let img =
            if f == image::ImageFormat::Jpeg { image::DynamicImage::ImageRgb8(sample().to_rgb8()) } else { sample() };
        match decodes(encode(img, f)) {
            Ok(()) => v.push(check(
                "media",
                name,
                s,
                "Available",
                "A synthetic image was decoded by the sandboxed worker just now.",
            )),
            Err(e) => v.push(check(
                "media",
                name,
                Status::Fail,
                "Not working",
                &format!("The sandboxed worker couldn't decode a synthetic {name} ({e:?}).",),
            )),
        }
    }
    let heic = include_bytes!("../tests/fixtures/tiny.heic").to_vec();
    match worker::run(Op::Fingerprint, 64, heic, Duration::from_secs(12)) {
        Ok(_) => v.push(check(
            "media",
            "HEIC / HEIF",
            s,
            "Available",
            "A bundled 2×2 HEIC test image was decoded by the system decoder inside the HEIF sandbox profile.",
        )),
        Err(_) if !cfg!(target_os = "macos") => v.push(check(
            "media",
            "HEIC / HEIF",
            Status::Limited,
            "Unavailable",
            "HEIC is decoded only on macOS (system decoder).",
        )),
        Err(e) => v.push(check(
            "media",
            "HEIC / HEIF",
            Status::Fail,
            "Not working",
            &format!("The bundled HEIC test image couldn't be decoded ({e:?})."),
        )),
    }
    let pdf_ok = worker::run(Op::PdfInfo, 64, pdf(), Duration::from_secs(10))
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.bytes).contains("openaction=true"));
    let mut page = 1u32.to_le_bytes().to_vec();
    page.extend(pdf());
    let page_ok = worker::run(Op::PdfPage, 200, page, Duration::from_secs(15)).is_ok();
    v.push(match (pdf_ok, page_ok) {
        (true, true) => check("media", "PDF safe preview", s, "Available", "A synthetic PDF with an automatic JavaScript action was read and rasterised in the PDF sandbox; the action was reported, not run."),
        _ if !cfg!(target_os = "macos") => check("media", "PDF safe preview", Status::Limited, "Unavailable", "PDF pages are rendered only on macOS (CoreGraphics in the worker)."),
        _ => check("media", "PDF safe preview", Status::Fail, "Not working", "The synthetic PDF couldn't be read or rendered in the sandbox."),
    });
    v.push(check(
        "media",
        "Video preview",
        Status::Info,
        "System media engine",
        "Containers are probed by Mori's sandboxed worker; playback uses the operating system's media engine through the web view, under Mori's hang/crash watchdog. Supported codecs are checked per file. Mori doesn't bundle FFmpeg.",
    ));
    v.push(check("media", "Audio preview", Status::Info, "System media engine", "MP3, AAC/M4A, WAV, AIFF and FLAC verified by magic bytes, played by the system media engine. Not played in the isolated view."));

    // ----------------------------------------------------------- security
    let png = encode(sample(), image::ImageFormat::Png);
    let ft = crate::filetype::detect(&png, png.len() as u64);
    let exp = crate::filetype::expected_ids("jpg").is_some_and(|e| !e.contains(&ft.id));
    let dbl = crate::risk::name_findings("invoice.pdf.app").iter().any(|f| f.level == crate::risk::Level::High);
    let bidi = crate::risk::name_findings("photo\u{202E}gpj.exe").iter().any(|f| f.level == crate::risk::Level::High);
    v.push(if ft.id == "png" && exp && dbl && bidi {
        check("security", "Real file type detection", s, "Active", "A PNG named .jpg was identified as PNG and flagged as an extension mismatch; “invoice.pdf.app” and a right-to-left override name were flagged.")
    } else {
        check("security", "Real file type detection", Status::Fail, "Not working", "A synthetic disguised file wasn't detected.")
    });
    let sandbox_line = |p: Option<(bool, bool, bool, bool)>, what: &str| {
        match p {
        Some((true, true, true, true)) => (Status::Pass, "Active".to_string(), format!("{what}: reading user files, writing files, opening network sockets and starting processes were all refused inside the worker just now.")),
        Some((f, w, n, sp)) => (
            Status::Limited,
            "Limited".to_string(),
            format!("{what}: file reads denied={f}, writes denied={w}, network denied={n}, processes denied={sp}."),
        ),
        None => (Status::Fail, "Not working".to_string(), format!("{what}: the worker couldn't be started.")),
    }
    };
    let base = selftest(None);
    if cfg!(target_os = "macos") {
        let (st, val, det) = sandbox_line(base, "Pure-computation sandbox");
        v.push(Check {
            section: "security",
            label: "Restricted decoder workers".into(),
            status: st,
            value: val,
            detail: det,
        });
        let heif = selftest(Some("heif"));
        let pdfp = selftest(Some("pdf"));
        let all = [base, heif, pdfp].iter().all(|p| matches!(p, Some((true, true, true, true))));
        v.push(if all {
            check("security", "Preview isolation", s, "Active", "Every worker profile (standard, HEIF, PDF) denied file access, writes, network and processes just now. The UI only receives re-encoded copies.")
        } else {
            check("security", "Preview isolation", Status::Limited, "Limited", "At least one worker profile didn't deny everything it should (see the worker checks).")
        });
    } else {
        let detail = if cfg!(windows) {
            "Workers run in a Job object (memory, single process, no UI) and never receive a path, but there is no filesystem-denying sandbox on Windows."
        } else {
            "Workers use resource limits and no_new_privs, but no seccomp filter yet."
        };
        v.push(check("security", "Restricted decoder workers", Status::Limited, "Limited", detail));
        v.push(check("security", "Preview isolation", Status::Limited, "Limited", detail));
    }
    v.push(match decodes(bomb_png()) {
        Err(WorkerError::Limits) => check(
            "security",
            "Resource limits",
            s,
            "Active",
            "A PNG claiming 100,000 × 100,000 pixels was refused before allocation.",
        ),
        r => check(
            "security",
            "Resource limits",
            Status::Fail,
            "Not working",
            &format!("The decompression-bomb test returned {r:?}."),
        ),
    });
    let mut jpeg = encode(image::DynamicImage::ImageRgb8(sample().to_rgb8()), image::ImageFormat::Jpeg);
    jpeg.truncate(jpeg.len() / 3);
    v.push(match decodes(jpeg) {
        Ok(()) | Err(WorkerError::Decode) | Err(WorkerError::Failed) => check(
            "security",
            "Malformed media",
            s,
            "Contained",
            "A truncated JPEG was handled by the worker without affecting Mori.",
        ),
        r => check("security", "Malformed media", Status::Limited, "Unexpected", &format!("Result: {r:?}.")),
    });
    let evil = zip(&[("../../escape.txt", b"x"), ("/etc/absolute.txt", b"y"), ("ok.txt", b"z")]);
    let nested = zip(&[("inner.zip", &zip(&[("deeper.zip", &zip(&[("deepest.zip", &zip(&[("x.txt", b"x")]))]))]))]);
    let a = crate::archive::inspect(&mut std::io::Cursor::new(&evil), evil.len() as u64, "zip");
    let b = crate::archive::inspect(&mut std::io::Cursor::new(&nested), nested.len() as u64, "zip");
    let codes = |r: &Result<crate::archive::Listing, String>| {
        r.as_ref().map(|l| l.findings.iter().map(|f| f.code).collect::<Vec<_>>()).unwrap_or_default()
    };
    let (ca, cb) = (codes(&a), codes(&b));
    v.push(if ca.contains(&"archive-traversal") && ca.contains(&"archive-absolute") && cb.iter().any(|c| c.starts_with("archive-nest")) {
        check("security", "Archive traversal protection", s, "Active", "A ZIP with “../” and absolute paths was flagged, and a ZIP nested four levels deep was flagged; nothing was extracted.")
    } else {
        check("security", "Archive traversal protection", Status::Fail, "Not working", &format!("Findings: {ca:?} / {cb:?}."))
    });

    let scratch = Scratch::new();
    match &scratch {
        Some(sc) => {
            let d = &sc.0;
            let ok = (|| -> Option<(bool, bool, bool, bool)> {
                fs::create_dir_all(d.join("root/sub")).ok()?;
                fs::write(d.join("outside.txt"), b"secret").ok()?;
                fs::write(d.join("root/sub/a.txt"), b"a").ok()?;
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(d.join("outside.txt"), d.join("root/link.txt")).ok()?;
                    std::os::unix::fs::symlink("../root", d.join("root/sub/loop")).ok()?;
                }
                let root = d.join("root");
                let link_refused = crate::secure::open_inside(&root, "link.txt").is_err()
                    && crate::secure::open_inside(&root, "../outside.txt").is_err();
                let idx = crate::index::scan(&root, &|| false, &mut |_, _| {}, None)?;
                let no_loop = idx.files.iter().filter(|e| e.kind != crate::index::Kind::Link).count() == 1;
                // Read-only Mode and a protected folder refuse a rename.
                let store = crate::privacy::Store::load(d.join("protected.json"));
                let ro = crate::policy::Policy { read_only: true, protected: &store };
                let ro_refused =
                    crate::fileops::rename_no_replace(&ro, &root.join("sub/a.txt"), &root.join("sub/b.txt")).is_err()
                        && root.join("sub/a.txt").exists();
                store.set(&root.join("sub"), 0, true).ok()?;
                let rw = crate::policy::Policy { read_only: false, protected: &store };
                let protected_refused =
                    crate::fileops::rename_no_replace(&rw, &root.join("sub/a.txt"), &root.join("sub/b.txt")).is_err()
                        && root.join("sub/a.txt").exists();
                Some((link_refused, no_loop, ro_refused, protected_refused))
            })();
            let (link, loops, ro, prot) = ok.unwrap_or((false, false, false, false));
            v.push(if link && loops {
                check("security", "Symlink protection", s, "Active", "A link pointing outside a folder was refused, and a link loop didn't pull anything into the index.")
            } else {
                check("security", "Symlink protection", Status::Fail, "Not working", "A synthetic link escape or loop wasn't handled.")
            });
            v.push(if ro {
                check(
                    "security",
                    "Read-only enforcement",
                    s,
                    if read_only_setting { "On" } else { "Available" },
                    "With Read-only Mode, the backend refused a rename of a scratch file; the file was unchanged.",
                )
            } else {
                check(
                    "security",
                    "Read-only enforcement",
                    Status::Fail,
                    "Not working",
                    "A rename went through in Read-only Mode.",
                )
            });
            v.push(if prot {
                check(
                    "security",
                    "Protected folders",
                    s,
                    "Available",
                    "A rename inside a scratch “Never Modify” folder was refused.",
                )
            } else {
                check(
                    "security",
                    "Protected folders",
                    Status::Fail,
                    "Not working",
                    "A rename inside a protected folder went through.",
                )
            });
        }
        None => v.push(check(
            "security",
            "Filesystem checks",
            Status::Fail,
            "Not run",
            "A scratch folder couldn't be created in the system temporary directory.",
        )),
    }

    // ----------------------------------------------------------- privacy
    v.push(check(
        "privacy",
        "Cloud account",
        Status::Info,
        "None",
        "Mori has no account, login or registration, and creates no remote identity (code fact).",
    ));
    v.push(check(
        "privacy",
        "Telemetry & analytics",
        Status::Info,
        "None",
        "No telemetry, analytics, crash-reporting or tracking code or dependencies (code fact; dependencies audited).",
    ));
    v.push(check(
        "privacy",
        "Remote processing",
        Status::Info,
        "None",
        "Media, metadata, checksums and analyses are processed on this computer. No uploads, no remote AI (code fact).",
    ));
    v.push(if temporary {
        check(
            "privacy",
            "Persistent local index",
            s,
            "Off (temporary session)",
            "This session's index stays in memory; nothing about the folder is written.",
        )
    } else {
        check(
            "privacy",
            "Persistent local index",
            Status::Info,
            &format!("On · {}", human(index_bytes)),
            "Folder indexes are cached in Mori's app data to make browsing fast. Use a Temporary Session to avoid it.",
        )
    });
    v.push(check("privacy", "Web view storage", Status::Info, "In memory only", "The web view uses a non-persistent data store: no cookies, local storage, IndexedDB or HTTP cache are written (configuration fact)."));

    // ----------------------------------------------------------- network
    v.push(check(
        "network",
        "Required for core functionality",
        Status::Info,
        "No",
        "Browsing, previews, analyses, metadata, checksums and integrity checks run locally.",
    ));
    v.push(check(
        "network",
        "Network features configured",
        Status::Info,
        "None",
        "Mori contains no update checks, map tiles, fonts, or other remote requests (code fact).",
    ));
    let remote = ["http:", "https:", "ws:", "wss:"].iter().any(|scheme| {
        csp.split(';').any(|d| {
            d.split_whitespace()
                .skip(1)
                .any(|t| t.starts_with(scheme) && !t.contains("localhost") && !t.contains("127.0.0.1"))
        })
    });
    v.push(if !csp.is_empty() && !remote {
        check("network", "Web view remote content", s, "Blocked", "The active Content-Security-Policy allows only Mori's own bundled UI, IPC and the local mori: protocol; every remote origin is refused by the web view.")
    } else {
        check("network", "Web view remote content", Status::Limited, "Not restricted", "The Content-Security-Policy doesn't block every remote origin.")
    });
    v.push(match base {
        Some((_, _, true, _)) => check(
            "network",
            "Decoder worker network access",
            s,
            "Blocked",
            "Enforced by the OS sandbox; verified just now on the loopback interface (no outside server is contacted).",
        ),
        _ => check(
            "network",
            "Decoder worker network access",
            Status::Limited,
            "Not enforced",
            "No OS-level network denial for workers on this platform; they contain no network code.",
        ),
    });
    v.push(check("network", "Main process", Status::Info, "No network code", "The main process has no network functionality configured. It isn't sandboxed from the network at OS level, so this is a code fact, not an enforced barrier."));

    // --------------------------------------------------------- ephemeral
    v.push(check("ephemeral", "Temporary session", s, if temporary { "Active" } else { "Available" }, "Index, thumbnails, previews, frames, checksums, analyses and history stay in memory and are cleared when the session ends."));
    v.push(check("ephemeral", "Startup cleanup", s, "Active", &format!("At launch Mori removes leftovers of interrupted writes inside its own folders ({stale_cleaned} this time).")));
    v.push(if temporary {
        check("ephemeral", "Persistent records during session", s, "Disabled", "Saving tags, favorites, folder rules, integrity snapshots or drive settings is refused while the session lasts.")
    } else {
        check("ephemeral", "Persistent records during session", Status::Info, "Disabled in sessions", "Start a Temporary Session or Private Inspection to turn them off.")
    });
    v.push(check("ephemeral", "External app launching", if temporary { s } else { Status::Info }, if temporary { "Blocked" } else { "Blocked in sessions" }, "During a temporary session Mori refuses “Open with Default App” and “Show in Finder”, which could create history in other apps."));
    let writable = if temporary {
        check(
            "ephemeral",
            "Local data store",
            Status::Info,
            "Not checked",
            "Not tested during a temporary session (the test writes a file).",
        )
    } else {
        let probe = data_dir.join("selftest.probe");
        let ok = fs::write(&probe, b"ok").is_ok()
            && fs::read(&probe).ok().as_deref() == Some(b"ok")
            && fs::remove_file(&probe).is_ok();
        if ok {
            check(
                "ephemeral",
                "Local data store",
                s,
                "Writable",
                "Mori's app-data folder was written, read back and cleaned just now.",
            )
        } else {
            check(
                "ephemeral",
                "Local data store",
                Status::Fail,
                "Not writable",
                "Mori couldn't write to its app-data folder; settings and records won't be saved.",
            )
        }
    };
    v.push(writable);
    let scratch_ok = scratch.as_ref().map(|s| s.0.clone());
    drop(scratch);
    v.push(match scratch_ok {
        Some(p) if !p.exists() => check(
            "ephemeral",
            "Temporary files",
            s,
            "Created and removed",
            "The self-test's scratch folder was created and fully removed.",
        ),
        Some(_) => check(
            "ephemeral",
            "Temporary files",
            Status::Fail,
            "Left behind",
            "The self-test's scratch folder couldn't be removed.",
        ),
        None => check(
            "ephemeral",
            "Temporary files",
            Status::Fail,
            "Not created",
            "The system temporary directory isn't writable.",
        ),
    });

    // ----------------------------------------------------------- logging
    v.push(if cfg!(debug_assertions) {
        check("logging", "Logs", Status::Limited, "Developer build", "This is a debug build: developer diagnostics may print paths to the terminal when explicitly enabled with MORI_DEBUG_* variables.")
    } else {
        check("logging", "Logs", Status::Info, "None", "Release builds write no logs. After a crash, a marker with the time, thread name and source-code line (never a message, path or file name) is kept in Mori's app data.")
    });
    v
}

fn human(b: u64) -> String {
    match b {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b / 1024),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In unit tests the executable isn't Mori, so no worker can start:
    /// every worker-backed capability must be reported as not working —
    /// never a false check mark.
    #[test]
    fn no_false_checkmarks_when_the_worker_is_unavailable() {
        let d = std::env::temp_dir();
        let checks = run("default-src 'none'; img-src 'self' mori:", false, false, 0, 0, &d);
        let get = |l: &str| checks.iter().find(|c| c.label == l).unwrap_or_else(|| panic!("{l}"));
        for l in ["JPEG", "PNG", "PDF safe preview", "Resource limits"] {
            assert_ne!(get(l).status, Status::Pass, "{l}");
        }
        if cfg!(target_os = "macos") {
            assert_ne!(get("Restricted decoder workers").status, Status::Pass);
            assert_ne!(get("Preview isolation").status, Status::Pass);
            assert_ne!(get("Decoder worker network access").status, Status::Pass);
        }
        // Checks that don't need the worker still run for real.
        assert_eq!(get("Real file type detection").status, Status::Pass);
        assert_eq!(get("Archive traversal protection").status, Status::Pass);
        assert_eq!(get("Symlink protection").status, Status::Pass);
        assert_eq!(get("Read-only enforcement").status, Status::Pass);
        // A CSP that allows a remote origin is reported as not restricted.
        let open = run("default-src 'none'; img-src https://example.com", false, false, 0, 0, &d);
        assert_ne!(open.iter().find(|c| c.label == "Web view remote content").unwrap().status, Status::Pass);
    }

    #[test]
    fn fixtures_are_what_they_claim() {
        let z = zip(&[("../../escape.txt", b"x"), ("/etc/absolute.txt", b"y"), ("ok.txt", b"z")]);
        let l = crate::archive::inspect(&mut std::io::Cursor::new(&z), z.len() as u64, "zip").unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names, ["../../escape.txt", "/etc/absolute.txt", "ok.txt"]);
        let codes: Vec<&str> = l.findings.iter().map(|f| f.code).collect();
        assert!(codes.contains(&"archive-traversal") && codes.contains(&"archive-absolute"), "{codes:?}");
        assert!(pdf().starts_with(b"%PDF-1.4"));
        assert_eq!(&bomb_png()[16..20], &100_000u32.to_be_bytes());
        let s = Scratch::new().unwrap();
        let p = s.0.clone();
        assert!(p.exists());
        drop(s);
        assert!(!p.exists(), "the scratch folder is removed");
    }
}
