//! CoreAudio mic-in-use probe (m38 chunk 2): whether the default input
//! device is running, via `kAudioDevicePropertyDeviceIsRunningSomewhere`.
//! A TCC-gated tap would be needed to name the capturing app, so the
//! span's app is just "microphone" — `MicProvider` (`crate::mic`) turns
//! this into the same call-span shape as `pw-dump` on Linux.

use std::ffi::c_void;
use std::mem;
use std::ptr;

use crate::mic::MicSource;

#[repr(C)]
struct AudioObjectPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

const K_AUDIO_OBJECT_SYSTEM_OBJECT: u32 = 1;
const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
const K_AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE: u32 = u32::from_be_bytes(*b"dIn ");
const K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_RUNNING_SOMEWHERE: u32 = u32::from_be_bytes(*b"goin");

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object_id: u32,
        address: *const AudioObjectPropertyAddress,
        qualifier_data_size: u32,
        qualifier_data: *const c_void,
        data_size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

pub struct CoreAudioMic;

impl CoreAudioMic {
    /// The default input device id, or an FFI error message carrying the
    /// `OSStatus`.
    fn default_input_device() -> Result<u32, String> {
        let address = AudioObjectPropertyAddress {
            m_selector: K_AUDIO_HARDWARE_PROPERTY_DEFAULT_INPUT_DEVICE,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        };
        let mut device_id: u32 = 0;
        let mut size = mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT,
                &address,
                0,
                ptr::null(),
                &mut size,
                (&mut device_id as *mut u32).cast::<c_void>(),
            )
        };
        if status != 0 {
            return Err(format!("default input device: OSStatus {status}"));
        }
        Ok(device_id)
    }

    /// Whether `device_id` is capturing right now, or an FFI error message
    /// carrying the `OSStatus`.
    fn is_running(device_id: u32) -> Result<bool, String> {
        let address = AudioObjectPropertyAddress {
            m_selector: K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_RUNNING_SOMEWHERE,
            m_scope: K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
            m_element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        };
        let mut running: u32 = 0;
        let mut size = mem::size_of::<u32>() as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                device_id,
                &address,
                0,
                ptr::null(),
                &mut size,
                (&mut running as *mut u32).cast::<c_void>(),
            )
        };
        if status != 0 {
            return Err(format!("device is running somewhere: OSStatus {status}"));
        }
        Ok(running != 0)
    }
}

impl MicSource for CoreAudioMic {
    fn active_inputs(&mut self) -> Result<Vec<String>, String> {
        let device = Self::default_input_device()?;
        // `kAudioObjectUnknown`: no input device at all, which is "no call",
        // not an error to warn about every poll.
        if device == 0 {
            return Ok(Vec::new());
        }
        if Self::is_running(device)? {
            Ok(vec!["microphone".to_owned()])
        } else {
            Ok(Vec::new())
        }
    }
}
