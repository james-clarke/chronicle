//! libproc on macOS (m38 chunk 2): a terminal's cwd is the newest child
//! shell's cwd — the macOS twin of the `/proc` walk in `x11.rs:54-95`.
//! Terminal.app and iTerm2 insert a `login` wrapper between the terminal
//! and the shell; it is stepped through when seen.

use std::ffi::c_void;
use std::mem;

/// A process's own cwd via `PROC_PIDVNODEPATHINFO`. `None` on any failure
/// (permission, exited process, non-UTF8 path).
pub fn cwd(pid: u32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast::<c_void>(),
            size,
        )
    };
    if ret <= 0 {
        return None;
    }
    let path = cdir_path(&info);
    (!path.is_empty()).then_some(path)
}

/// `pvi_cdir.vip_path` is `[[c_char; 32]; 32]` (the vendored `libc` splits
/// the `MAXPATHLEN` buffer this way for an old-rustc array-size limit);
/// flatten it and stop at the first NUL.
fn cdir_path(info: &libc::proc_vnodepathinfo) -> String {
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Direct children of `ppid`. A fixed 4096-pid buffer avoids a second
/// `proc_listchildpids` call to size it first; no real terminal has more
/// children than that.
fn list_children(ppid: u32) -> Vec<u32> {
    const MAX: usize = 4096;
    let mut buf = vec![0i32; MAX];
    let bytes = unsafe {
        libc::proc_listchildpids(
            ppid as libc::pid_t,
            buf.as_mut_ptr().cast::<c_void>(),
            (MAX * mem::size_of::<i32>()) as libc::c_int,
        )
    };
    if bytes <= 0 {
        return Vec::new();
    }
    let count = (bytes as usize / mem::size_of::<i32>()).min(MAX);
    buf.truncate(count);
    buf.into_iter()
        .filter(|&p| p > 0)
        .map(|p| p as u32)
        .collect()
}

/// A process's short name via `proc_name`; empty on failure.
fn proc_name(pid: u32) -> String {
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::proc_name(
            pid as libc::c_int,
            buf.as_mut_ptr().cast::<c_void>(),
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buf[..n as usize]).into_owned()
}

/// A process's start time in microseconds via `PROC_PIDTBSDINFO`; 0 on
/// failure, which sorts last and is never picked over a process whose
/// start is known.
fn start_us(pid: u32) -> u64 {
    let mut info: libc::proc_bsdinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast::<c_void>(),
            size,
        )
    };
    if ret <= 0 {
        return 0;
    }
    info.pbi_start_tvsec
        .saturating_mul(1_000_000)
        .saturating_add(info.pbi_start_tvusec)
}

/// A process row for the terminal-child pick: `(pid, name, start_us,
/// parent)`.
type Row = (u32, String, u64, u32);

fn rows_for(parent: u32) -> Vec<Row> {
    list_children(parent)
        .into_iter()
        .map(|pid| (pid, proc_name(pid), start_us(pid), parent))
        .collect()
}

/// Working directory of the focused terminal (m38): the newest child
/// shell of the terminal process (the tab opened last, exact for a single
/// tab), else the terminal's own cwd.
pub fn terminal_cwd(pid: u32) -> Option<String> {
    let mut rows = rows_for(pid);
    if let Some(login_pid) = rows
        .iter()
        .find(|(_, name, _, _)| name == "login")
        .map(|r| r.0)
    {
        rows.extend(rows_for(login_pid));
    }
    cwd(pick_terminal_child(pid, &rows))
}

/// Pure pick over process rows: the newest direct child of `terminal` by
/// start time; when one of those children is `login` (Terminal.app and
/// iTerm2 spawn it before the shell), the newest of *its* children
/// instead. `terminal` itself when it has no usable children.
fn pick_terminal_child(terminal: u32, rows: &[Row]) -> u32 {
    let children_of =
        |parent: u32| -> Vec<&Row> { rows.iter().filter(|(_, _, _, p)| *p == parent).collect() };
    let mut candidates = children_of(terminal);
    if let Some(login_pid) = candidates
        .iter()
        .find(|(_, name, _, _)| name == "login")
        .map(|r| r.0)
    {
        let login_children = children_of(login_pid);
        if !login_children.is_empty() {
            candidates = login_children;
        }
    }
    candidates
        .into_iter()
        .max_by_key(|(_, _, start, _)| *start)
        .map(|r| r.0)
        .unwrap_or(terminal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, name: &str, start_us: u64, parent: u32) -> Row {
        (pid, name.to_owned(), start_us, parent)
    }

    #[test]
    fn no_children_picks_the_terminal_itself() {
        assert_eq!(pick_terminal_child(100, &[]), 100);
    }

    #[test]
    fn newest_direct_child_wins() {
        let rows = vec![
            row(201, "zsh", 10, 100),
            row(202, "zsh", 30, 100),
            row(203, "zsh", 20, 100),
        ];
        assert_eq!(pick_terminal_child(100, &rows), 202);
    }

    #[test]
    fn steps_through_a_login_wrapper() {
        let rows = vec![
            row(300, "login", 5, 100),
            row(401, "zsh", 10, 300),
            row(402, "bash", 40, 300),
        ];
        assert_eq!(pick_terminal_child(100, &rows), 402);
    }

    #[test]
    fn login_with_no_children_falls_back_to_its_own_row() {
        let rows = vec![row(300, "login", 5, 100)];
        assert_eq!(pick_terminal_child(100, &rows), 300);
    }
}
