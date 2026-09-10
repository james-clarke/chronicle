//! tmux pane poller (m37 chunk 1): every 60 s, `tmux list-panes -a -F`
//! reports the active pane of every active window of every attached
//! session — a `cwd` event per pane, keyed by place (shell_hook.rs's
//! `place_of`), so a session someone is actually looking at anchors that
//! place the way a focused terminal's cwd does (x11.rs, ports.rs).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::shell_hook::place_of;
use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);
const FORMAT: &str =
    "#{pane_id}\t#{pane_current_path}\t#{pane_active}\t#{window_active}\t#{session_attached}";

pub struct TmuxProvider {
    tmux: PathBuf,
    /// First-seen time per `ext_id`, so a repeat upserts a longer span;
    /// replaced each poll so a pane not seen this round is dropped.
    seen: HashMap<String, Timestamp>,
    /// Last stderr line warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl TmuxProvider {
    /// `Some` when `tmux` resolves on `PATH` (a pure env lookup, no
    /// shelling out).
    pub fn detect() -> Option<Self> {
        let tmux = chronicle_core::config::resolve_command("tmux");
        if !tmux.contains('/') {
            return None;
        }
        Some(Self {
            tmux: PathBuf::from(tmux),
            seen: HashMap::new(),
            last_err: None,
        })
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let result = Command::new(&self.tmux)
            .args(["list-panes", "-a", "-F", FORMAT])
            .output();
        let out = match result {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or("tmux list-panes failed")
                    .to_owned();
                self.warn_once(err);
                self.seen.clear();
                return Vec::new();
            }
            Err(e) => {
                self.warn_once(e.to_string());
                self.seen.clear();
                return Vec::new();
            }
        };
        self.last_err = None;
        let now = Timestamp::now();
        let mut next_seen = HashMap::new();
        let mut events = Vec::new();
        for pane in parse_panes(&String::from_utf8_lossy(&out.stdout))
            .into_iter()
            .filter(Pane::is_focused)
        {
            let place = place_of(Path::new(&pane.cwd));
            let ext_id = format!("tmux:{}:{place}", pane.id);
            let first = self.seen.get(&ext_id).copied().unwrap_or(now);
            next_seen.insert(ext_id.clone(), first);
            events.push(ActivityEvent {
                ts: first,
                end_ts: Some(now),
                repo: place,
                branch: String::new(),
                kind: ActivityKind::Cwd,
                ext_id: Some(ext_id),
                summary: None,
                detail: None,
            });
        }
        self.seen = next_seen;
        events
    }

    fn warn_once(&mut self, err: String) {
        if self.last_err.as_deref() != Some(err.as_str()) {
            tracing::warn!("tmux list-panes: {err}");
            self.last_err = Some(err);
        }
    }
}

impl FocusProvider for TmuxProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pane {
    id: String,
    cwd: String,
    pane_active: bool,
    window_active: bool,
    session_attached: bool,
}

impl Pane {
    /// The pane someone is actually looking at: its own active pane, in the
    /// window on screen, in a session with a client attached.
    fn is_focused(&self) -> bool {
        self.pane_active && self.window_active && self.session_attached
    }
}

/// One `Pane` per line of `tmux list-panes -a -F` output ([`FORMAT`]).
/// Malformed lines (missing fields) are skipped.
fn parse_panes(text: &str) -> Vec<Pane> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            Some(Pane {
                id: (*f.first()?).to_owned(),
                cwd: (*f.get(1)?).to_owned(),
                pane_active: *f.get(2)? == "1",
                window_active: *f.get(3)? == "1",
                session_attached: *f.get(4)? != "0",
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "%1\t/home/x/dev/chronicle\t1\t1\t1\n\
%2\t/home/x/dev/chronicle/crates\t0\t1\t1\n\
%3\t/home/x/scratch\t1\t0\t1\n\
%4\t/home/x/other\t1\t1\t0\n";

    #[test]
    fn parses_every_pane_line() {
        let panes = parse_panes(FIXTURE);
        assert_eq!(panes.len(), 4);
        assert_eq!(panes[0].id, "%1");
        assert_eq!(panes[0].cwd, "/home/x/dev/chronicle");
        assert!(panes[0].pane_active && panes[0].window_active && panes[0].session_attached);
        assert!(!panes[1].pane_active, "the inactive pane in its window");
        assert!(!panes[2].window_active, "the pane's window isn't on screen");
        assert!(!panes[3].session_attached, "no client attached");
    }

    #[test]
    fn filters_to_the_one_focused_pane() {
        let panes = parse_panes(FIXTURE);
        let focused: Vec<&Pane> = panes.iter().filter(|p| p.is_focused()).collect();
        assert_eq!(focused.len(), 1);
        assert_eq!(focused[0].id, "%1");
    }
}
