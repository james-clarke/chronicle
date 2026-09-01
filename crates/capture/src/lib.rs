//! Provider traits + platform impls (`#[cfg]`). Everything downstream
//! consumes `CaptureEvent` only. Platform impls land in M1 (X11), M17 (macOS),
//! M18 (Windows); the git poller (m15) is platform-independent.

pub mod git;
#[cfg(target_os = "linux")]
pub mod x11;

use chronicle_core::types::CaptureEvent;
use crossbeam_channel::Sender;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Blocking event loop, runs on its own thread.
pub trait FocusProvider: Send {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError>;
}

/// Polled (≤ 1/30 s).
pub trait AfkProvider: Send {
    fn idle_ms(&self) -> Result<u64, BoxError>;
}
