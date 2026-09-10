//! The KDE focus route (m39). KWin exposes `org_kde_plasma_window_management`
//! only to its own shell clients, so the Wayland protocol route is closed;
//! what is open is KWin's scripting API, which is what `awatcher` and
//! `kdotool` use. Chronicle owns a name on the session bus, loads a script
//! into KWin that calls back on every window activation and caption change,
//! and unloads it on the way out.
//!
//! The script reports the pid, so this route places terminals through the
//! cwd probe exactly as X11 does.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chronicle_core::types::CaptureEvent;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use zbus::blocking::{Connection, Proxy};

use super::focus::FocusState;
use crate::{BoxError, FocusProvider};

const BUS_NAME: &str = "dev.chronicled.Chronicle";
const OBJECT_PATH: &str = "/dev/chronicled/Chronicle1";
const INTERFACE: &str = "dev.chronicled.Chronicle1";
const KWIN: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING_IFACE: &str = "org.kde.kwin.Scripting";
const SCRIPT_IFACE: &str = "org.kde.kwin.Script";
const PLUGIN: &str = "chronicle";

/// One activation or caption change, as the script reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub pid: u32,
    pub app: String,
    pub title: String,
}

/// The object KWin's `callDBus` talks to.
struct FocusSink {
    tx: Sender<Report>,
    /// The load, the run and the first report are three separate things
    /// that can fail; one line says all three worked.
    first: AtomicBool,
}

#[zbus::interface(name = "dev.chronicled.Chronicle1")]
impl FocusSink {
    /// KWin calls this on every window activation and on every caption
    /// change of the active window. `pid` is a decimal string: see the
    /// note in the script.
    fn focus(&self, pid: String, app: String, title: String) {
        let pid = pid.parse().unwrap_or(0);
        if self.first.swap(false, Ordering::Relaxed) {
            tracing::info!("the KWin script is reporting focus");
        }
        let _ = self.tx.send(Report { pid, app, title });
    }
}

/// The script KWin runs. `windowActivated` is Plasma 6 and `clientActivated`
/// Plasma 5; the script picks whichever the running KWin has rather than
/// asking the version, since both have shipped under the same D-Bus name.
pub fn script() -> String {
    format!(
        r#"// Chronicle (m39): report the active window to the daemon.
function report(w) {{
    if (!w) {{ return; }}
    // Every argument is a string on purpose: KWin turns a JS number into
    // whichever of int32/uint32/double QJSValue::toVariant picks, and a
    // signature the interface does not declare is dropped without an error.
    callDBus("{BUS_NAME}", "{OBJECT_PATH}", "{INTERFACE}", "Focus",
             String(w.pid > 0 ? w.pid : 0),
             String(w.resourceClass || ""),
             String(w.caption || ""));
}}

var current = null;

function onCaption() {{ report(current); }}

function activated(w) {{
    if (current && current.captionChanged) {{
        try {{ current.captionChanged.disconnect(onCaption); }} catch (e) {{ }}
    }}
    current = w;
    if (w && w.captionChanged) {{ w.captionChanged.connect(onCaption); }}
    report(w);
}}

if (workspace.windowActivated) {{
    workspace.windowActivated.connect(activated);
    activated(workspace.activeWindow);
}} else {{
    workspace.clientActivated.connect(activated);
    activated(workspace.activeClient);
}}
"#
    )
}

/// Where the script is written for KWin to read. KWin loads it by path, so
/// it has to outlive the load call, not the process.
fn script_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    dir.join("chronicle").join("kwin-focus.js")
}

pub struct KwinFocusProvider {
    conn: Connection,
    path: PathBuf,
}

impl KwinFocusProvider {
    /// Fails when KWin's scripting service is not on the session bus, which
    /// is every non-KDE session.
    pub fn new() -> Result<Self, BoxError> {
        let conn = Connection::session()?;
        let scripting = Proxy::new(&conn, KWIN, SCRIPTING_PATH, SCRIPTING_IFACE)?;
        // Any call proves the service answers; `isScriptLoaded` is harmless
        // and also tells us whether a previous run left the script behind.
        let loaded: bool = scripting.call("isScriptLoaded", &(PLUGIN))?;
        if loaded {
            let _: () = scripting.call("unloadScript", &(PLUGIN))?;
        }
        let path = script_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, script())?;
        Ok(Self { conn, path })
    }
}

/// Unloads the script when the focus thread stops, so a restarted daemon
/// does not leave KWin reporting into a bus name nobody owns.
struct ScriptGuard {
    conn: Connection,
}

impl Drop for ScriptGuard {
    fn drop(&mut self) {
        if let Ok(scripting) = Proxy::new(&self.conn, KWIN, SCRIPTING_PATH, SCRIPTING_IFACE) {
            let _: Result<(), _> = scripting.call("unloadScript", &(PLUGIN));
        }
    }
}

/// `loadScript` answers with the script's id; `run` then starts it. The
/// object it lives at moved in Plasma 6, and both names are still in the
/// wild, so try the new one and fall back.
fn load_and_run(conn: &Connection, path: &Path) -> Result<(), BoxError> {
    let scripting = Proxy::new(conn, KWIN, SCRIPTING_PATH, SCRIPTING_IFACE)?;
    let id: i32 = scripting.call("loadScript", &(path.to_string_lossy().as_ref(), PLUGIN))?;
    let paths = [format!("/Scripting/Script{id}"), format!("/{id}")];
    let mut last = None;
    for object in paths {
        let script = match Proxy::new(conn, KWIN, object.clone(), SCRIPT_IFACE) {
            Ok(script) => script,
            Err(e) => {
                last = Some(e.to_string());
                continue;
            }
        };
        match script.call::<_, _, ()>("run", &()) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e.to_string()),
        }
    }
    Err(format!(
        "KWin loaded the script as {id} but would not run it: {}",
        last.unwrap_or_else(|| "no error".into())
    )
    .into())
}

/// One report against the current state. A different window (pid, or app
/// under the same pid) is a focus change; the same window with a new
/// caption is a title change, debounced like the X11 provider's.
fn apply(
    focus: &mut FocusState,
    report: Report,
    now: Instant,
    tx: &Sender<CaptureEvent>,
) -> Result<(), BoxError> {
    let pid = (report.pid != 0).then_some(report.pid);
    if focus.current_pid() == pid && focus.current_app() == report.app {
        if focus.current_title() != report.title {
            focus.title(report.title, now);
        }
        return Ok(());
    }
    focus.focus(report.app, report.title, pid, tx)
}

impl FocusProvider for KwinFocusProvider {
    fn run(self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        let (reports_tx, reports) = crossbeam_channel::unbounded();
        let _sink = zbus::blocking::connection::Builder::session()?
            .name(BUS_NAME)?
            .serve_at(
                OBJECT_PATH,
                FocusSink {
                    tx: reports_tx,
                    first: AtomicBool::new(true),
                },
            )?
            .build()?;
        load_and_run(&self.conn, &self.path)?;
        let _guard = ScriptGuard {
            conn: self.conn.clone(),
        };
        pump(reports, tx)
    }
}

/// The focus loop: block on the script's reports, but never past a pending
/// title's deadline.
fn pump(reports: Receiver<Report>, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
    let mut focus = FocusState::new();
    loop {
        focus.flush(Instant::now(), &tx)?;
        let report = match focus.wait(Instant::now()) {
            Some(wait) => match reports.recv_timeout(wait) {
                Ok(report) => Some(report),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("the KWin script's channel closed".into());
                }
            },
            None => Some(reports.recv()?),
        };
        if let Some(report) = report {
            apply(&mut focus, report, Instant::now(), &tx)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wayland::focus::TITLE_DEBOUNCE;

    #[test]
    fn the_script_handles_both_plasma_generations() {
        let text = script();
        assert!(text.contains("workspace.windowActivated"), "{text}");
        assert!(text.contains("workspace.clientActivated"), "{text}");
        assert!(text.contains("activated(workspace.activeWindow)"), "{text}");
        assert!(text.contains("activated(workspace.activeClient)"), "{text}");
    }

    #[test]
    fn the_script_calls_the_interface_the_daemon_serves() {
        let text = script();
        assert!(
            text.contains(&format!(
                r#"callDBus("{BUS_NAME}", "{OBJECT_PATH}", "{INTERFACE}", "Focus","#
            )),
            "{text}"
        );
        for field in ["w.pid", "w.resourceClass", "w.caption"] {
            assert!(text.contains(field), "{field} missing from {text}");
        }
        // Every argument crosses as a string; see the note in the script.
        for arg in [
            "String(w.pid > 0 ? w.pid : 0)",
            r#"String(w.resourceClass || "")"#,
            r#"String(w.caption || "")"#,
        ] {
            assert!(text.contains(arg), "{arg} missing from {text}");
        }
    }

    #[test]
    fn the_script_follows_the_caption_of_the_window_it_last_saw() {
        let text = script();
        assert!(
            text.contains("current.captionChanged.disconnect(onCaption)"),
            "{text}"
        );
        assert!(
            text.contains("w.captionChanged.connect(onCaption)"),
            "{text}"
        );
    }

    fn kinds(rx: &Receiver<CaptureEvent>) -> Vec<(String, String, Option<u32>)> {
        rx.try_iter()
            .filter_map(|event| match event {
                CaptureEvent::Focus(f) => Some(("focus".into(), f.title, f.pid)),
                CaptureEvent::TitleChanged(f) => Some(("title".into(), f.title, f.pid)),
                _ => None,
            })
            .collect()
    }

    fn report(pid: u32, app: &str, title: &str) -> Report {
        Report {
            pid,
            app: app.into(),
            title: title.into(),
        }
    }

    #[test]
    fn a_new_window_is_a_focus_change_even_with_the_same_app_and_title() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut focus = FocusState::new();
        apply(&mut focus, report(10, "konsole", "zsh"), now, &tx).unwrap();
        apply(&mut focus, report(11, "konsole", "zsh"), now, &tx).unwrap();
        assert_eq!(
            kinds(&rx),
            vec![
                ("focus".into(), "zsh".into(), Some(10)),
                ("focus".into(), "zsh".into(), Some(11)),
            ]
        );
    }

    #[test]
    fn a_caption_change_on_the_same_window_debounces_into_a_title_row() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut focus = FocusState::new();
        apply(&mut focus, report(10, "konsole", "zsh"), now, &tx).unwrap();
        apply(&mut focus, report(10, "konsole", "cargo"), now, &tx).unwrap();
        apply(
            &mut focus,
            report(10, "konsole", "cargo test"),
            now + TITLE_DEBOUNCE / 2,
            &tx,
        )
        .unwrap();
        focus.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        assert_eq!(
            kinds(&rx),
            vec![
                ("focus".into(), "zsh".into(), Some(10)),
                ("title".into(), "cargo test".into(), Some(10)),
            ]
        );
    }

    #[test]
    fn a_repeated_activation_of_the_same_window_emits_nothing_new() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let now = Instant::now();
        let mut focus = FocusState::new();
        apply(&mut focus, report(10, "konsole", "zsh"), now, &tx).unwrap();
        apply(&mut focus, report(10, "konsole", "zsh"), now, &tx).unwrap();
        focus.flush(now + TITLE_DEBOUNCE * 2, &tx).unwrap();
        assert_eq!(kinds(&rx), vec![("focus".into(), "zsh".into(), Some(10))]);
    }

    #[test]
    fn a_window_kwin_reports_without_a_pid_carries_none() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut focus = FocusState::new();
        apply(&mut focus, report(0, "konsole", "zsh"), Instant::now(), &tx).unwrap();
        assert_eq!(kinds(&rx), vec![("focus".into(), "zsh".into(), None)]);
    }
}
