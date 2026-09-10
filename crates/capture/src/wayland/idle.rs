//! Wayland idle (m39). `AfkProvider::idle_ms` is a poll (every 30 s from
//! `afk_loop`) but the protocols are edge-driven: a notification with a
//! short timeout reports `idled` once input has stopped for that long and
//! `resumed` when it comes back. Holding the last `idled` instant turns
//! those edges back into a duration, at 1 s resolution against a threshold
//! measured in minutes.
//!
//! `ext-idle-notify-v1` is the protocol every current compositor speaks;
//! `org_kde_kwin_idle` is the same two events under an older name, kept for
//! wlroots before 0.16 and KWin before 5.27.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use wayland_client::globals::{GlobalList, registry_queue_init};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::{
    Event as ExtEvent, ExtIdleNotificationV1,
};
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;
use wayland_protocols_plasma::idle::client::org_kde_kwin_idle::OrgKdeKwinIdle;
use wayland_protocols_plasma::idle::client::org_kde_kwin_idle_timeout::{
    Event as KdeEvent, OrgKdeKwinIdleTimeout,
};

use crate::{AfkProvider, BoxError};

/// The notification's timeout, and so the floor on every non-zero answer.
const TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Idle {
    /// When the compositor said input had been idle for `TIMEOUT`.
    idled_at: Option<Instant>,
}

ignore_registry!(Idle);
ignore_events!(Idle, WlSeat);
ignore_events!(Idle, ExtIdleNotifierV1);
ignore_events!(Idle, OrgKdeKwinIdle);

impl Dispatch<ExtIdleNotificationV1, ()> for Idle {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ExtEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ExtEvent::Idled => state.idled_at = Some(Instant::now()),
            ExtEvent::Resumed => state.idled_at = None,
            _ => {}
        }
    }
}

impl Dispatch<OrgKdeKwinIdleTimeout, ()> for Idle {
    fn event(
        state: &mut Self,
        _: &OrgKdeKwinIdleTimeout,
        event: KdeEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            KdeEvent::Idle => state.idled_at = Some(Instant::now()),
            KdeEvent::Resumed => state.idled_at = None,
            _ => {}
        }
    }
}

/// Kept alive for the life of the provider: dropping either object cancels
/// the subscription.
#[allow(dead_code)]
enum Notification {
    Ext(ExtIdleNotificationV1),
    Kde(OrgKdeKwinIdleTimeout),
}

struct Inner {
    queue: EventQueue<Idle>,
    state: Idle,
    _notification: Notification,
}

pub struct WaylandAfkProvider {
    /// `idle_ms` takes `&self` but has to dispatch; the provider runs on the
    /// one AFK thread, so a `RefCell` is enough.
    inner: RefCell<Inner>,
}

impl WaylandAfkProvider {
    /// Fails when the compositor advertises neither idle protocol, which is
    /// how the X11 provider behaves without MIT-SCREEN-SAVER: the daemon
    /// runs on without AFK rather than not at all.
    pub fn new() -> Result<Self, BoxError> {
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Idle>(&conn)?;
        let handle = queue.handle();
        let seat: WlSeat = globals
            .bind(&handle, 1..=9, ())
            .map_err(|e| -> BoxError { format!("wl_seat: {e}").into() })?;
        let timeout = u32::try_from(TIMEOUT.as_millis()).unwrap_or(1000);
        let notification = if has(&globals, ExtIdleNotifierV1::interface().name) {
            let notifier: ExtIdleNotifierV1 = globals
                .bind(&handle, 1..=2, ())
                .map_err(|e| -> BoxError { format!("ext_idle_notifier_v1: {e}").into() })?;
            Notification::Ext(notifier.get_idle_notification(timeout, &seat, &handle, ()))
        } else if has(&globals, OrgKdeKwinIdle::interface().name) {
            let notifier: OrgKdeKwinIdle = globals
                .bind(&handle, 1..=1, ())
                .map_err(|e| -> BoxError { format!("org_kde_kwin_idle: {e}").into() })?;
            Notification::Kde(notifier.get_idle_timeout(&seat, timeout, &handle, ()))
        } else {
            return Err("the compositor supports neither ext-idle-notify-v1 nor \
                        org_kde_kwin_idle, so Chronicle cannot tell idle from working"
                .into());
        };
        let mut state = Idle::default();
        queue.roundtrip(&mut state)?;
        Ok(Self {
            inner: RefCell::new(Inner {
                queue,
                state,
                _notification: notification,
            }),
        })
    }
}

fn has(globals: &GlobalList, interface: &str) -> bool {
    globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == interface))
}

impl AfkProvider for WaylandAfkProvider {
    fn idle_ms(&self) -> Result<u64, BoxError> {
        let mut inner = self.inner.borrow_mut();
        let Inner { queue, state, .. } = &mut *inner;
        // Drain what has arrived since the last poll without waiting: an
        // idled/resumed pair per typing pause, so there is rarely anything.
        super::dispatch_until(queue, state, Some(Duration::ZERO))?;
        Ok(idle_ms_since(state.idled_at, Instant::now()))
    }
}

/// The compositor reports `idled` once input has stopped for `TIMEOUT`, so
/// the answer is that timeout plus however long ago that was.
fn idle_ms_since(idled_at: Option<Instant>, now: Instant) -> u64 {
    let Some(at) = idled_at else { return 0 };
    let since = now.saturating_duration_since(at);
    u64::try_from((TIMEOUT + since).as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_reads_as_zero() {
        assert_eq!(idle_ms_since(None, Instant::now()), 0);
    }

    #[test]
    fn the_answer_starts_at_the_notification_timeout() {
        let now = Instant::now();
        assert_eq!(idle_ms_since(Some(now), now), 1000);
    }

    #[test]
    fn the_answer_grows_with_the_wait() {
        let now = Instant::now();
        let idled = now - Duration::from_secs(119);
        assert_eq!(idle_ms_since(Some(idled), now), 120_000);
    }

    #[test]
    fn a_clock_that_went_backwards_still_reads_as_idle() {
        let now = Instant::now();
        assert_eq!(idle_ms_since(Some(now + Duration::from_secs(5)), now), 1000);
    }
}
