//! Control for long-running background jobs: cancel, pause and resume.
//!
//! A job's worker threads call `checkpoint()` between units of work. While
//! the job is paused they block there (holding no file open); a cancel wakes
//! them and makes `checkpoint()` return `Cancelled`.

use crate::dupes::Cancelled;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Default)]
pub struct Control {
    cancel: AtomicBool,
    paused: Mutex<bool>,
    wake: Condvar,
}

impl Control {
    pub fn reset(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.wake.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner) = paused;
        self.wake.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// The flag existing code (`dupes::collect`) polls.
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }

    /// Block while paused; `Err` once cancelled.
    pub fn checkpoint(&self) -> Result<(), Cancelled> {
        let mut paused = self.paused.lock().unwrap_or_else(PoisonError::into_inner);
        while *paused && !self.cancelled() {
            paused =
                self.wake.wait_timeout(paused, Duration::from_millis(500)).unwrap_or_else(PoisonError::into_inner).0;
        }
        if self.cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn pause_blocks_resume_releases_cancel_ends() {
        let c = Arc::new(Control::default());
        assert!(c.checkpoint().is_ok());
        c.set_paused(true);
        let c2 = c.clone();
        let t = std::thread::spawn(move || {
            let start = Instant::now();
            let r = c2.checkpoint();
            (r, start.elapsed())
        });
        std::thread::sleep(Duration::from_millis(150));
        c.set_paused(false);
        let (r, waited) = t.join().unwrap();
        assert!(r.is_ok() && waited >= Duration::from_millis(100));
        c.set_paused(true);
        let c3 = c.clone();
        let t = std::thread::spawn(move || c3.checkpoint());
        std::thread::sleep(Duration::from_millis(50));
        c.cancel();
        assert_eq!(t.join().unwrap(), Err(Cancelled));
        c.reset();
        assert!(c.checkpoint().is_ok() && !c.is_paused());
    }
}
