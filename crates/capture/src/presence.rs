//! Per-minute input counts from XI2 raw events (m32 chunk 1). Raw events
//! carry device keycodes and button numbers; only the counts leave this
//! module. The idle clock stays with the AFK poller (XScreenSaver).

use std::time::Duration;

use chronicle_core::types::PresenceMinute;
use jiff::Timestamp;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xinput::{ConnectionExt as _, EventMask, XIEventMask};
use x11rb::rust_connection::RustConnection;

use crate::{BoxError, PresenceProvider};

/// XIAllDevices: raw events come from the physical (slave) devices.
const XI_ALL_DEVICES: u16 = 0;
/// Motion arrives in bursts of hundreds per second; draining every quarter
/// second keeps the socket buffer small without a wake-up per event.
const POLL: Duration = Duration::from_millis(250);

pub struct X11PresenceProvider {
    conn: RustConnection,
}

impl X11PresenceProvider {
    /// Fails without XInput 2 (Xvfb without the extension, a nested server).
    pub fn new() -> Result<Self, BoxError> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let version = conn.xinput_xi_query_version(2, 2)?.reply()?;
        if version.major_version < 2 {
            return Err(format!(
                "XInput {}.{} < 2.0",
                version.major_version, version.minor_version
            )
            .into());
        }
        conn.xinput_xi_select_events(
            root,
            &[EventMask {
                deviceid: XI_ALL_DEVICES,
                mask: vec![
                    XIEventMask::RAW_KEY_PRESS
                        | XIEventMask::RAW_BUTTON_PRESS
                        | XIEventMask::RAW_MOTION,
                ],
            }],
        )?
        .check()?;
        Ok(Self { conn })
    }
}

fn minute_of(ms: i64) -> i64 {
    ms - ms.rem_euclid(60_000)
}

impl PresenceProvider for X11PresenceProvider {
    fn run(self, on_minute: &mut dyn FnMut(PresenceMinute) -> bool) -> Result<(), BoxError> {
        let mut cur = PresenceMinute {
            minute_ts: minute_of(Timestamp::now().as_millisecond()),
            ..PresenceMinute::default()
        };
        loop {
            while let Some(event) = self.conn.poll_for_event()? {
                match event {
                    Event::XinputRawKeyPress(_) => cur.keys += 1,
                    // Wheel clicks are buttons 4–7. Smooth scrolling also
                    // reports a scroll-axis RawMotion, so a wheel turn bumps
                    // motion too; counts are a signal, not a measurement.
                    Event::XinputRawButtonPress(e) if (4..=7).contains(&e.detail) => {
                        cur.scroll += 1;
                    }
                    Event::XinputRawButtonPress(_) => cur.buttons += 1,
                    Event::XinputRawMotion(_) => cur.motion += 1,
                    _ => {}
                }
            }
            let minute = minute_of(Timestamp::now().as_millisecond());
            if minute != cur.minute_ts {
                let had_input = cur.keys + cur.buttons + cur.motion + cur.scroll > 0;
                if had_input && !on_minute(cur) {
                    return Ok(());
                }
                cur = PresenceMinute {
                    minute_ts: minute,
                    ..PresenceMinute::default()
                };
            }
            std::thread::sleep(POLL);
        }
    }
}
