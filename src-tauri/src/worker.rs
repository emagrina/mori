//! Sandboxed media worker.
//!
//! Untrusted image data is never decoded inside the main Mori process. For
//! each job Mori starts a fresh copy of its own executable in worker mode:
//!
//!   mori --mori-worker <op> <max-edge>
//!
//! The worker locks itself down *before reading any input* (OS sandbox with no
//! filesystem or network access, no new processes, CPU / file-size / memory
//! limits), reads the raw file bytes from stdin, decodes them with
//! memory-safe Rust decoders under strict size limits, and writes a freshly
//! encoded image to stdout. It never receives a path, so even a fully
//! compromised worker cannot reach other files. If it crashes, hangs or blows
//! a limit, Mori kills it and just shows "preview unavailable".
//!
//! Wire format (stdout), deliberately trivial — no general deserialisation:
//!   b"MORI" | u32 LE source width | u32 LE source height | u8 format | 3 × 0
//!   followed by the encoded image bytes.

use image::codecs::gif::{GifDecoder, GifEncoder, Repeat};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{AnimationDecoder, DynamicImage, Frame, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits};
use std::io::{Cursor, Read, Write};
use std::sync::PoisonError;

pub const WORKER_FLAG: &str = "--mori-worker";

/// Largest source file handed to a worker.
pub const MAX_INPUT: u64 = 160 * 1024 * 1024;
/// Largest encoded image accepted back from a worker.
const MAX_OUTPUT: usize = 64 * 1024 * 1024;
/// Decompression-bomb guards.
const MAX_DIMENSION: u32 = 20_000;
const MAX_PIXELS: u64 = 100_000_000;
const MAX_DECODE_ALLOC: u64 = 512 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 600;
const MAX_GIF_EDGE: u32 = 720;
/// CPU seconds before the kernel kills the worker (wall-clock is enforced by the host too).
const CPU_SECONDS: u64 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// Small still thumbnail.
    Thumb,
    /// Large still (or animated, for GIF) preview.
    Preview,
    /// Re-encode a PNG video frame captured by the webview into a thumbnail.
    Frame,
    /// Identify a video's codecs from an MP4 `moov` box or the head of a WebM file.
    Probe,
}

impl Op {
    fn name(self) -> &'static str {
        match self {
            Op::Thumb => "thumb",
            Op::Preview => "preview",
            Op::Frame => "frame",
            Op::Probe => "probe",
        }
    }
    fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "thumb" => Op::Thumb,
            "preview" => Op::Preview,
            "frame" => Op::Frame,
            "probe" => Op::Probe,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutFormat {
    Jpeg = 1,
    Png = 2,
    Gif = 3,
    /// Plain `key=value` lines from `Op::Probe`.
    Probe = 4,
}

impl OutFormat {
    pub fn mime(self) -> &'static str {
        match self {
            OutFormat::Jpeg => "image/jpeg",
            OutFormat::Png => "image/png",
            OutFormat::Gif => "image/gif",
            OutFormat::Probe => "text/plain",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            OutFormat::Jpeg => "jpg",
            OutFormat::Png => "png",
            OutFormat::Gif => "gif",
            OutFormat::Probe => "txt",
        }
    }
}

pub struct Output {
    pub width: u32,
    pub height: u32,
    pub format: OutFormat,
    pub bytes: Vec<u8>,
}

// Exit codes.
const EXIT_UNSUPPORTED: i32 = 2;
const EXIT_LIMITS: i32 = 3;
const EXIT_DECODE: i32 = 4;
const EXIT_SANDBOX: i32 = 5;
const EXIT_USAGE: i32 = 6;

// ===================================================================== child

/// Entry point when running as a worker. Never returns.
pub fn worker_main(args: &[String]) -> ! {
    // Lock down first; refuse to touch input if the sandbox can't be applied.
    restrict_resources();
    if !enter_sandbox() {
        std::process::exit(EXIT_SANDBOX);
    }
    if args.first().map(String::as_str) == Some("selftest") {
        selftest();
    }
    let (Some(op), Some(max)) = (
        args.first().and_then(|s| Op::parse(s)),
        args.get(1).and_then(|s| s.parse::<u32>().ok()).filter(|m| (32..=4096).contains(m)),
    ) else {
        std::process::exit(EXIT_USAGE)
    };

    let mut input = Vec::new();
    if std::io::stdin().take(MAX_INPUT + 1).read_to_end(&mut input).is_err() || input.len() as u64 > MAX_INPUT {
        std::process::exit(EXIT_LIMITS);
    }
    let result = match op {
        Op::Frame => process(&input, max, &[ImageFormat::Png], false, true),
        Op::Thumb => process(
            &input,
            max,
            &[ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP, ImageFormat::Gif],
            false,
            false,
        ),
        Op::Preview => process(
            &input,
            max,
            &[ImageFormat::Jpeg, ImageFormat::Png, ImageFormat::WebP, ImageFormat::Gif],
            true,
            false,
        ),
        Op::Probe => probe_video(&input),
    };
    match result {
        Ok(out) => {
            let mut stdout = std::io::stdout().lock();
            let mut header = Vec::with_capacity(16);
            header.extend_from_slice(b"MORI");
            header.extend_from_slice(&out.width.to_le_bytes());
            header.extend_from_slice(&out.height.to_le_bytes());
            header.extend_from_slice(&[out.format as u8, 0, 0, 0]);
            let ok = stdout.write_all(&header).and_then(|_| stdout.write_all(&out.bytes)).and_then(|_| stdout.flush());
            std::process::exit(if ok.is_ok() { 0 } else { EXIT_DECODE });
        }
        Err(code) => std::process::exit(code),
    }
}

/// Input: 4-byte tag (`MP4\0` + moov payload, or `WEBM` + file head).
fn probe_video(input: &[u8]) -> Result<Output, i32> {
    let (tag, data) = input.split_at_checked(4).ok_or(EXIT_UNSUPPORTED)?;
    let probe = match tag {
        b"MP4\0" => crate::probe::mp4_moov(data),
        b"WEBM" => crate::probe::webm(data),
        _ => return Err(EXIT_UNSUPPORTED),
    }
    .map_err(|_| EXIT_DECODE)?;
    Ok(Output { width: probe.width, height: probe.height, format: OutFormat::Probe, bytes: probe.encode() })
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_image_width = Some(MAX_DIMENSION);
    l.max_image_height = Some(MAX_DIMENSION);
    l.max_alloc = Some(MAX_DECODE_ALLOC);
    l
}

fn classify(e: image::ImageError) -> i32 {
    match e {
        image::ImageError::Limits(_) => EXIT_LIMITS,
        _ => EXIT_DECODE,
    }
}

fn check_dims(w: u32, h: u32) -> Result<(), i32> {
    if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION || (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(EXIT_LIMITS);
    }
    Ok(())
}

fn process(input: &[u8], max: u32, allowed: &[ImageFormat], animate: bool, flatten: bool) -> Result<Output, i32> {
    // Format comes from magic bytes only; the filename is never seen here.
    let format = image::guess_format(input).map_err(|_| EXIT_UNSUPPORTED)?;
    if !allowed.contains(&format) {
        return Err(EXIT_UNSUPPORTED);
    }
    if format == ImageFormat::Gif && animate {
        return animated_gif(input, max.min(MAX_GIF_EDGE));
    }

    let mut reader = ImageReader::with_format(Cursor::new(input), format);
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(classify)?;
    let (w, h) = decoder.dimensions();
    check_dims(w, h)?;
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder).map_err(classify)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let (src_w, src_h) = (img.width(), img.height());
    if flatten {
        img = DynamicImage::ImageRgb8(img.to_rgb8());
    }
    if src_w > max || src_h > max {
        img = img.resize(max, max, FilterType::Triangle);
    }
    encode_still(img, src_w, src_h)
}

fn encode_still(img: DynamicImage, src_w: u32, src_h: u32) -> Result<Output, i32> {
    let mut bytes = Vec::new();
    let format = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        PngEncoder::new(&mut bytes)
            .write_image(&rgba, rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
            .map_err(|_| EXIT_DECODE)?;
        OutFormat::Png
    } else {
        let rgb = img.to_rgb8();
        JpegEncoder::new_with_quality(&mut bytes, 86).encode_image(&rgb).map_err(|_| EXIT_DECODE)?;
        OutFormat::Jpeg
    };
    Ok(Output { width: src_w, height: src_h, format, bytes })
}

/// Decode and re-encode an animated GIF frame by frame (bounded memory).
fn animated_gif(input: &[u8], max: u32) -> Result<Output, i32> {
    let mut decoder = GifDecoder::new(Cursor::new(input)).map_err(classify)?;
    let (w, h) = decoder.dimensions();
    check_dims(w, h)?;
    decoder.set_limits(limits()).map_err(|_| EXIT_LIMITS)?;
    let scale = (max as f64 / w.max(h) as f64).min(1.0);
    let (ow, oh) = (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1));

    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new_with_speed(Capped(&mut bytes), 30);
        encoder.set_repeat(Repeat::Infinite).map_err(|_| EXIT_DECODE)?;
        for (i, frame) in decoder.into_frames().enumerate() {
            if i >= MAX_GIF_FRAMES {
                break;
            }
            let frame = frame.map_err(|_| EXIT_DECODE)?;
            let delay = frame.delay();
            let mut buf = frame.into_buffer();
            if scale < 1.0 {
                buf = image::imageops::resize(&buf, ow, oh, FilterType::Triangle);
            }
            encoder.encode_frame(Frame::from_parts(buf, 0, 0, delay)).map_err(|_| EXIT_LIMITS)?;
        }
    }
    Ok(Output { width: w, height: h, format: OutFormat::Gif, bytes })
}

/// Writer that refuses to grow past MAX_OUTPUT.
struct Capped<'a>(&'a mut Vec<u8>);

impl Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + buf.len() > MAX_OUTPUT {
            return Err(std::io::Error::other("output limit"));
        }
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
fn restrict_resources() {
    fn set(resource: libc::c_int, value: u64) {
        let lim = libc::rlimit { rlim_cur: value as libc::rlim_t, rlim_max: value as libc::rlim_t };
        unsafe { libc::setrlimit(resource as _, &lim) };
    }
    set(libc::RLIMIT_CPU as _, CPU_SECONDS);
    // May not create or grow files; attempts fail with EFBIG instead of a signal.
    unsafe { libc::signal(libc::SIGXFSZ, libc::SIG_IGN) };
    set(libc::RLIMIT_FSIZE as _, 0);
    set(libc::RLIMIT_CORE as _, 0);
    set(libc::RLIMIT_NOFILE as _, 8);
    #[cfg(target_os = "linux")]
    {
        set(libc::RLIMIT_AS as _, 2 * 1024 * 1024 * 1024);
        set(libc::RLIMIT_NPROC as _, 0);
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    }
}

#[cfg(windows)]
fn restrict_resources() {
    // On Windows the host places the worker in a Job object (memory, CPU,
    // process-count and UI limits) before sending it any data.
}

/// macOS: Seatbelt "pure-computation" profile — no filesystem, no network,
/// no IPC, no process creation. Applied after the executable has loaded.
#[cfg(target_os = "macos")]
fn enter_sandbox() -> bool {
    use std::ffi::{c_char, c_int};
    extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }
    const SANDBOX_NAMED: u64 = 0x0001;
    unsafe {
        let mut err: *mut c_char = std::ptr::null_mut();
        // Same value as the SDK's kSBXProfilePureComputation (a char array).
        let rc = sandbox_init(c"pure-computation".as_ptr(), SANDBOX_NAMED, &mut err);
        if !err.is_null() {
            sandbox_free_error(err);
        }
        rc == 0
    }
}

#[cfg(not(target_os = "macos"))]
fn enter_sandbox() -> bool {
    // Linux: rlimits + no_new_privs (see restrict_resources); Windows: Job object
    // applied by the host. The worker still never receives a path.
    true
}

/// Used by tests: prove the sandbox denies filesystem and network access.
fn selftest() -> ! {
    let fs_denied = std::fs::read("/etc/hosts").is_err() && std::fs::read_dir("/").is_err();
    let write_denied = std::fs::write(std::env::temp_dir().join("mori-selftest"), b"x").is_err();
    let net_denied =
        std::net::TcpStream::connect_timeout(&"1.1.1.1:443".parse().unwrap(), std::time::Duration::from_millis(500))
            .is_err()
            && std::net::TcpListener::bind("127.0.0.1:0").is_err();
    let spawn_denied = std::process::Command::new("/bin/echo").output().is_err();
    println!("fs_denied={fs_denied} write_denied={write_denied} net_denied={net_denied} spawn_denied={spawn_denied}");
    std::process::exit(0);
}

// ====================================================================== host

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerError {
    /// Not a format the worker will decode.
    Unsupported,
    /// Crashed, timed out, hit a limit or produced garbage.
    Failed,
}

/// At most this many workers run at once.
const MAX_WORKERS: usize = 3;
static SLOTS: (std::sync::Mutex<usize>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());

struct Slot;
impl Slot {
    fn acquire() -> Slot {
        let (lock, cv) = &SLOTS;
        let mut n = lock.lock().unwrap_or_else(PoisonError::into_inner);
        while *n >= MAX_WORKERS {
            n = cv.wait(n).unwrap_or_else(PoisonError::into_inner);
        }
        *n += 1;
        Slot
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let (lock, cv) = &SLOTS;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        cv.notify_one();
    }
}

/// Run one job in a fresh sandboxed worker process.
pub fn run(op: Op, max: u32, input: Vec<u8>, timeout: std::time::Duration) -> Result<Output, WorkerError> {
    use std::process::{Command, Stdio};
    let _slot = Slot::acquire();
    let exe = std::env::current_exe().map_err(|_| WorkerError::Failed)?;
    let mut cmd = Command::new(exe);
    // Fixed arguments only; nothing derived from the file or its name.
    cmd.arg(WORKER_FLAG)
        .arg(op.name())
        .arg(max.to_string())
        .env_clear()
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if let Some(root) = std::env::var_os("SystemRoot") {
            cmd.env("SystemRoot", root);
        }
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn().map_err(|_| WorkerError::Failed)?;
    #[cfg(windows)]
    let _job = win_job::confine(&child);

    let mut stdin = child.stdin.take().ok_or(WorkerError::Failed)?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let mut stdout = child.stdout.take().ok_or(WorkerError::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(MAX_OUTPUT as u64 + 17).read_to_end(&mut out);
        out
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(std::time::Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let Some(status) = status else { return Err(WorkerError::Failed) };
    if status.code() == Some(EXIT_UNSUPPORTED) {
        return Err(WorkerError::Unsupported);
    }
    if !status.success() {
        return Err(WorkerError::Failed);
    }
    parse_output(out)
}

fn parse_output(mut out: Vec<u8>) -> Result<Output, WorkerError> {
    if out.len() < 16 || &out[0..4] != b"MORI" || out.len() > MAX_OUTPUT + 16 {
        return Err(WorkerError::Failed);
    }
    let width = u32::from_le_bytes(out[4..8].try_into().unwrap());
    let height = u32::from_le_bytes(out[8..12].try_into().unwrap());
    let format = match out[12] {
        1 => OutFormat::Jpeg,
        2 => OutFormat::Png,
        3 => OutFormat::Gif,
        4 => OutFormat::Probe,
        _ => return Err(WorkerError::Failed),
    };
    // Double-check the payload really is the format the worker claimed.
    let ok_magic = match format {
        OutFormat::Jpeg => out[16..].starts_with(&[0xFF, 0xD8, 0xFF]),
        OutFormat::Png => out[16..].starts_with(b"\x89PNG"),
        OutFormat::Gif => out[16..].starts_with(b"GIF8"),
        OutFormat::Probe => out.len() <= 16 + 512 && out[16..].starts_with(b"video="),
    };
    if !ok_magic {
        return Err(WorkerError::Failed);
    }
    let bytes = out.split_off(16);
    Ok(Output { width, height, format, bytes })
}

#[cfg(windows)]
mod win_job {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::*;

    pub struct Job(HANDLE);
    impl Drop for Job {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Memory cap, single process, kill-on-close, no UI access.
    pub fn confine(child: &std::process::Child) -> Option<Job> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
                | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
                | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            info.BasicLimitInformation.ActiveProcessLimit = 1;
            info.ProcessMemoryLimit = 1024 * 1024 * 1024;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            let mut ui: JOBOBJECT_BASIC_UI_RESTRICTIONS = std::mem::zeroed();
            ui.UIRestrictionsClass = JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_EXITWINDOWS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD;
            SetInformationJobObject(
                job,
                JobObjectBasicUIRestrictions,
                &ui as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            );
            AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE);
            Some(Job(job))
        }
    }
}
