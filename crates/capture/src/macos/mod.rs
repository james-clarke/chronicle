//! macOS capture (m38). Polling providers behind the same traits as X11:
//! the frontmost app and its AX window title, the CoreGraphics idle clock
//! and input counters, the session lock flag, libproc for a terminal's
//! cwd, `lsof` for listeners, CoreAudio for the mic. C frameworks are
//! declared by hand in `ffi`; Objective-C goes through objc2.

pub mod ax;
pub mod ffi;
pub mod focus;
pub mod input;
pub mod lock;
pub mod lsof;
pub mod mic;
pub mod proc;
