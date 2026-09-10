//! The focused terminal's working directory (m30), shared by every focus
//! provider that can name the focused window's pid: X11 (`_NET_WM_PID`),
//! KWin (the script reports it) and the wlroots route on the compositors
//! whose IPC answers it (m39).

use std::collections::HashMap;

use chronicle_core::extract::{self, Family};
use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

/// Working directory of the focused terminal: the newest child shell of the
/// terminal process (the tab opened last, exact for a single tab), else the
/// terminal's own cwd, both read from `/proc`.
pub fn terminal_cwd(pid: u32) -> Option<String> {
    let mut newest: Option<(u64, u32)> = None;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(child) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid … starttime` — comm may hold spaces.
        let Some(rest) = stat.rsplit_once(')').map(|(_, r)| r) else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        let ppid = fields.get(1).and_then(|p| p.parse::<u32>().ok());
        if ppid != Some(pid) {
            continue;
        }
        let start = fields
            .get(19)
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(0);
        if newest.is_none_or(|(s, _)| start >= s) {
            newest = Some((start, child));
        }
    }
    let target = newest.map(|(_, c)| c).unwrap_or(pid);
    std::fs::read_link(format!("/proc/{target}/cwd"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// One `(terminal pid, place)` row per place a focused terminal has sat in,
/// refreshed on every probe so the row's `end_ts` follows the focus.
#[derive(Default)]
pub struct CwdProbe {
    seen: HashMap<(u32, String), Timestamp>,
}

impl CwdProbe {
    /// One cwd probe for a terminal window: emits (or refreshes) the
    /// `(pid, place)` row when the shell sits in a named place.
    pub fn probe(&mut self, app: &str, pid: Option<u32>, tx: &Sender<CaptureEvent>) {
        let Some(pid) = pid else { return };
        if extract::family(app) != Family::Terminal {
            return;
        }
        let Some(cwd) = terminal_cwd(pid) else { return };
        let Some(place) = extract::place_from_path(&cwd) else {
            return;
        };
        let now = Timestamp::now();
        let first = *self.seen.entry((pid, place.clone())).or_insert(now);
        let event = ActivityEvent {
            ts: first,
            end_ts: Some(now),
            repo: place.clone(),
            branch: String::new(),
            kind: ActivityKind::Cwd,
            ext_id: Some(format!("cwd:{pid}:{place}")),
            summary: None,
            detail: Some(serde_json::json!({ "path": cwd }).to_string()),
        };
        let _ = tx.send(CaptureEvent::Activity(event));
    }
}
