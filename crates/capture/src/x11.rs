//! X11 capture; the Wayland routes are in `wayland` (m39). Windows die
//! racily, so every property read tolerates BadWindow/BadDrawable as
//! non-fatal.

use std::thread;
use std::time::Duration;

use chronicle_core::types::{CaptureEvent, FocusEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::screensaver::ConnectionExt as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt, EventMask, Window,
};
use x11rb::rust_connection::RustConnection;

use crate::cwd::CwdProbe;
use crate::{AfkProvider, BoxError, FocusProvider};

x11rb::atom_manager! {
    Atoms:
    AtomsCookie {
        _NET_ACTIVE_WINDOW,
        _NET_WM_NAME,
        _NET_WM_PID,
        UTF8_STRING,
    }
}

const TITLE_DEBOUNCE: Duration = Duration::from_secs(1);

pub struct X11FocusProvider {
    conn: RustConnection,
    root: Window,
    atoms: Atoms,
}

struct State {
    current: Option<Window>,
    last_app: String,
    last_title: String,
    cwd: CwdProbe,
}

impl X11FocusProvider {
    pub fn new() -> Result<Self, BoxError> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = Atoms::new(&conn)?.reply()?;
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?
        .check()?;
        Ok(Self { conn, root, atoms })
    }

    fn active_window(&self) -> Option<Window> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.atoms._NET_ACTIVE_WINDOW,
                AtomEnum::WINDOW,
                0,
                1,
            )
            .ok()?
            .reply()
            .ok()?;
        reply.value32()?.next().filter(|&w| w != 0)
    }

    fn title(&self, win: Window) -> String {
        let candidates: [(Atom, Atom); 2] = [
            (self.atoms._NET_WM_NAME, self.atoms.UTF8_STRING),
            (AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()),
        ];
        for (prop, ty) in candidates {
            let Ok(cookie) = self.conn.get_property(false, win, prop, ty, 0, u32::MAX) else {
                continue;
            };
            let Ok(reply) = cookie.reply() else { continue };
            if !reply.value.is_empty() {
                return String::from_utf8_lossy(&reply.value).into_owned();
            }
        }
        String::new()
    }

    /// WM_CLASS is "instance\0class\0"; the class half is the app name.
    fn app_name(&self, win: Window) -> String {
        let Ok(cookie) =
            self.conn
                .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
        else {
            return String::new();
        };
        let Ok(reply) = cookie.reply() else {
            return String::new();
        };
        let mut parts = reply.value.split(|&b| b == 0);
        let instance = parts.next();
        parts
            .next()
            .filter(|c| !c.is_empty())
            .or(instance)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .unwrap_or_default()
    }

    fn pid(&self, win: Window) -> Option<u32> {
        let reply = self
            .conn
            .get_property(false, win, self.atoms._NET_WM_PID, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        reply.value32()?.next()
    }

    fn is_title_atom(&self, atom: Atom) -> bool {
        atom == self.atoms._NET_WM_NAME || atom == Atom::from(AtomEnum::WM_NAME)
    }

    fn focus_changed(&self, tx: &Sender<CaptureEvent>, state: &mut State) -> Result<(), BoxError> {
        let win = self.active_window();
        if win == state.current {
            return Ok(());
        }
        // Move the PROPERTY_CHANGE subscription (tab-title changes) to the new
        // active window; both calls unchecked since either window may be gone.
        let unsubscribe = ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT);
        let subscribe = ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        if let Some(prev) = state.current {
            let _ = self.conn.change_window_attributes(prev, &unsubscribe);
        }
        state.current = win;
        let Some(win) = win else {
            self.conn.flush()?;
            return Ok(());
        };
        let _ = self.conn.change_window_attributes(win, &subscribe);
        self.conn.flush()?;
        let app = self.app_name(win);
        let title = self.title(win);
        state.last_app = app.clone();
        state.last_title = title.clone();
        let pid = self.pid(win);
        let event = FocusEvent {
            ts: Timestamp::now(),
            app,
            title,
            pid,
        };
        if tx.send(CaptureEvent::Focus(event)).is_err() {
            return Err("event channel closed".into());
        }
        state.cwd.probe(&state.last_app.clone(), pid, tx);
        Ok(())
    }

    fn handle(
        &self,
        event: Event,
        tx: &Sender<CaptureEvent>,
        state: &mut State,
    ) -> Result<(), BoxError> {
        let Event::PropertyNotify(e) = event else {
            return Ok(());
        };
        if e.window == self.root && e.atom == self.atoms._NET_ACTIVE_WINDOW {
            self.focus_changed(tx, state)?;
        } else if Some(e.window) == state.current && self.is_title_atom(e.atom) {
            // Trailing-edge debounce: let the burst settle (the X connection
            // queues events meanwhile), replay what queued, then read once.
            thread::sleep(TITLE_DEBOUNCE);
            while let Some(queued) = self.conn.poll_for_event()? {
                if let Event::PropertyNotify(q) = &queued
                    && Some(q.window) == state.current
                    && self.is_title_atom(q.atom)
                {
                    continue; // coalesced into the read below
                }
                self.handle(queued, tx, state)?;
            }
            let Some(win) = state.current else {
                return Ok(());
            };
            let title = self.title(win);
            if title != state.last_title {
                state.last_title = title.clone();
                let pid = self.pid(win);
                let event = FocusEvent {
                    ts: Timestamp::now(),
                    app: state.last_app.clone(),
                    title,
                    pid,
                };
                if tx.send(CaptureEvent::TitleChanged(event)).is_err() {
                    return Err("event channel closed".into());
                }
                // A shell that moved prints a new prompt title.
                state.cwd.probe(&state.last_app.clone(), pid, tx);
            }
        }
        Ok(())
    }
}

impl FocusProvider for X11FocusProvider {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        let mut state = State {
            current: None,
            last_app: String::new(),
            last_title: String::new(),
            cwd: CwdProbe::default(),
        };
        self.focus_changed(&tx, &mut state)?;
        loop {
            let event = self.conn.wait_for_event()?;
            self.handle(event, &tx, &mut state)?;
        }
    }
}

pub struct X11AfkProvider {
    conn: RustConnection,
    root: Window,
}

impl X11AfkProvider {
    pub fn new() -> Result<Self, BoxError> {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let this = Self { conn, root };
        this.idle_ms()?; // fail fast when MIT-SCREEN-SAVER is missing
        Ok(this)
    }
}

impl AfkProvider for X11AfkProvider {
    fn idle_ms(&self) -> Result<u64, BoxError> {
        let reply = self.conn.screensaver_query_info(self.root)?.reply()?;
        Ok(u64::from(reply.ms_since_user_input))
    }
}
