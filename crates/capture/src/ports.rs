//! Listener map (m32 chunk 4): which repo owns each local dev server.
//! Every minute the TCP listeners in `/proc/net/tcp{,6}` are walked to the
//! process holding the socket and that process's working directory; a
//! listener whose cwd names a place is emitted as a `Cwd` row keyed
//! `port:<port>:<place>` with `port` in its detail, refreshed while it
//! lives. The anchors turn a browser span on `localhost:<port>` into that
//! place. Linux `/proc` only; elsewhere the provider emits nothing.

use std::collections::HashMap;
use std::time::Duration;

use chronicle_core::extract;
use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct PortMapProvider {
    /// First time each `(port, place)` was seen, for the upsert row's start.
    seen: HashMap<(u16, String), Timestamp>,
}

impl PortMapProvider {
    fn poll(&mut self) -> Vec<ActivityEvent> {
        let now = Timestamp::now();
        let mut out = Vec::new();
        for (port, cwd) in listeners() {
            let Some(place) = extract::place_from_path(&cwd) else {
                continue;
            };
            let first = *self.seen.entry((port, place.clone())).or_insert(now);
            out.push(ActivityEvent {
                ts: first,
                end_ts: Some(now),
                repo: place.clone(),
                branch: String::new(),
                kind: ActivityKind::Cwd,
                ext_id: Some(format!("port:{port}:{place}")),
                summary: None,
                detail: Some(serde_json::json!({ "path": cwd, "port": port }).to_string()),
            });
        }
        out
    }
}

impl FocusProvider for PortMapProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

/// `(port, cwd)` per listening TCP socket whose owner is readable.
#[cfg(target_os = "linux")]
fn listeners() -> Vec<(u16, String)> {
    let mut by_inode: HashMap<u64, u16> = HashMap::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for (inode, port) in parse_listeners(&text) {
            by_inode.entry(inode).or_insert(port);
        }
    }
    if by_inode.is_empty() {
        return Vec::new();
    }
    let mut out: HashMap<u16, String> = HashMap::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        let mut owned: Vec<u16> = Vec::new();
        for fd in fds.flatten() {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            let Some(inode) = socket_inode(&target.to_string_lossy()) else {
                continue;
            };
            if let Some(port) = by_inode.get(&inode) {
                owned.push(*port);
            }
        }
        if owned.is_empty() {
            continue;
        }
        let Ok(cwd) = std::fs::read_link(format!("/proc/{pid}/cwd")) else {
            continue;
        };
        let cwd = cwd.to_string_lossy().into_owned();
        for port in owned {
            out.entry(port).or_insert_with(|| cwd.clone());
        }
    }
    let mut v: Vec<(u16, String)> = out.into_iter().collect();
    v.sort();
    v
}

#[cfg(not(target_os = "linux"))]
fn listeners() -> Vec<(u16, String)> {
    Vec::new()
}

/// `(inode, port)` of every LISTEN row (`st` 0A) in a `/proc/net/tcp` table.
fn parse_listeners(text: &str) -> Vec<(u64, u16)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.get(3).copied() != Some("0A") {
                return None;
            }
            let port = u16::from_str_radix(f.get(1)?.rsplit(':').next()?, 16).ok()?;
            let inode = f.get(9)?.parse::<u64>().ok()?;
            Some((inode, port))
        })
        .collect()
}

/// `socket:[12345]` → 12345.
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listen_rows_only() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:1F41 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 41234 1 0000000000000000 100 0 0 10 0\n\
   1: 0100007F:1F41 0100007F:B3C2 01 00000000:00000000 00:00000000 00000000  1000        0 41999 1 0000000000000000 100 0 0 10 0\n\
   2: 00000000:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 52000 1 0000000000000000 100 0 0 10 0\n";
        assert_eq!(parse_listeners(table), vec![(41234, 8001), (52000, 3000)]);
        assert_eq!(socket_inode("socket:[41234]"), Some(41234));
        assert_eq!(socket_inode("/dev/null"), None);
    }
}
