//! The wlroots focus route (m39): `zwlr_foreign_toplevel_management_v1`,
//! which Sway, Hyprland, Niri, river and labwc all speak. The compositor
//! pushes one handle per window with its app_id, title and states, so the
//! provider is event-driven like the X11 one rather than polled.
//!
//! The protocol carries no pid. The compositor's own IPC does, and `ipc`
//! asks it; without one the focus rows have no pid and the terminal cwd
//! probe stays quiet, leaving placement to the shell hook.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use chronicle_core::types::CaptureEvent;
use crossbeam_channel::Sender;
use wayland_client::backend::{ObjectId, WaylandError};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_handle_v1::{
    Event as HandleEvent, State as HandleState, ZwlrForeignToplevelHandleV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::zwlr_foreign_toplevel_manager_v1::{
    EVT_TOPLEVEL_OPCODE, Event as ManagerEvent, ZwlrForeignToplevelManagerV1,
};

use super::focus::FocusState;
use crate::{BoxError, FocusProvider};

const TOPLEVEL_INTERFACE: &str = "zwlr_foreign_toplevel_manager_v1";

pub struct WlrFocusProvider {
    conn: Connection,
}

impl WlrFocusProvider {
    /// Fails when the compositor advertises no toplevel manager, which is
    /// the case the route probe has to answer: GNOME and KDE both land here.
    pub fn new() -> Result<Self, BoxError> {
        let conn = Connection::connect_to_env()?;
        let (globals, _queue) = registry_queue_init::<Probe>(&conn)?;
        if !has_toplevel_manager(&globals) {
            return Err(super::unsupported(
                &std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            ));
        }
        Ok(Self { conn })
    }
}

fn has_toplevel_manager(globals: &GlobalList) -> bool {
    globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == TOPLEVEL_INTERFACE))
}

/// The registry-only state the protocol probe in `new` dispatches with.
struct Probe;

/// Globals arriving after `registry_queue_init` change nothing: the
/// toplevel manager is bound once, at start, or the route is not this one.
macro_rules! ignore_registry {
    ($state:ty) => {
        impl Dispatch<WlRegistry, GlobalListContents> for $state {
            fn event(
                _: &mut Self,
                _: &WlRegistry,
                _: <WlRegistry as Proxy>::Event,
                _: &GlobalListContents,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

ignore_registry!(Probe);
ignore_registry!(Wlr);

/// One toplevel's committed state. The protocol sends app_id, title and
/// states as separate events and `done` to mark the set complete, so focus
/// is only re-read on `done`.
#[derive(Default, Clone)]
struct Toplevel {
    app_id: String,
    title: String,
    activated: bool,
}

struct Wlr {
    tx: Sender<CaptureEvent>,
    tops: HashMap<ObjectId, Toplevel>,
    active: Option<ObjectId>,
    focus: FocusState,
    /// `Dispatch::event` cannot fail, so a closed channel or a compositor
    /// that stopped the protocol is stashed and raised by the run loop.
    err: Option<String>,
}

impl Wlr {
    fn new(tx: Sender<CaptureEvent>) -> Self {
        Self {
            tx,
            tops: HashMap::new(),
            active: None,
            focus: FocusState::new(),
            err: None,
        }
    }

    /// Re-read focus after one toplevel's event set completed. `id` is that
    /// toplevel: preferring it when it is activated settles the ordering
    /// between the old window's deactivation and the new one's activation,
    /// which the protocol does not guarantee.
    fn settle(&mut self, id: &ObjectId) {
        let now = Instant::now();
        let just_activated = self
            .tops
            .get(id)
            .filter(|top| top.activated)
            .map(|_| id.clone());
        let active = just_activated.or_else(|| {
            self.tops
                .iter()
                .find(|(_, top)| top.activated)
                .map(|(id, _)| id.clone())
        });
        let Some(active) = active else {
            self.active = None;
            self.focus.blur();
            return;
        };
        let Some(top) = self.tops.get(&active).cloned() else {
            return;
        };
        if self.active.as_ref() == Some(&active) && top.app_id == self.focus.current_app() {
            if top.title != self.focus.current_title() {
                self.focus.title(top.title, now);
            }
            return;
        }
        self.active = Some(active);
        if let Err(e) = self.focus.focus(top.app_id, top.title, None, &self.tx) {
            self.err = Some(e.to_string());
        }
    }

    fn flush(&mut self, now: Instant) -> Result<(), BoxError> {
        if let Some(e) = self.err.take() {
            return Err(e.into());
        }
        self.focus.flush(now, &self.tx)
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Wlr {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: ManagerEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ManagerEvent::Toplevel { toplevel } => {
                state.tops.insert(toplevel.id(), Toplevel::default());
            }
            // The compositor stopped serving the protocol; nothing watches
            // focus any more, so the thread exits and the span closes.
            ManagerEvent::Finished => {
                state.err = Some("the compositor stopped the toplevel protocol".into());
            }
            _ => {}
        }
    }

    // The manager's `toplevel` event creates the handle object, so the
    // queue needs to be told what interface it carries.
    event_created_child!(Wlr, ZwlrForeignToplevelManagerV1, [
        EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for Wlr {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: HandleEvent,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            HandleEvent::AppId { app_id } => {
                state.tops.entry(id).or_default().app_id = app_id;
            }
            HandleEvent::Title { title } => {
                state.tops.entry(id).or_default().title = title;
            }
            HandleEvent::State { state: states } => {
                state.tops.entry(id).or_default().activated = is_activated(&states);
            }
            HandleEvent::Done => state.settle(&id),
            HandleEvent::Closed => {
                state.tops.remove(&id);
                if state.active.as_ref() == Some(&id) {
                    state.active = None;
                }
                state.settle(&id);
            }
            _ => {}
        }
    }
}

/// The `state` event carries a wl_array of `zwlr_foreign_toplevel_handle_v1`
/// state values, native-endian.
fn is_activated(states: &[u8]) -> bool {
    states
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| u32::from_ne_bytes(*chunk))
        .any(|value| value == HandleState::Activated as u32)
}

impl FocusProvider for WlrFocusProvider {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        let (globals, mut queue) = registry_queue_init::<Wlr>(&self.conn)?;
        let handle = queue.handle();
        let _manager: ZwlrForeignToplevelManagerV1 = globals
            .bind(&handle, 1..=3, ())
            .map_err(|e| -> BoxError { format!("{TOPLEVEL_INTERFACE}: {e}").into() })?;
        let mut wlr = Wlr::new(tx);
        // The compositor replays every open window before the first idle.
        queue.roundtrip(&mut wlr)?;
        loop {
            wlr.flush(Instant::now())?;
            let wait = wlr.focus.wait(Instant::now());
            dispatch(&mut queue, &mut wlr, wait)?;
        }
    }
}

/// One dispatch pass, waiting at most `wait` so a pending title change is
/// emitted once its burst settles.
fn dispatch(
    queue: &mut EventQueue<Wlr>,
    wlr: &mut Wlr,
    wait: Option<Duration>,
) -> Result<(), BoxError> {
    queue.dispatch_pending(wlr)?;
    let Some(guard) = queue.prepare_read() else {
        return Ok(());
    };
    queue.flush()?;
    let fd = guard.connection_fd().as_raw_fd();
    let timeout = match wait {
        Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
        None => -1,
    };
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd, a count that matches, and a fd the
    // guard keeps alive for the call.
    let ready = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
    if ready < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == ErrorKind::Interrupted {
            return Ok(());
        }
        return Err(e.into());
    }
    if ready == 0 {
        return Ok(());
    }
    match guard.read() {
        Ok(_) => {}
        // Another queue drained the socket first.
        Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
        Err(e) => return Err(e.into()),
    }
    queue.dispatch_pending(wlr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activated_is_read_out_of_the_state_array() {
        let activated = (HandleState::Activated as u32).to_ne_bytes();
        let maximized = (HandleState::Maximized as u32).to_ne_bytes();
        assert!(is_activated(&activated));
        assert!(!is_activated(&maximized));
        let both: Vec<u8> = maximized.iter().chain(activated.iter()).copied().collect();
        assert!(is_activated(&both));
        assert!(!is_activated(&[]));
        // A trailing partial value never makes a window look focused.
        assert!(!is_activated(&[2, 0, 0]));
    }
}
