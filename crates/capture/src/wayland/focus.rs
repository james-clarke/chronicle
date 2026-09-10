//! The focus state machine both Wayland routes drive (m39). Raw window
//! bookkeeping is protocol-specific and stays in `wlr` and `kwin`; what is
//! shared is the title debounce, the `FocusEvent` shape and the terminal
//! cwd probe, all of which have to behave as the X11 provider's do.

use std::time::{Duration, Instant};

use chronicle_core::types::{CaptureEvent, FocusEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::BoxError;
use crate::cwd::CwdProbe;

/// Trailing-edge debounce, the same 1 s the X11 provider applies: a shell
/// prompt or a browser tab rewrites its title in bursts.
pub const TITLE_DEBOUNCE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct FocusState {
    app: String,
    title: String,
    /// The focused window's pid where the route can name it. A window keeps
    /// its pid, so title changes reuse this rather than asking again.
    pid: Option<u32>,
    focused: bool,
    /// A title change waiting for the burst to settle.
    pending: Option<(String, Instant)>,
    cwd: CwdProbe,
}

impl FocusState {
    pub fn new() -> Self {
        Self::default()
    }

    /// The focused window changed. Always emits: the routes dedupe by window
    /// handle before calling, as the X11 provider does.
    pub fn focus(
        &mut self,
        app: String,
        title: String,
        pid: Option<u32>,
        tx: &Sender<CaptureEvent>,
    ) -> Result<(), BoxError> {
        self.app = app;
        self.title = title;
        self.pid = pid;
        self.focused = true;
        self.pending = None;
        let event = FocusEvent {
            ts: Timestamp::now(),
            app: self.app.clone(),
            title: self.title.clone(),
            pid,
        };
        if tx.send(CaptureEvent::Focus(event)).is_err() {
            return Err("event channel closed".into());
        }
        let app = self.app.clone();
        self.cwd.probe(&app, pid, tx);
        Ok(())
    }

    /// Nothing is focused any more. The X11 provider emits nothing when
    /// `_NET_ACTIVE_WINDOW` goes to 0 either: the open span ends at idle or
    /// lock, not at an empty desktop.
    pub fn blur(&mut self) {
        self.focused = false;
        self.pending = None;
    }

    /// The focused window's title changed. Held until `flush` sees it settle.
    pub fn title(&mut self, title: String, now: Instant) {
        if !self.focused {
            return;
        }
        self.pending = Some((title, now));
    }

    /// How long the caller should wait before the next `flush` has anything
    /// to do; `None` when no title is pending.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        let (_, at) = self.pending.as_ref()?;
        Some(TITLE_DEBOUNCE.saturating_sub(now.saturating_duration_since(*at)))
    }

    /// Emit a settled title change, if one has settled.
    pub fn flush(&mut self, now: Instant, tx: &Sender<CaptureEvent>) -> Result<(), BoxError> {
        let Some((title, at)) = self.pending.as_ref() else {
            return Ok(());
        };
        if now.saturating_duration_since(*at) < TITLE_DEBOUNCE {
            return Ok(());
        }
        let title = title.clone();
        self.pending = None;
        if title == self.title {
            return Ok(());
        }
        self.title = title;
        let event = FocusEvent {
            ts: Timestamp::now(),
            app: self.app.clone(),
            title: self.title.clone(),
            pid: self.pid,
        };
        if tx.send(CaptureEvent::TitleChanged(event)).is_err() {
            return Err("event channel closed".into());
        }
        // A shell that moved prints a new prompt title.
        let app = self.app.clone();
        self.cwd.probe(&app, self.pid, tx);
        Ok(())
    }

    /// The app the routes compare against when deciding whether a window's
    /// identity changed under the same handle.
    pub fn current_app(&self) -> &str {
        &self.app
    }

    /// The title the routes compare against when deciding whether a change
    /// is worth debouncing: the pending one when there is one.
    pub fn current_title(&self) -> &str {
        match &self.pending {
            Some((title, _)) => title,
            None => &self.title,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(rx: &crossbeam_channel::Receiver<CaptureEvent>) -> Vec<(String, String, String)> {
        rx.try_iter()
            .filter_map(|event| match event {
                CaptureEvent::Focus(f) => Some(("focus".into(), f.app, f.title)),
                CaptureEvent::TitleChanged(f) => Some(("title".into(), f.app, f.title)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_focus_change_emits_at_once() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut state = FocusState::new();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        assert_eq!(
            drain(&rx),
            vec![("focus".into(), "foot".into(), "zsh".into())]
        );
    }

    #[test]
    fn a_title_burst_collapses_to_one_event() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut state = FocusState::new();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        let _ = drain(&rx);

        state.title("cargo".into(), now);
        state.title("cargo test".into(), now + Duration::from_millis(300));
        state.title("cargo test -p x".into(), now + Duration::from_millis(600));
        state.flush(now + Duration::from_millis(900), &tx).unwrap();
        assert!(drain(&rx).is_empty(), "still inside the debounce");

        state.flush(now + Duration::from_millis(1700), &tx).unwrap();
        assert_eq!(
            drain(&rx),
            vec![("title".into(), "foot".into(), "cargo test -p x".into())]
        );
    }

    #[test]
    fn a_title_that_settles_back_emits_nothing() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut state = FocusState::new();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        let _ = drain(&rx);

        state.title("cargo".into(), now);
        state.title("zsh".into(), now + Duration::from_millis(200));
        state.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        assert!(drain(&rx).is_empty());
    }

    #[test]
    fn a_focus_change_drops_the_pending_title_of_the_old_window() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut state = FocusState::new();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        state.title("cargo".into(), now);
        state
            .focus("firefox".into(), "docs".into(), None, &tx)
            .unwrap();
        state.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        assert_eq!(
            drain(&rx),
            vec![
                ("focus".into(), "foot".into(), "zsh".into()),
                ("focus".into(), "firefox".into(), "docs".into()),
            ]
        );
    }

    #[test]
    fn a_title_change_while_nothing_is_focused_is_dropped() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut state = FocusState::new();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        state.blur();
        state.title("cargo".into(), now);
        state.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        assert_eq!(
            drain(&rx),
            vec![("focus".into(), "foot".into(), "zsh".into())]
        );
    }

    #[test]
    fn wait_counts_down_and_is_none_when_idle() {
        let now = Instant::now();
        let mut state = FocusState::new();
        assert_eq!(state.wait(now), None);
        let (tx, _rx) = crossbeam_channel::unbounded();
        state.focus("foot".into(), "zsh".into(), None, &tx).unwrap();
        state.title("cargo".into(), now);
        assert_eq!(state.wait(now), Some(TITLE_DEBOUNCE));
        assert_eq!(
            state.wait(now + Duration::from_millis(400)),
            Some(Duration::from_millis(600))
        );
        assert_eq!(state.wait(now + TITLE_DEBOUNCE * 3), Some(Duration::ZERO));
    }

    #[test]
    fn the_focused_window_keeps_its_pid_across_a_title_change() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut state = FocusState::new();
        state
            .focus("foot".into(), "zsh".into(), Some(4242), &tx)
            .unwrap();
        state.title("cargo".into(), now);
        state.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        let pids: Vec<Option<u32>> = rx
            .try_iter()
            .filter_map(|event| match event {
                CaptureEvent::Focus(f) | CaptureEvent::TitleChanged(f) => Some(f.pid),
                _ => None,
            })
            .collect();
        assert_eq!(pids, vec![Some(4242), Some(4242)]);
    }
}
