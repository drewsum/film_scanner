use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Receives progress updates from a running scan and lets the caller cancel it.
pub trait ProgressSink {
    /// Called with the overall fraction completed, in `0.0..=1.0`.
    fn progress(&mut self, fraction: f32);
    /// Polled by the device between chunks; returning `true` aborts the scan.
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A sink that ignores progress and never cancels.
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn progress(&mut self, _fraction: f32) {}
}

/// Shared, clonable cancellation flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
