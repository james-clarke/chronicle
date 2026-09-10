//! Mic-in-use watcher (m22, Linux/PipeWire; m38, macOS/CoreAudio): polls a
//! `MicSource` for the apps capturing the microphone right now and emits
//! one `call` span from the first capture to the last release. Explains
//! AFK gaps without a calendar.

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(20);

/// One poll of "what's using the mic right now": `pw-dump` on Linux,
/// CoreAudio on macOS. `Err` is a transient failure (a PipeWire hiccup, a
/// brief permission blip): `MicProvider` warns once and reports no
/// events, leaving an open call span untouched rather than closing it on
/// bad data.
pub trait MicSource: Send {
    fn active_inputs(&mut self) -> Result<Vec<String>, String>;
}

pub struct MicProvider<S: MicSource> {
    source: S,
    /// Call in progress: start and the app that opened it.
    current: Option<(Timestamp, String)>,
    /// Last error warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl<S: MicSource> MicProvider<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            current: None,
            last_err: None,
        }
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let apps = match self.source.active_inputs() {
            Ok(apps) => {
                self.last_err = None;
                apps
            }
            Err(e) => {
                self.warn_once(e);
                return Vec::new();
            }
        };
        self.step(&apps, Timestamp::now()).into_iter().collect()
    }

    fn warn_once(&mut self, err: String) {
        if self.last_err.as_deref() != Some(err.as_str()) {
            tracing::warn!("mic source poll: {err}");
            self.last_err = Some(err);
        }
    }

    /// Transition on the capture set: none→some opens a call, some→none
    /// closes it (same `ext_id`, so storage fills `end_ts`).
    fn step(&mut self, apps: &[String], now: Timestamp) -> Option<ActivityEvent> {
        match (&self.current, apps.is_empty()) {
            (None, false) => {
                let app = apps.join(", ");
                self.current = Some((now, app.clone()));
                Some(call_event(now, None, &app))
            }
            (Some((start, app)), true) => {
                let e = call_event(*start, Some(now), app);
                self.current = None;
                Some(e)
            }
            _ => None,
        }
    }
}

fn call_event(start: Timestamp, end: Option<Timestamp>, app: &str) -> ActivityEvent {
    ActivityEvent {
        ts: start,
        end_ts: end,
        repo: String::new(),
        branch: String::new(),
        kind: ActivityKind::Call,
        ext_id: Some(format!("call:{}", start.as_millisecond())),
        summary: (!app.is_empty()).then(|| app.to_owned()),
        detail: None,
    }
}

impl<S: MicSource> FocusProvider for MicProvider<S> {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

/// `pw-dump` polling (Linux/PipeWire).
#[cfg(target_os = "linux")]
pub struct PwDump {
    path: PathBuf,
}

#[cfg(target_os = "linux")]
impl PwDump {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[cfg(target_os = "linux")]
impl MicSource for PwDump {
    fn active_inputs(&mut self) -> Result<Vec<String>, String> {
        let out = Command::new(&self.path)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("pw-dump failed")
                .to_owned());
        }
        Ok(parse_pw_dump(&String::from_utf8_lossy(&out.stdout)))
    }
}

/// Names of apps with a running mic capture stream, sorted and deduped.
/// Exact class match: `Stream/Input/Audio/Internal` (bluez loopbacks) and
/// suspended/idle streams (a parked tab) are not calls.
#[cfg(target_os = "linux")]
fn parse_pw_dump(json: &str) -> Vec<String> {
    let Ok(objects) = serde_json::from_str::<Vec<serde_json::Value>>(json) else {
        return Vec::new();
    };
    let mut apps: Vec<String> = objects
        .iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Node"))
        .filter_map(|o| {
            let info = o.get("info")?;
            if info.get("state").and_then(|s| s.as_str()) != Some("running") {
                return None;
            }
            let props = info.get("props")?;
            if props.get("media.class").and_then(|c| c.as_str()) != Some("Stream/Input/Audio") {
                return None;
            }
            let name = ["application.name", "node.name"]
                .iter()
                .find_map(|k| props.get(*k).and_then(|v| v.as_str()))
                .unwrap_or("unknown");
            Some(name.to_owned())
        })
        .collect();
    apps.sort();
    apps.dedup();
    apps
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const DUMP: &str = r#"[
      {"id":1,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio/Internal","node.name":"bluez_capture_internal"}}},
      {"id":2,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Output/Audio","application.name":"Firefox"}}},
      {"id":3,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio","application.name":"Firefox","media.name":"Meet"}}},
      {"id":4,"type":"PipeWire:Interface:Node","info":{"state":"suspended","props":{"media.class":"Stream/Input/Audio","application.name":"Zoom"}}},
      {"id":5,"type":"PipeWire:Interface:Node","info":{"state":"running","props":{"media.class":"Stream/Input/Audio","application.name":"Firefox"}}},
      {"id":6,"type":"PipeWire:Interface:Port","info":{"props":{"media.class":"Stream/Input/Audio"}}}
    ]"#;

    #[test]
    fn only_running_exact_input_streams_count() {
        assert_eq!(parse_pw_dump(DUMP), vec!["Firefox".to_owned()]);
        assert!(parse_pw_dump("[]").is_empty());
        assert!(parse_pw_dump("nope").is_empty());
    }

    #[test]
    fn call_opens_and_closes_on_the_same_ext_id() {
        let mut p = MicProvider::new(PwDump::new(PathBuf::from("pw-dump")));
        let t0: Timestamp = "2026-09-02T15:00:00Z".parse().unwrap();
        let t1: Timestamp = "2026-09-02T15:20:00Z".parse().unwrap();
        let t2: Timestamp = "2026-09-02T15:32:00Z".parse().unwrap();
        assert!(p.step(&[], t0).is_none());
        let open = p.step(&["Firefox".into()], t0).unwrap();
        assert_eq!(open.kind, ActivityKind::Call);
        assert_eq!(open.end_ts, None);
        assert_eq!(open.summary.as_deref(), Some("Firefox"));
        assert!(
            p.step(&["Firefox".into()], t1).is_none(),
            "still on the call"
        );
        let close = p.step(&[], t2).unwrap();
        assert_eq!(close.ext_id, open.ext_id);
        assert_eq!(close.ts, t0);
        assert_eq!(close.end_ts, Some(t2));
        assert!(p.step(&[], t2).is_none());
    }
}
