//! Listener map (m32 chunk 4): which repo owns each local dev server.
//! Every minute the TCP listeners in `/proc/net/tcp{,6}` are walked to the
//! process holding the socket and that process's working directory; a
//! listener whose cwd names a place is emitted as a `Cwd` row keyed
//! `port:<port>:<place>` with `port` in its detail, refreshed while it
//! lives. The anchors turn a browser span on `localhost:<port>` into that
//! place. Linux `/proc` and macOS `lsof` (`macos/lsof.rs`); elsewhere the
//! provider emits nothing.

use std::collections::{HashMap, HashSet};
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

#[cfg(target_os = "macos")]
fn listeners() -> Vec<(u16, String)> {
    crate::macos::lsof::listeners()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn listeners() -> Vec<(u16, String)> {
    Vec::new()
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))] // Linux `/proc` shape, tested everywhere
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// `socket:[12345]` → 12345.
fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// `(pid, port)` per listener from `lsof -nP -iTCP -sTCP:LISTEN -Fpn`: a
/// `p<pid>` line sets the current pid, each following `n<addr>:<port>`
/// line is one socket under it. IPv4 and IPv6 rows duplicate the same
/// port; deduped by `(pid, port)`. Platform-neutral so it can be tested
/// here; only `macos/lsof.rs` runs the actual command.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only called from macos::lsof
pub(crate) fn parse_listen(out: &str) -> Vec<(u32, u16)> {
    let mut pid: Option<u32> = None;
    let mut seen: HashSet<(u32, u16)> = HashSet::new();
    let mut result = Vec::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix('n') {
            let Some(p) = pid else { continue };
            let Some(port) = rest.rsplit(':').next().and_then(|s| s.parse::<u16>().ok()) else {
                continue;
            };
            if seen.insert((p, port)) {
                result.push((p, port));
            }
        }
    }
    result
}

/// `pid -> cwd` from `lsof -a -p <pids> -d cwd -Fpn`: a `p<pid>` line sets
/// the current pid, the following `n<path>` line is its cwd.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))] // only called from macos::lsof
pub(crate) fn parse_cwd(out: &str) -> HashMap<u32, String> {
    let mut pid: Option<u32> = None;
    let mut map = HashMap::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix('n')
            && let Some(p) = pid
        {
            map.insert(p, rest.to_owned());
        }
    }
    map
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

    #[test]
    fn parse_listen_dedupes_v4_and_v6_rows() {
        let out = "p1234\nn*:8080\nn[::1]:8080\np5678\nn127.0.0.1:5432\n";
        assert_eq!(parse_listen(out), vec![(1234, 8080), (5678, 5432)]);
    }

    #[test]
    fn parse_listen_ignores_n_lines_before_any_pid() {
        assert!(parse_listen("n*:8080\n").is_empty());
    }

    #[test]
    fn parse_cwd_maps_pid_to_path() {
        let out = "p1234\nn/home/james/dev/chronicle\np5678\nn/home/james/dev/otherapp\n";
        let map = parse_cwd(out);
        assert_eq!(
            map.get(&1234).map(String::as_str),
            Some("/home/james/dev/chronicle")
        );
        assert_eq!(
            map.get(&5678).map(String::as_str),
            Some("/home/james/dev/otherapp")
        );
        assert_eq!(map.len(), 2);
    }
}
