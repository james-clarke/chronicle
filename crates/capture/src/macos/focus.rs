//! Frontmost-app polling (m38): `NSWorkspace` has no "focus changed"
//! notification worth a run loop, so this polls once a second — X11 already
//! debounces titles to that granularity (x11.rs:33), so nothing downstream
//! sees a coarser signal. Window titles come from the Accessibility API,
//! which needs a one-time user grant; without it every title is empty.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chronicle_core::extract::{self, Family};
use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent, FocusEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRetained, CFString, CFType};

use crate::macos::ax;
use crate::macos::ffi::{self, AXUIElementRef};
use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(1);
/// How often a denied grant is re-checked, so a later trust change in
/// System Settings takes effect without restarting the daemon.
const TRUST_RECHECK: Duration = Duration::from_secs(30);

pub struct MacFocusProvider;

impl MacFocusProvider {
    pub fn new() -> Result<Self, BoxError> {
        Ok(Self)
    }
}

struct State {
    last_pid: Option<i32>,
    last_app: String,
    last_title: String,
    trusted: bool,
    trust_checked: Instant,
    warned: bool,
    /// First time each `(terminal pid, place)` was seen, for the cwd probe's
    /// upsert row (mirrors x11.rs).
    cwd_seen: HashMap<(u32, String), Timestamp>,
}

/// One AX round trip: the frontmost app's focused window, then that
/// window's title. Every step tolerates failure (wrong app, no window
/// focused, an `AXError`) by returning an empty string.
fn window_title(pid: i32) -> String {
    // SAFETY: `AXUIElementCreateApplication` never touches Rust memory; the
    // pid need not even be valid, it just returns an element that later
    // calls will fail against.
    let app: AXUIElementRef = unsafe { ffi::AXUIElementCreateApplication(pid) };
    let Some(app) = std::ptr::NonNull::new(app.cast_mut()) else {
        return String::new();
    };
    // SAFETY: `AXUIElementCreateApplication` follows the Create rule.
    let app: CFRetained<CFType> = unsafe { CFRetained::from_raw(app) };

    let focused_window = CFString::from_str("AXFocusedWindow");
    let mut window: *const CFType = std::ptr::null();
    // SAFETY: `window` is a valid out-pointer for the duration of the call.
    let err = unsafe {
        ffi::AXUIElementCopyAttributeValue(
            CFRetained::as_ptr(&app).as_ptr(),
            &focused_window,
            &mut window,
        )
    };
    if err != 0 {
        return String::new();
    }
    let Some(window) = std::ptr::NonNull::new(window.cast_mut()) else {
        return String::new();
    };
    // SAFETY: `AXUIElementCopyAttributeValue` follows the Copy rule.
    let window: CFRetained<CFType> = unsafe { CFRetained::from_raw(window) };

    let title_attr = CFString::from_str("AXTitle");
    let mut title: *const CFType = std::ptr::null();
    // SAFETY: `title` is a valid out-pointer for the duration of the call.
    let err = unsafe {
        ffi::AXUIElementCopyAttributeValue(
            CFRetained::as_ptr(&window).as_ptr(),
            &title_attr,
            &mut title,
        )
    };
    if err != 0 {
        return String::new();
    }
    let Some(title) = std::ptr::NonNull::new(title.cast_mut()) else {
        return String::new();
    };
    // SAFETY: `AXUIElementCopyAttributeValue` follows the Copy rule.
    let title: CFRetained<CFType> = unsafe { CFRetained::from_raw(title) };
    title
        .downcast_ref::<CFString>()
        .map(|s| s.to_string())
        .unwrap_or_default()
}

impl State {
    /// One cwd probe for a terminal window: emits (or refreshes) the
    /// `(pid, place)` row when the shell sits in a named place. Mirrors
    /// `x11.rs`'s `State::probe_cwd`.
    fn probe_cwd(&mut self, app: &str, pid: Option<u32>, tx: &Sender<CaptureEvent>) {
        let Some(pid) = pid else { return };
        if extract::family(app) != Family::Terminal {
            return;
        }
        let Some(cwd) = crate::macos::proc::terminal_cwd(pid) else {
            return;
        };
        let Some(place) = extract::place_from_path(&cwd) else {
            return;
        };
        let now = Timestamp::now();
        let first = *self.cwd_seen.entry((pid, place.clone())).or_insert(now);
        let event = ActivityEvent {
            ts: first,
            end_ts: Some(now),
            repo: place.clone(),
            branch: String::new(),
            kind: ActivityKind::Cwd,
            ext_id: Some(format!("cwd:{pid}:{place}")),
            summary: None,
            detail: Some(serde_json::json!({ "path": cwd }).to_string()),
        };
        let _ = tx.send(CaptureEvent::Activity(event));
    }
}

impl FocusProvider for MacFocusProvider {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        let workspace = NSWorkspace::sharedWorkspace();
        let mut state = State {
            last_pid: None,
            last_app: String::new(),
            last_title: String::new(),
            trusted: ax::trusted(false),
            trust_checked: Instant::now(),
            warned: false,
            cwd_seen: HashMap::new(),
        };
        if !state.trusted {
            tracing::warn!("Accessibility not granted: window titles unavailable");
            state.warned = true;
        }
        loop {
            if state.trust_checked.elapsed() >= TRUST_RECHECK {
                state.trusted = ax::trusted(false);
                state.trust_checked = Instant::now();
                if state.trusted {
                    state.warned = false;
                } else if !state.warned {
                    tracing::warn!("Accessibility not granted: window titles unavailable");
                    state.warned = true;
                }
            }

            let app = workspace.frontmostApplication();
            let (name, pid) = match app {
                Some(app) => {
                    let name = app
                        .localizedName()
                        .map(|s| s.to_string())
                        .or_else(|| app.bundleIdentifier().map(|s| s.to_string()))
                        .unwrap_or_else(|| "unknown".to_string());
                    let pid = app.processIdentifier();
                    (name, if pid >= 0 { Some(pid) } else { None })
                }
                None => ("unknown".to_string(), None),
            };
            let title = if state.trusted {
                pid.map(window_title).unwrap_or_default()
            } else {
                String::new()
            };
            let u32_pid = pid.map(|p| p as u32);

            if pid != state.last_pid {
                state.last_pid = pid;
                state.last_app = name.clone();
                state.last_title = title.clone();
                let event = FocusEvent {
                    ts: Timestamp::now(),
                    app: name.clone(),
                    title,
                    pid: u32_pid,
                };
                if tx.send(CaptureEvent::Focus(event)).is_err() {
                    return Err("event channel closed".into());
                }
                state.probe_cwd(&name, u32_pid, &tx);
            } else if title != state.last_title {
                state.last_title = title.clone();
                let event = FocusEvent {
                    ts: Timestamp::now(),
                    app: state.last_app.clone(),
                    title,
                    pid: u32_pid,
                };
                if tx.send(CaptureEvent::TitleChanged(event)).is_err() {
                    return Err("event channel closed".into());
                }
                state.probe_cwd(&state.last_app.clone(), u32_pid, &tx);
            }

            std::thread::sleep(POLL);
        }
    }
}
