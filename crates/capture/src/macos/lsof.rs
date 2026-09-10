//! `lsof` for TCP listeners (m38 chunk 2): one call for the listening
//! sockets, one for the owning processes' cwd. The `-F` field output is
//! parsed by the pure, platform-neutral `parse_listen`/`parse_cwd` in
//! `crate::ports` so those parsers can be tested on Linux too; this file
//! only runs the commands.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ports::{parse_cwd, parse_listen};

static WARNED: AtomicBool = AtomicBool::new(false);

/// `(port, cwd)` per listening TCP socket whose owning process's cwd is
/// readable. `lsof` missing or failing to run warns once and yields
/// nothing rather than erroring the poll loop; a nonzero exit with no
/// matches (nothing listening) is not a failure.
pub fn listeners() -> Vec<(u16, String)> {
    let Some(listen_out) = run(&["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]) else {
        return Vec::new();
    };
    let listen = parse_listen(&listen_out);
    if listen.is_empty() {
        return Vec::new();
    }
    let mut pids: Vec<u32> = listen.iter().map(|(pid, _)| *pid).collect();
    pids.sort_unstable();
    pids.dedup();
    let pid_list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let Some(cwd_out) = run(&["-a", "-p", &pid_list, "-d", "cwd", "-Fpn"]) else {
        return Vec::new();
    };
    let cwds = parse_cwd(&cwd_out);
    listen
        .into_iter()
        .filter_map(|(pid, port)| cwds.get(&pid).map(|cwd| (port, cwd.clone())))
        .collect()
}

fn run(args: &[&str]) -> Option<String> {
    match Command::new("lsof").args(args).output() {
        Ok(out) => Some(String::from_utf8_lossy(&out.stdout).into_owned()),
        Err(e) => {
            warn_once(&e.to_string());
            None
        }
    }
}

fn warn_once(err: &str) {
    if !WARNED.swap(true, Ordering::Relaxed) {
        tracing::warn!("lsof: {err}");
    }
}
