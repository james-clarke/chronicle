//! GitHub PR poller (m22): asks `gh search prs` for the user's authored and
//! reviewed pull requests updated since yesterday and emits one `pr_*` marker
//! per `(pr, updatedAt)`. Uses the user's existing `gh auth` — no token in
//! Chronicle's config. Two searches per poll against a 30/min search limit.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::{SignedDuration, Timestamp, Zoned};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(300);
const LIMIT: &str = "30";
const FIELDS: &str = "number,title,url,state,updatedAt,repository";

pub struct GitHubProvider {
    gh: PathBuf,
    /// Last stderr line warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl GitHubProvider {
    pub fn new(gh: PathBuf) -> Self {
        Self { gh, last_err: None }
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let since = (Zoned::now() - SignedDuration::from_hours(24))
            .strftime("%Y-%m-%d")
            .to_string();
        let mut out = Vec::new();
        for (kind, who) in [
            (ActivityKind::PrAuthored, "--author=@me"),
            (ActivityKind::PrReviewed, "--reviewed-by=@me"),
        ] {
            let result = Command::new(&self.gh)
                .args([
                    "search",
                    "prs",
                    who,
                    &format!("--updated=>={since}"),
                    "--json",
                    FIELDS,
                    "--limit",
                    LIMIT,
                ])
                .output();
            match result {
                Ok(o) if o.status.success() => {
                    out.extend(parse_prs(&String::from_utf8_lossy(&o.stdout), kind));
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .next()
                        .unwrap_or("non-zero exit")
                        .to_owned();
                    if self.last_err.as_deref() != Some(err.as_str()) {
                        tracing::warn!("gh search prs {who}: {err}");
                        self.last_err = Some(err);
                    }
                }
                Err(e) => {
                    let err = e.to_string();
                    if self.last_err.as_deref() != Some(err.as_str()) {
                        tracing::warn!("gh could not run: {err}");
                        self.last_err = Some(err);
                    }
                }
            }
        }
        out
    }
}

impl FocusProvider for GitHubProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

/// `gh search prs --json` output → markers. Rows missing a field are skipped.
fn parse_prs(json: &str, kind: ActivityKind) -> Vec<ActivityEvent> {
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(json) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            let ts: Timestamp = r.get("updatedAt")?.as_str()?.parse().ok()?;
            let url = r.get("url")?.as_str()?.to_owned();
            let number = r.get("number")?.as_i64()?;
            let title = r.get("title")?.as_str()?.trim();
            let state = r.get("state").and_then(|s| s.as_str()).unwrap_or("");
            let repo = r
                .get("repository")?
                .get("nameWithOwner")?
                .as_str()?
                .rsplit('/')
                .next()?
                .to_owned();
            let mut summary = format!("#{number} {title}");
            if !state.is_empty() {
                summary.push_str(" \u{b7} ");
                summary.push_str(&state.to_ascii_lowercase());
            }
            Some(ActivityEvent {
                ts,
                end_ts: None,
                repo,
                branch: String::new(),
                kind,
                ext_id: Some(url),
                summary: Some(summary),
                detail: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_search_rows_to_markers() {
        let json = r#"[
          {"number":40,"repository":{"nameWithOwner":"org/acme-ai-agent-backend"},"title":"ACME-11342: store GIFs","state":"open","updatedAt":"2026-09-02T15:30:32Z","url":"https://github.com/org/acme-ai-agent-backend/pull/40"},
          {"number":9441,"repository":{"nameWithOwner":"org/contoso"},"title":"Acme 10770","state":"merged","updatedAt":"2026-09-01T18:22:19Z","url":"https://github.com/org/contoso/pull/9441"},
          {"number":1,"title":"no repo","state":"open","updatedAt":"2026-09-01T18:22:19Z","url":"u"}
        ]"#;
        let got = parse_prs(json, ActivityKind::PrAuthored);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].repo, "acme-ai-agent-backend");
        assert_eq!(got[0].kind, ActivityKind::PrAuthored);
        assert_eq!(
            got[0].summary.as_deref(),
            Some("#40 ACME-11342: store GIFs \u{b7} open")
        );
        assert_eq!(
            got[0].ext_id.as_deref(),
            Some("https://github.com/org/acme-ai-agent-backend/pull/40")
        );
        assert_eq!(
            got[0].ts,
            "2026-09-02T15:30:32Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            got[1].summary.as_deref(),
            Some("#9441 Acme 10770 \u{b7} merged")
        );
        assert!(parse_prs("not json", ActivityKind::PrReviewed).is_empty());
    }
}
