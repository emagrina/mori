//! Store transport only: decoding remains in the separately sandboxed XPC executable.
use super::*;
use std::collections::VecDeque;
use std::ffi::{c_char, CString};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

// Only lazy callers may queue: waiting requests retain file handles, not input buffers.
// Already-materialized requests never wait and cannot grow a queue of 160 MiB Vecs.
const MAX_WAITERS: usize = 16;
#[derive(Default)]
struct AdmissionState {
    active: bool,
    next: u64,
    waiting: VecDeque<u64>,
}
struct Admission {
    state: Mutex<AdmissionState>,
    changed: Condvar,
}
impl Admission {
    const fn new() -> Self {
        Self {
            state: Mutex::new(AdmissionState { active: false, next: 0, waiting: VecDeque::new() }),
            changed: Condvar::new(),
        }
    }
    fn acquire(&self, deadline: Instant, wait: bool) -> Result<Permit<'_>, WorkerError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if Instant::now() >= deadline {
            return Err(WorkerError::Busy);
        }
        if !state.active && state.waiting.is_empty() {
            state.active = true;
            return Ok(Permit(self));
        }
        if !wait || state.waiting.len() >= MAX_WAITERS {
            return Err(WorkerError::Busy);
        }
        let ticket = state.next;
        state.next = state.next.wrapping_add(1);
        state.waiting.push_back(ticket);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.waiting.retain(|&v| v != ticket);
                self.changed.notify_all();
                return Err(WorkerError::Busy);
            }
            if !state.active && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.active = true;
                return Ok(Permit(self));
            }
            state = self.changed.wait_timeout(state, remaining).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}
struct Permit<'a>(&'a Admission);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner).active = false;
        self.0.changed.notify_all();
    }
}
static ADMISSION: Admission = Admission::new();
extern "C" {
    fn mori_xpc_call(
        op: *const c_char,
        max: u32,
        input: *const u8,
        len: usize,
        timeout_ms: u64,
        output: *mut *mut u8,
        output_len: *mut usize,
    ) -> i32;
}

pub(super) fn run(op: Op, max: u32, input: Vec<u8>, timeout: std::time::Duration) -> Result<Output, WorkerError> {
    run_admitted(op, max, timeout, false, || Ok(input))
}

pub(super) fn run_lazy(
    op: Op,
    max: u32,
    timeout: Duration,
    load: impl FnOnce() -> Result<Vec<u8>, WorkerError>,
) -> Result<Output, WorkerError> {
    run_admitted(op, max, timeout, true, load)
}

fn run_admitted(
    op: Op,
    max: u32,
    timeout: Duration,
    wait: bool,
    load: impl FnOnce() -> Result<Vec<u8>, WorkerError>,
) -> Result<Output, WorkerError> {
    if !(32..=4096).contains(&max) {
        return Err(WorkerError::Limits);
    }
    if timeout.is_zero() {
        return Err(WorkerError::Timeout);
    }
    let deadline = Instant::now() + timeout.min(Duration::from_secs(30));
    let _permit = ADMISSION.acquire(deadline, wait)?;
    let input = load()?;
    if input.len() as u64 > MAX_INPUT {
        return Err(WorkerError::Limits);
    }
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        return Err(WorkerError::Timeout);
    }
    let name = CString::new(op.name()).map_err(|_| WorkerError::Failed)?;
    let mut data = std::ptr::null_mut();
    let mut len = 0;
    let status = unsafe {
        mori_xpc_call(
            name.as_ptr(),
            max,
            input.as_ptr(),
            input.len(),
            timeout.as_millis().clamp(1, 30_000) as u64,
            &mut data,
            &mut len,
        )
    };
    // The C bridge owns malloc allocation; always free, including malformed responses.
    let wire = if status == 0 && !data.is_null() && (16..=MAX_OUTPUT + 16).contains(&len) {
        Some(unsafe { std::slice::from_raw_parts(data, len) }.to_vec())
    } else {
        None
    };
    unsafe { libc::free(data.cast()) };
    match status {
        0 => validate(op, max, wire.ok_or(WorkerError::Failed)?),
        EXIT_UNSUPPORTED => Err(WorkerError::Unsupported),
        EXIT_LIMITS => Err(WorkerError::Limits),
        EXIT_DECODE => Err(WorkerError::Decode),
        8 => Err(WorkerError::Timeout),
        _ => Err(WorkerError::Failed),
    }
}

fn validate(op: Op, max: u32, wire: Vec<u8>) -> Result<Output, WorkerError> {
    let out = parse_output(wire)?;
    let valid_format = match op {
        Op::Thumb | Op::Frame | Op::PdfPage => out.format == OutFormat::Png,
        Op::Preview => out.format == OutFormat::Png,
        Op::Probe => out.format == OutFormat::Probe,
        Op::Fingerprint => out.format == OutFormat::Raw,
        Op::PdfInfo => out.format == OutFormat::Text,
        Op::Meta | Op::MetaBatch => out.format == OutFormat::Json,
        Op::Sanitize => out.format == OutFormat::Bytes,
    };
    if !valid_format {
        return Err(WorkerError::Failed);
    }
    if out.format == OutFormat::Png && !output_validation::static_png(&out.bytes, max) {
        return Err(WorkerError::Failed);
    }
    if matches!(op, Op::Thumb | Op::Preview | Op::Frame | Op::PdfPage | Op::Fingerprint) {
        check_dims(out.width, out.height).map_err(|_| WorkerError::Limits)?;
    } else if !matches!(op, Op::Probe) && (out.width != 0 || out.height != 0) {
        return Err(WorkerError::Failed);
    }
    if matches!(op, Op::Meta | Op::MetaBatch) {
        let value: serde_json::Value = serde_json::from_slice(&out.bytes).map_err(|_| WorkerError::Failed)?;
        if (op == Op::Meta && !value.is_object())
            || (op == Op::MetaBatch && !value.as_array().is_some_and(|a| a.len() <= 256))
        {
            return Err(WorkerError::Failed);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_is_bounded_deadlined_and_releases_on_panic() {
        let gate = Admission::new();
        let permit = gate.acquire(Instant::now() + Duration::from_secs(2), false).unwrap();
        assert!(matches!(gate.acquire(Instant::now() + Duration::from_secs(1), false), Err(WorkerError::Busy)));
        let start = Instant::now();
        assert!(matches!(gate.acquire(start + Duration::from_millis(25), true), Err(WorkerError::Busy)));
        assert!(start.elapsed() < Duration::from_millis(500));
        assert!(gate.state.lock().unwrap().waiting.is_empty());
        {
            let mut s = gate.state.lock().unwrap();
            s.waiting.extend(0..MAX_WAITERS as u64);
        }
        assert!(matches!(gate.acquire(Instant::now() + Duration::from_secs(1), true), Err(WorkerError::Busy)));
        gate.state.lock().unwrap().waiting.clear();
        drop(permit);
        let _ = std::panic::catch_unwind(|| {
            let _permit = gate.acquire(Instant::now() + Duration::from_secs(1), true).unwrap();
            panic!("synthetic loader panic");
        });
        assert!(gate.acquire(Instant::now() + Duration::from_secs(1), false).is_ok());
    }
    #[test]
    fn fifteen_concurrent_jobs_load_only_after_admission() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let gate = Admission::new();
        let active = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let start = std::sync::Barrier::new(15);
        std::thread::scope(|scope| {
            for _ in 0..15 {
                scope.spawn(|| {
                    start.wait();
                    let _permit = gate.acquire(Instant::now() + Duration::from_secs(3), true).unwrap();
                    assert_eq!(active.fetch_add(1, Ordering::SeqCst), 0);
                    // This represents the loader: no buffer exists before the permit.
                    let bytes = vec![0u8; 1024];
                    std::thread::sleep(Duration::from_millis(2));
                    assert_eq!(bytes.len(), 1024);
                    active.fetch_sub(1, Ordering::SeqCst);
                    completed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        assert_eq!(completed.load(Ordering::SeqCst), 15);
    }
    fn wire(format: u8, width: u32, height: u32, data: &[u8]) -> Vec<u8> {
        let mut v = b"MORI".to_vec();
        v.extend(width.to_le_bytes());
        v.extend(height.to_le_bytes());
        v.extend([format, 0, 0, 0]);
        v.extend(data);
        v
    }
    #[test]
    fn validates_operation_dimensions_json_and_reserved_bytes() {
        assert!(validate(Op::Meta, 128, wire(7, 0, 0, b"{\"width\":1}")).is_ok());
        assert!(validate(Op::Thumb, 128, wire(7, 0, 0, b"{}")).is_err());
        assert!(validate(Op::Thumb, 128, wire(1, 0, 1, &[255, 216, 255])).is_err());
        assert!(validate(Op::Thumb, 128, wire(1, 20_001, 1, &[255, 216, 255])).is_err());
        assert!(validate(Op::Meta, 128, wire(7, 0, 0, b"{broken")).is_err());
        assert!(validate(Op::MetaBatch, 128, wire(7, 0, 0, b"{}")).is_err());
        let mut v = wire(7, 0, 0, b"{}");
        v[15] = 1;
        assert!(validate(Op::Meta, 128, v).is_err());
    }
    #[test]
    fn busy_service_rejects_without_waiting_or_fallback() {
        let _held = ADMISSION.acquire(Instant::now() + Duration::from_secs(1), false).unwrap();
        let start = std::time::Instant::now();
        assert!(matches!(run(Op::Thumb, 64, vec![0; 1024], std::time::Duration::from_secs(1)), Err(WorkerError::Busy)));
        assert!(start.elapsed() < std::time::Duration::from_millis(100));
        assert!(matches!(
            run_lazy(Op::Thumb, 64, Duration::from_millis(25), || {
                panic!("input must not be read while admission is occupied")
            }),
            Err(WorkerError::Busy)
        ));
        assert!(!enter_sandbox(Profile::Pure));
    }
    #[test]
    fn rejects_invalid_requests_before_contacting_service() {
        assert!(matches!(run(Op::Thumb, 0, vec![], std::time::Duration::from_secs(1)), Err(WorkerError::Limits)));
        assert!(matches!(run(Op::Thumb, 64, vec![], std::time::Duration::ZERO), Err(WorkerError::Timeout)));
    }
}
