//! The focused window's pid, asked of the compositor (m39).
//!
//! `wlr-foreign-toplevel-management` carries no pid, and the pid is what
//! the terminal cwd probe needs. Sway, Hyprland and Niri each answer it in
//! one request over a unix socket, so the wlroots route asks whichever of
//! them is running. Elsewhere focus rows have no pid and placement falls to
//! the shell hook, as it does on WSL2.
//!
//! Every answer is checked against the app_id the toplevel protocol
//! reported: all three sockets return the window the compositor itself
//! considers focused, so a mismatch means focus moved between the protocol
//! event and the reply, and the next event corrects it. The title is not
//! compared — a window's title can change between the two without the
//! window changing, and that would cost a pid for nothing.

use std::io::{Read as _, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// A wedged compositor must never stall the focus thread.
const TIMEOUT: Duration = Duration::from_millis(250);
/// i3 IPC replies are small; a tree on a busy desktop is tens of kilobytes.
const MAX_REPLY: u64 = 4 * 1024 * 1024;

/// Which compositor socket to ask, decided once from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ipc {
    /// `$SWAYSOCK`, the i3 IPC protocol.
    Sway(PathBuf),
    /// `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock`, line requests.
    Hyprland(PathBuf),
    /// `$NIRI_SOCKET`, one JSON request per line.
    Niri(PathBuf),
    /// No socket in the environment: river, labwc, an older Niri.
    None,
}

impl Ipc {
    pub fn detect() -> Self {
        Self::from_env(|key| std::env::var(key).ok())
    }

    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(sock) = env("SWAYSOCK").filter(|s| !s.is_empty()) {
            return Ipc::Sway(PathBuf::from(sock));
        }
        if let Some(sock) = env("NIRI_SOCKET").filter(|s| !s.is_empty()) {
            return Ipc::Niri(PathBuf::from(sock));
        }
        if let Some(signature) = env("HYPRLAND_INSTANCE_SIGNATURE").filter(|s| !s.is_empty()) {
            // Hyprland moved the socket under XDG_RUNTIME_DIR in 0.35;
            // before that it lived in /tmp.
            let runtime = env("XDG_RUNTIME_DIR").map(PathBuf::from);
            let candidates = [
                runtime.map(|dir| dir.join("hypr").join(&signature).join(".socket.sock")),
                Some(
                    PathBuf::from("/tmp/hypr")
                        .join(&signature)
                        .join(".socket.sock"),
                ),
            ];
            for path in candidates.into_iter().flatten() {
                if path.exists() {
                    return Ipc::Hyprland(path);
                }
            }
        }
        Ipc::None
    }

    /// The focused window's pid, or `None` when there is no socket, the
    /// compositor did not answer, or it answered about another window.
    pub fn focused_pid(&self, app: &str) -> Option<u32> {
        let reply = match self {
            Ipc::Sway(path) => sway_request(path).ok()?,
            Ipc::Hyprland(path) => request(path, "j/activewindow", Read::Eof).ok()?,
            Ipc::Niri(path) => request(path, "\"FocusedWindow\"", Read::Line).ok()?,
            Ipc::None => return None,
        };
        let value: serde_json::Value = serde_json::from_slice(&reply).ok()?;
        let window = match self {
            Ipc::Sway(_) => sway_focused(&value)?,
            Ipc::Hyprland(_) => value,
            Ipc::Niri(_) => value.get("Ok")?.get("FocusedWindow")?.clone(),
            Ipc::None => return None,
        };
        matching_pid(&window, app)
    }
}

/// The pid, but only when the window the compositor named is the one the
/// toplevel protocol reported.
fn matching_pid(window: &serde_json::Value, app: &str) -> Option<u32> {
    let reported = window_app(window);
    if reported != app {
        return None;
    }
    let pid = window.get("pid")?.as_i64()?;
    u32::try_from(pid).ok().filter(|pid| *pid != 0)
}

/// Sway and Niri call it `app_id`, Hyprland `class`; an Xwayland window
/// under sway has neither and carries `window_properties.class` instead.
fn window_app(window: &serde_json::Value) -> &str {
    for key in ["app_id", "class"] {
        if let Some(app) = window.get(key).and_then(|v| v.as_str()) {
            return app;
        }
    }
    window
        .get("window_properties")
        .and_then(|p| p.get("class"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
}

/// Depth-first for the node sway marked focused. The tree nests
/// `nodes`/`floating_nodes`, and only one node in it is focused.
fn sway_focused(tree: &serde_json::Value) -> Option<serde_json::Value> {
    if tree.get("focused").and_then(|v| v.as_bool()) == Some(true) {
        return Some(tree.clone());
    }
    for key in ["nodes", "floating_nodes"] {
        for child in tree.get(key).and_then(|v| v.as_array())?.iter() {
            if let Some(found) = sway_focused(child) {
                return Some(found);
            }
        }
    }
    None
}

/// i3 IPC framing: the magic, a payload length and a message type, all
/// little-endian, in both directions. `GET_TREE` is type 4.
fn sway_request(path: &PathBuf) -> std::io::Result<Vec<u8>> {
    const MAGIC: &[u8; 6] = b"i3-ipc";
    const GET_TREE: u32 = 4;

    let mut stream = connect(path)?;
    let mut request = Vec::with_capacity(14);
    request.extend_from_slice(MAGIC);
    request.extend_from_slice(&0u32.to_ne_bytes());
    request.extend_from_slice(&GET_TREE.to_ne_bytes());
    stream.write_all(&request)?;
    stream.flush()?;

    let mut header = [0u8; 14];
    stream.read_exact(&mut header)?;
    if &header[..6] != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not an i3 IPC reply",
        ));
    }
    let len = u32::from_ne_bytes([header[6], header[7], header[8], header[9]]);
    let mut payload = Vec::new();
    stream
        .take(u64::from(len).min(MAX_REPLY))
        .read_to_end(&mut payload)?;
    Ok(payload)
}

/// How the reply ends. Hyprland pretty-prints its JSON across lines and
/// closes the socket, so its reply is everything up to EOF. Niri answers
/// with one compact line and may hold the connection open, so reading to
/// EOF there would block until the read timeout and lose the reply.
enum Read {
    Eof,
    Line,
}

/// Hyprland and Niri both take one line of request.
fn request(path: &PathBuf, request: &str, until: Read) -> std::io::Result<Vec<u8>> {
    use std::io::{BufRead, BufReader, Read as _};

    let mut stream = connect(path)?;
    stream.write_all(request.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = BufReader::new(stream).take(MAX_REPLY);
    let mut reply = Vec::new();
    match until {
        Read::Eof => {
            reader.read_to_end(&mut reply)?;
        }
        Read::Line => {
            reader.read_until(b'\n', &mut reply)?;
        }
    }
    Ok(reply)
}

fn connect(path: &PathBuf) -> std::io::Result<UnixStream> {
    let stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn sway_wins_when_more_than_one_socket_is_set() {
        let ipc = Ipc::from_env(env_of(&[
            ("SWAYSOCK", "/run/user/1000/sway-ipc.sock"),
            ("NIRI_SOCKET", "/run/user/1000/niri.sock"),
        ]));
        assert_eq!(
            ipc,
            Ipc::Sway(PathBuf::from("/run/user/1000/sway-ipc.sock"))
        );
    }

    #[test]
    fn an_empty_socket_variable_is_no_socket() {
        assert_eq!(Ipc::from_env(env_of(&[("SWAYSOCK", "")])), Ipc::None);
        assert_eq!(Ipc::from_env(env_of(&[])), Ipc::None);
    }

    #[test]
    fn hyprland_needs_the_socket_to_exist() {
        // Neither candidate path is there, so the route has no pid source
        // rather than a path that will fail on every focus change.
        let ipc = Ipc::from_env(env_of(&[
            ("HYPRLAND_INSTANCE_SIGNATURE", "deadbeef"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]));
        assert_eq!(ipc, Ipc::None);
    }

    #[test]
    fn the_sway_tree_yields_the_focused_leaf() {
        let tree = serde_json::json!({
            "type": "root", "focused": false,
            "nodes": [
                {"type": "output", "focused": false, "nodes": [
                    {"type": "con", "app_id": "firefox", "pid": 11, "focused": false, "nodes": []},
                    {"type": "con", "app_id": "foot", "pid": 22, "focused": true, "nodes": []}
                ], "floating_nodes": []}
            ],
            "floating_nodes": []
        });
        let focused = sway_focused(&tree).unwrap();
        assert_eq!(matching_pid(&focused, "foot"), Some(22));
        // Focus moved between the protocol event and the reply.
        assert_eq!(matching_pid(&focused, "firefox"), None);
    }

    #[test]
    fn a_floating_sway_window_is_found_too() {
        let tree = serde_json::json!({
            "focused": false, "nodes": [], "floating_nodes": [
                {"app_id": "pavucontrol", "pid": 7, "focused": true, "nodes": [], "floating_nodes": []}
            ]
        });
        let focused = sway_focused(&tree).unwrap();
        assert_eq!(matching_pid(&focused, "pavucontrol"), Some(7));
    }

    #[test]
    fn an_xwayland_window_under_sway_is_matched_on_its_class() {
        let window = serde_json::json!({
            "app_id": serde_json::Value::Null,
            "pid": 99,
            "focused": true,
            "window_properties": {"class": "Steam", "instance": "steam"}
        });
        assert_eq!(matching_pid(&window, "Steam"), Some(99));
    }

    #[test]
    fn a_sway_tree_with_nothing_focused_yields_nothing() {
        let tree = serde_json::json!({"focused": false, "nodes": [], "floating_nodes": []});
        assert!(sway_focused(&tree).is_none());
    }

    #[test]
    fn the_hyprland_reply_is_the_window_itself() {
        let reply: serde_json::Value = serde_json::from_str(
            r#"{"address":"0x55d2","class":"kitty","title":"zsh","pid":4242,"floating":false}"#,
        )
        .unwrap();
        assert_eq!(matching_pid(&reply, "kitty"), Some(4242));
        assert_eq!(matching_pid(&reply, "foot"), None);
    }

    #[test]
    fn the_niri_reply_is_wrapped_in_its_result() {
        let reply: serde_json::Value = serde_json::from_str(
            r#"{"Ok":{"FocusedWindow":{"id":3,"title":"nvim","app_id":"Alacritty","pid":515,"is_focused":true}}}"#,
        )
        .unwrap();
        let window = reply
            .get("Ok")
            .and_then(|v| v.get("FocusedWindow"))
            .unwrap();
        assert_eq!(matching_pid(window, "Alacritty"), Some(515));
    }

    #[test]
    fn a_window_without_a_usable_pid_yields_nothing() {
        for window in [
            serde_json::json!({"app_id": "foot"}),
            serde_json::json!({"app_id": "foot", "pid": 0}),
            serde_json::json!({"app_id": "foot", "pid": -1}),
        ] {
            assert_eq!(matching_pid(&window, "foot"), None, "{window}");
        }
    }
}
