//! Accessibility trust (m38): the one check the focus provider and the
//! onboarding card both need before window titles are readable.

use objc2_core_foundation::{CFDictionary, kCFBooleanFalse, kCFBooleanTrue};

use crate::macos::ffi;

/// `prompt = true` shows the system's Accessibility consent alert if the
/// user has not yet decided; `false` reads the current grant silently.
/// Every AX call elsewhere tolerates a `false` return (empty titles).
pub fn trusted(prompt: bool) -> bool {
    // SAFETY: `kCFBooleanTrue`/`kCFBooleanFalse` are non-null exported
    // constants; reading an extern static is always unsafe.
    let value = unsafe {
        if prompt {
            kCFBooleanTrue
        } else {
            kCFBooleanFalse
        }
    }
    .expect("kCFBooleanTrue/kCFBooleanFalse is never null");
    // SAFETY: `kAXTrustedCheckOptionPrompt` is a non-null exported constant;
    // reading an extern static is always unsafe.
    let key = unsafe { ffi::kAXTrustedCheckOptionPrompt };
    let options = CFDictionary::from_slices(&[key], &[value]);
    // SAFETY: `options` is a valid dictionary kept alive for the call.
    unsafe { ffi::AXIsProcessTrustedWithOptions(Some(options.as_opaque())) }
}
