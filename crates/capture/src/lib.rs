//! Provider traits + platform impls (`#[cfg]`). Everything downstream
//! consumes `CaptureEvent` only. Platform impls land in M1 (X11), M17 (macOS),
//! M18 (Windows); the git poller (m15) and the AI session watcher (m22) are
//! platform-independent.

pub mod ai_sessions;
pub mod gcal;
pub mod git;
pub mod github;
#[cfg(target_os = "linux")]
pub mod lock;
#[cfg(target_os = "linux")]
pub mod mic;
#[cfg(target_os = "linux")]
pub mod presence;
pub mod shell;
#[cfg(target_os = "linux")]
pub mod x11;

use std::time::Duration;

use chronicle_core::types::{ActivityEvent, CaptureEvent, PresenceMinute};
use crossbeam_channel::Sender;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Blocking event loop, runs on its own thread.
pub trait FocusProvider: Send {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError>;
}

/// Screen lock edges (m32 chunk 0). Blocking, runs on its own thread;
/// `on_change(locked)` fires on the initial state and every edge, repeats
/// included.
pub trait LockSignal: Send {
    fn run(self, on_change: &mut dyn FnMut(bool)) -> Result<(), BoxError>;
}

/// Input counts per minute (m32 chunk 1): how many keys, buttons, motion
/// and scroll events, never which. Blocking, runs on its own thread;
/// `on_minute` fires once per minute that had any input and returns false
/// to stop the loop.
pub trait PresenceProvider: Send {
    fn run(self, on_minute: &mut dyn FnMut(PresenceMinute) -> bool) -> Result<(), BoxError>;
}

/// Polled (≤ 1/30 s).
pub trait AfkProvider: Send {
    fn idle_ms(&self) -> Result<u64, BoxError>;
}

/// Shared shape for the poll-then-sleep providers (git and shell poll
/// per-item state and sleep first, so they don't fit this): poll
/// immediately, emit every event, sleep, repeat. `poll` is responsible for
/// its own error handling (warn-once-per-streak, return no events on
/// failure) — this loop only stops, without error, once the receiver hangs
/// up.
pub(crate) fn poll_loop(
    tx: &Sender<CaptureEvent>,
    interval: Duration,
    mut poll: impl FnMut() -> Vec<ActivityEvent>,
) -> Result<(), BoxError> {
    loop {
        for event in poll() {
            if tx.send(CaptureEvent::Activity(event)).is_err() {
                return Ok(());
            }
        }
        std::thread::sleep(interval);
    }
}
