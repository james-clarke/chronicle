//! Editor heartbeats (m26 chunk 3): Wakapi-compatible heartbeats folded
//! into `edit` spans per (project, branch).
//!
//! WakaTime plugins (`vim-wakatime` and ~60 more) post one heartbeat every
//! couple of minutes while a file is open. Folding is WakaTime's own
//! "durations" rule: consecutive heartbeats for the same `(project, branch)`
//! belong to one span until a gap of [`GAP_SECS`] passes. Every heartbeat
//! inside the gap re-emits the span with the SAME `ext_id`, so
//! [`crate::types::Dedupe::Upsert`] refreshes `end_ts` in place.

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

use crate::types::{ActivityEvent, ActivityKind, ms_to_ts};

/// Gap that closes an edit span (WakaTime's default heartbeat timeout).
pub const GAP_SECS: i64 = 15 * 60;

/// One heartbeat as the WakaTime API documents it. Clients send `null` for
/// the fields they cannot fill, so every field takes null as its default.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Heartbeat {
    /// File path, app name or domain, per `kind`.
    #[serde(default, deserialize_with = "null_default")]
    pub entity: String,
    /// `file` (the only kind that folds), `app` or `domain`.
    #[serde(rename = "type", default, deserialize_with = "null_default")]
    pub kind: String,
    #[serde(default, deserialize_with = "null_default")]
    pub project: String,
    #[serde(default, deserialize_with = "null_default")]
    pub branch: String,
    #[serde(default, deserialize_with = "null_default")]
    pub language: String,
    /// Epoch seconds, fractional.
    #[serde(default, deserialize_with = "null_default")]
    pub time: f64,
    #[serde(default, deserialize_with = "null_default")]
    pub is_write: bool,
}

fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

struct Span {
    start_ms: i64,
    end_ms: i64,
}

/// Open edit span per `(project, branch)`. Lives in the HTTP endpoint for
/// as long as the daemon runs; a restart just starts new spans.
#[derive(Default)]
pub struct Folder {
    spans: HashMap<(String, String), Span>,
}

impl Folder {
    /// The event this heartbeat belongs to, or `None` when it is not a file
    /// heartbeat (browser/app plugins report domains and apps; focus capture
    /// already covers those). `now_ms` stands in for a missing `time`.
    pub fn fold(&mut self, hb: &Heartbeat, now_ms: i64) -> Option<ActivityEvent> {
        if !hb.kind.is_empty() && hb.kind != "file" {
            return None;
        }
        let ms = if hb.time > 0.0 {
            (hb.time * 1000.0) as i64
        } else {
            now_ms
        };
        let key = (hb.project.clone(), hb.branch.clone());
        let open = match self.spans.get(&key) {
            Some(s) => ms >= s.start_ms && ms - s.end_ms <= GAP_SECS * 1000,
            None => false,
        };
        if !open {
            self.spans.insert(
                key.clone(),
                Span {
                    start_ms: ms,
                    end_ms: ms,
                },
            );
        }
        let span = self.spans.get_mut(&key).expect("open or just inserted");
        span.end_ms = span.end_ms.max(ms);
        Some(ActivityEvent {
            ts: ms_to_ts(span.start_ms),
            end_ts: Some(ms_to_ts(span.end_ms)),
            repo: hb.project.clone(),
            branch: hb.branch.clone(),
            kind: ActivityKind::Edit,
            ext_id: Some(format!("{}@{}#{}", hb.project, hb.branch, span.start_ms)),
            summary: Some(basename(&hb.entity).to_owned()),
        })
    }
}

/// Current file, without its path: the span's summary.
fn basename(entity: &str) -> &str {
    entity
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(entity)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload `vim-wakatime` posts to `heartbeats.bulk`, with the nulls
    /// clients send for fields they cannot fill.
    const BULK: &str = r#"[
      {"entity": "/home/j/dev/contoso/app/models.py", "type": "file",
       "time": 1756890000.5, "project": "contoso", "branch": "main",
       "language": "Python", "is_write": true, "lines": 32120},
      {"entity": "/home/j/dev/contoso/app/views.py", "type": "file",
       "time": 1756890120.0, "project": "contoso", "branch": null,
       "language": null, "is_write": false}
    ]"#;

    #[test]
    fn parses_documented_bulk_payload() {
        let hbs: Vec<Heartbeat> = serde_json::from_str(BULK).unwrap();
        assert_eq!(hbs.len(), 2);
        assert_eq!(hbs[0].entity, "/home/j/dev/contoso/app/models.py");
        assert_eq!(hbs[0].kind, "file");
        assert_eq!(hbs[0].project, "contoso");
        assert_eq!(hbs[0].branch, "main");
        assert_eq!(hbs[0].language, "Python");
        assert!(hbs[0].is_write);
        assert_eq!(hbs[0].time, 1756890000.5);
        // Nulls read as the field's default, not as a parse error.
        assert_eq!(hbs[1].branch, "");
        assert_eq!(hbs[1].language, "");
        assert!(!hbs[1].is_write);
    }

    fn hb(time: f64, entity: &str) -> Heartbeat {
        Heartbeat {
            entity: entity.to_owned(),
            kind: "file".to_owned(),
            project: "contoso".to_owned(),
            branch: "main".to_owned(),
            time,
            ..Heartbeat::default()
        }
    }

    #[test]
    fn folds_one_span_until_the_gap_then_starts_another() {
        let mut folder = Folder::default();
        let a = folder.fold(&hb(1000.0, "a/models.py"), 0).unwrap();
        assert_eq!(a.kind, ActivityKind::Edit);
        assert_eq!(a.repo, "contoso");
        assert_eq!(a.branch, "main");
        assert_eq!(a.ext_id.as_deref(), Some("contoso@main#1000000"));
        assert_eq!(a.summary.as_deref(), Some("models.py"));
        assert_eq!(a.end_ts, Some(ms_to_ts(1_000_000)));

        // Inside the gap: same ext_id, later end_ts (Upsert refreshes it).
        let b = folder
            .fold(&hb(1000.0 + GAP_SECS as f64, "a/views.py"), 0)
            .unwrap();
        assert_eq!(b.ext_id, a.ext_id);
        assert_eq!(b.ts, a.ts);
        assert_eq!(b.end_ts, Some(ms_to_ts(1_000_000 + GAP_SECS * 1000)));
        assert_eq!(b.summary.as_deref(), Some("views.py"));

        // One second past the gap: a new span.
        let c = folder
            .fold(&hb(1001.0 + 2.0 * GAP_SECS as f64, "a/views.py"), 0)
            .unwrap();
        assert_eq!(
            c.ext_id.as_deref(),
            Some(format!("contoso@main#{}", 1_001_000 + 2 * GAP_SECS * 1000).as_str())
        );
        assert_eq!(c.ts, c.end_ts.unwrap());

        // A different branch is a different span.
        let mut other = hb(1001.0 + 2.0 * GAP_SECS as f64, "a/views.py");
        other.branch = "feature".into();
        let d = folder.fold(&other, 0).unwrap();
        assert_ne!(d.ext_id, c.ext_id);
    }

    #[test]
    fn skips_non_file_heartbeats() {
        let mut folder = Folder::default();
        let mut domain = hb(1000.0, "github.com");
        domain.kind = "domain".into();
        assert!(folder.fold(&domain, 0).is_none());
    }
}
