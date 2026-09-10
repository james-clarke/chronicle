//! Hand-written bindings for the four C frameworks nothing else wraps:
//! Accessibility (`AX*`, focus.rs), CoreGraphics event source counters
//! (input.rs) and the session dictionary (lock.rs). `objc2-app-kit`
//! already covers `NSWorkspace`; adding a bindings crate for four
//! functions would only add build time.

use objc2_core_foundation::{CFDictionary, CFString, CFType};

/// `AXError`: `kAXErrorSuccess` is 0; every other value is a failure this
/// crate treats the same way (empty title, nothing crashes).
pub type AXError = i32;

/// `AXUIElementRef` is `typedef CFTypeRef AXUIElementRef` in the real
/// header, i.e. any AX object is toll-free bridged to `CFType` and follows
/// the normal CF retain/release rules.
pub type AXUIElementRef = *const CFType;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    /// Create rule: the caller owns the returned reference.
    pub fn AXUIElementCreateApplication(pid: libc::pid_t) -> AXUIElementRef;

    /// Copy rule: `*value` is a new owned reference on `kAXErrorSuccess`,
    /// untouched otherwise.
    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: &CFString,
        value: *mut *const CFType,
    ) -> AXError;

    /// `options` may be omitted (equivalent to plain `AXIsProcessTrusted`);
    /// `Some` with `kAXTrustedCheckOptionPrompt` set to `true` shows the
    /// system consent alert.
    /// Returns a CF `Boolean` (`unsigned char`), never a C `_Bool`: compare
    /// against 0 rather than declaring `bool`.
    pub fn AXIsProcessTrustedWithOptions(options: Option<&CFDictionary>) -> u8;

    /// Cap on how long a synchronous AX request waits on the target app
    /// (the default is several seconds; a hung app would stall the poll).
    pub fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_secs: f32) -> AXError;

    /// The one key `AXIsProcessTrustedWithOptions` understands.
    pub static kAXTrustedCheckOptionPrompt: &'static CFString;
}

/// `kCGEventSourceStateCombinedSessionState`: HID-level and session-local
/// events both count, which is what "is the user here" needs.
pub const COMBINED_SESSION_STATE: i32 = 0;
/// `kCGAnyInputEventType`.
pub const ANY_INPUT_EVENT_TYPE: u32 = !0;

pub const EVENT_KEY_DOWN: u32 = 10;
pub const EVENT_LEFT_MOUSE_DOWN: u32 = 1;
pub const EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
pub const EVENT_OTHER_MOUSE_DOWN: u32 = 25;
pub const EVENT_MOUSE_MOVED: u32 = 5;
pub const EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
pub const EVENT_RIGHT_MOUSE_DRAGGED: u32 = 7;
pub const EVENT_OTHER_MOUSE_DRAGGED: u32 = 27;
pub const EVENT_SCROLL_WHEEL: u32 = 22;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// Seconds since the last input event of `event_type` on `state_id`.
    pub fn CGEventSourceSecondsSinceLastEventType(state_id: i32, event_type: u32) -> f64;

    /// A per-event-type counter that increments for the session and
    /// restarts at 0 with it; only deltas between two reads are meaningful
    /// (input.rs folds them).
    pub fn CGEventSourceCounterForEventType(state_id: i32, event_type: u32) -> u32;

    /// Null when there is no session (e.g. fast user switching); the
    /// caller owns the returned dictionary (Copy rule) when non-null.
    pub fn CGSessionCopyCurrentDictionary() -> *mut CFDictionary<CFString, CFType>;
}
