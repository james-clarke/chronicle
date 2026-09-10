//! Screen lock edges from the session dictionary (m38): polled rather than
//! the distributed `com.apple.screenIsLocked` notification, which would
//! need a run loop on its own thread. `LockSignal` fires on the initial
//! state and every edge, same contract as logind's push-based version.

use std::ptr::NonNull;
use std::time::Duration;

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};

use crate::macos::ffi;
use crate::{BoxError, LockSignal};

const POLL: Duration = Duration::from_secs(2);

pub struct MacLock;

impl MacLock {
    pub fn new() -> Result<Self, BoxError> {
        Ok(Self)
    }

    /// Absent dictionary (no session, e.g. fast user switching) or absent
    /// key both read as unlocked, matching what the key's own absence means
    /// on a normal unlocked session.
    fn locked() -> bool {
        // SAFETY: `CGSessionCopyCurrentDictionary` follows the Copy rule;
        // null is a documented "no session" result, not an error.
        let ptr = unsafe { ffi::CGSessionCopyCurrentDictionary() };
        let Some(ptr) = NonNull::new(ptr) else {
            return false;
        };
        // SAFETY: non-null Copy-rule result from the call above.
        let dict: CFRetained<CFDictionary<CFString, CFType>> = unsafe { CFRetained::from_raw(ptr) };
        let key = CFString::from_str("CGSSessionScreenIsLocked");
        dict.get(&key)
            .and_then(|value| value.downcast_ref::<CFBoolean>().map(CFBoolean::value))
            .unwrap_or(false)
    }
}

impl LockSignal for MacLock {
    fn run(self, on_change: &mut dyn FnMut(bool)) -> Result<(), BoxError> {
        let mut locked = Self::locked();
        on_change(locked);
        loop {
            std::thread::sleep(POLL);
            let now = Self::locked();
            if now != locked {
                locked = now;
                on_change(locked);
            }
        }
    }
}

/// Whether this process runs inside a GUI login session: the session
/// dictionary is null over ssh and under a LaunchDaemon, where AppKit
/// (the menu-bar icon) must not be started.
pub fn gui_session() -> bool {
    // SAFETY: Copy rule; null is the documented "no session" result.
    let ptr = unsafe { ffi::CGSessionCopyCurrentDictionary() };
    match NonNull::new(ptr) {
        Some(ptr) => {
            // SAFETY: non-null Copy-rule result, released on drop.
            let _dict: CFRetained<CFDictionary<CFString, CFType>> =
                unsafe { CFRetained::from_raw(ptr) };
            true
        }
        None => false,
    }
}
