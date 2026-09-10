//! Idle time and per-minute input counts from CoreGraphics event source
//! counters (m38): the same two signals X11 gets from XScreenSaver and
//! XInput 2 raw events, polled instead of pushed since CoreGraphics has no
//! equivalent event stream worth a run loop.

use std::time::Duration;

use chronicle_core::types::PresenceMinute;
use jiff::Timestamp;

use crate::macos::ffi;
use crate::{AfkProvider, BoxError, PresenceProvider};

const POLL: Duration = Duration::from_secs(1);

pub struct MacAfkProvider;

impl MacAfkProvider {
    pub fn new() -> Result<Self, BoxError> {
        Ok(Self)
    }
}

impl AfkProvider for MacAfkProvider {
    fn idle_ms(&self) -> Result<u64, BoxError> {
        // SAFETY: reads a global clock; both arguments are always-accepted
        // enum values, nothing is borrowed or allocated.
        let secs = unsafe {
            ffi::CGEventSourceSecondsSinceLastEventType(
                ffi::COMBINED_SESSION_STATE,
                ffi::ANY_INPUT_EVENT_TYPE,
            )
        };
        Ok((secs.max(0.0) * 1000.0) as u64)
    }
}

/// A snapshot of the four raw event counters CoreGraphics tracks, each
/// itself the wrapping sum of the underlying `CGEventType`s that make up
/// one presence dimension (m38 design: multiple button/motion event types
/// fold into one count). Summing before differencing is still exact across
/// a wrap of any individual counter, since `u32` wrapping subtraction is
/// modular.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counters {
    keys: u32,
    buttons: u32,
    motion: u32,
    scroll: u32,
}

impl Counters {
    fn read() -> Self {
        let counter = |event_type: u32| {
            // SAFETY: reads a global counter for a fixed, always-valid event type.
            unsafe {
                ffi::CGEventSourceCounterForEventType(ffi::COMBINED_SESSION_STATE, event_type)
            }
        };
        Self {
            keys: counter(ffi::EVENT_KEY_DOWN),
            buttons: counter(ffi::EVENT_LEFT_MOUSE_DOWN)
                .wrapping_add(counter(ffi::EVENT_RIGHT_MOUSE_DOWN))
                .wrapping_add(counter(ffi::EVENT_OTHER_MOUSE_DOWN)),
            motion: counter(ffi::EVENT_MOUSE_MOVED)
                .wrapping_add(counter(ffi::EVENT_LEFT_MOUSE_DRAGGED))
                .wrapping_add(counter(ffi::EVENT_RIGHT_MOUSE_DRAGGED)),
            scroll: counter(ffi::EVENT_SCROLL_WHEEL),
        }
    }
}

/// Adds the wrapping deltas between `prev` and `now` into `cur`'s counts.
/// Pure so it runs without CoreGraphics.
fn fold(prev: &Counters, now: &Counters, cur: &mut PresenceMinute) {
    cur.keys += now.keys.wrapping_sub(prev.keys);
    cur.buttons += now.buttons.wrapping_sub(prev.buttons);
    cur.motion += now.motion.wrapping_sub(prev.motion);
    cur.scroll += now.scroll.wrapping_sub(prev.scroll);
}

fn minute_of(ms: i64) -> i64 {
    ms - ms.rem_euclid(60_000)
}

pub struct MacPresenceProvider;

impl MacPresenceProvider {
    pub fn new() -> Result<Self, BoxError> {
        Ok(Self)
    }
}

impl PresenceProvider for MacPresenceProvider {
    fn run(self, on_minute: &mut dyn FnMut(PresenceMinute) -> bool) -> Result<(), BoxError> {
        let mut prev = Counters::read();
        let mut cur = PresenceMinute {
            minute_ts: minute_of(Timestamp::now().as_millisecond()),
            ..PresenceMinute::default()
        };
        loop {
            std::thread::sleep(POLL);
            let now = Counters::read();
            fold(&prev, &now, &mut cur);
            prev = now;
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_accumulates_plain_deltas() {
        let prev = Counters {
            keys: 10,
            buttons: 2,
            motion: 100,
            scroll: 0,
        };
        let now = Counters {
            keys: 15,
            buttons: 2,
            motion: 130,
            scroll: 4,
        };
        let mut cur = PresenceMinute::default();
        fold(&prev, &now, &mut cur);
        assert_eq!(cur.keys, 5);
        assert_eq!(cur.buttons, 0);
        assert_eq!(cur.motion, 30);
        assert_eq!(cur.scroll, 4);
    }

    #[test]
    fn fold_handles_wraparound() {
        let prev = Counters {
            keys: u32::MAX - 2,
            buttons: 0,
            motion: 0,
            scroll: 0,
        };
        let now = Counters {
            keys: 2,
            buttons: 0,
            motion: 0,
            scroll: 0,
        };
        let mut cur = PresenceMinute::default();
        fold(&prev, &now, &mut cur);
        // Wrapped past u32::MAX: -2 -> -1 -> 0 -> 1 -> 2 is 5 key events.
        assert_eq!(cur.keys, 5);
    }

    #[test]
    fn fold_is_additive_across_polls() {
        let a = Counters {
            keys: 0,
            buttons: 0,
            motion: 0,
            scroll: 0,
        };
        let b = Counters {
            keys: 3,
            buttons: 1,
            motion: 0,
            scroll: 0,
        };
        let c = Counters {
            keys: 7,
            buttons: 1,
            motion: 2,
            scroll: 0,
        };
        let mut cur = PresenceMinute::default();
        fold(&a, &b, &mut cur);
        fold(&b, &c, &mut cur);
        let mut direct = PresenceMinute::default();
        fold(&a, &c, &mut direct);
        assert_eq!(cur, direct);
    }
}
