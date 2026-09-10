//! GitLab MR poller (m37 chunk 2): asks `glab mr list` for the user's
//! assigned and reviewed merge requests, updated recently, and emits one
//! `pr_*` marker per `(mr, updated_at)`. Uses the user's existing
//! `glab auth` — no token in Chronicle's config. `glab mr list` needs a repo
//! context, so one `-R host/group/repo` pair of calls runs per configured
//! git repo whose remote resolves to a GitLab host.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(300);

pub struct GitlabProvider {
    glab: PathBuf,
    /// `host/group/repo` remotes to query, one `-R` per poll pair.
    repos: Vec<String>,
    /// Last stderr line warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl GitlabProvider {
    /// Resolves `glab` on PATH; `None` when it isn't installed (opt-in,
    /// never load-bearing).
    pub fn detect() -> Option<PathBuf> {
        let glab = chronicle_core::config::resolve_command("glab");
        glab.contains('/').then(|| PathBuf::from(glab))
    }

    /// `repos` are configured git work dirs; only those whose remote
    /// resolves to a GitLab host are kept.
    pub fn new(glab: PathBuf, repos: &[PathBuf]) -> Self {
        let repos = repos
            .iter()
            .filter_map(|p| chronicle_core::project::remote_of(p))
            .filter(|remote| is_gitlab_remote(remote))
            .collect();
        Self {
            glab,
            repos,
            last_err: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let mut out = Vec::new();
        for repo in &self.repos {
            for (kind, who) in [
                (ActivityKind::PrAuthored, "--assignee=@me"),
                (ActivityKind::PrReviewed, "--reviewer=@me"),
            ] {
                let result = Command::new(&self.glab)
                    .args(["mr", "list", "-R", repo, who, "--output", "json"])
                    .output();
                match result {
                    Ok(o) if o.status.success() => {
                        out.extend(parse_mrs(&String::from_utf8_lossy(&o.stdout), kind));
                    }
                    Ok(o) => {
                        let err = String::from_utf8_lossy(&o.stderr)
                            .lines()
                            .next()
                            .unwrap_or("non-zero exit")
                            .to_owned();
                        if self.last_err.as_deref() != Some(err.as_str()) {
                            tracing::warn!("glab mr list -R {repo} {who}: {err}");
                            self.last_err = Some(err);
                        }
                    }
                    Err(e) => {
                        let err = e.to_string();
                        if self.last_err.as_deref() != Some(err.as_str()) {
                            tracing::warn!("glab could not run: {err}");
                            self.last_err = Some(err);
                        }
                    }
                }
            }
        }
        out
    }
}

impl FocusProvider for GitlabProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

/// `host/group/repo` (as returned by `chronicle_core::project::remote_of`)
/// is a GitLab remote when its host segment contains "gitlab" — covers
/// gitlab.com and typical self-managed hosts (`gitlab.example.com`).
fn is_gitlab_remote(remote: &str) -> bool {
    remote
        .split('/')
        .next()
        .is_some_and(|host| host.contains("gitlab"))
}

/// `glab mr list --output json` → markers. Rows missing a field are skipped.
fn parse_mrs(json: &str, kind: ActivityKind) -> Vec<ActivityEvent> {
    let Ok(rows) = serde_json::from_str::<Vec<serde_json::Value>>(json) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            let ts: Timestamp = r.get("updated_at")?.as_str()?.parse().ok()?;
            let url = r.get("web_url")?.as_str()?.to_owned();
            let iid = r.get("iid")?.as_i64()?;
            let title = r.get("title")?.as_str()?.trim();
            let state = r.get("state").and_then(|s| s.as_str()).unwrap_or("");
            let repo = mr_repo(r, &url)?;
            let mut summary = format!("!{iid} {title}");
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

/// The repo name is the last path segment of the project path: from
/// `references.full` (`group/repo!12`) when present, else from the MR's
/// `web_url` (`.../group/repo/-/merge_requests/12`).
fn mr_repo(row: &serde_json::Value, web_url: &str) -> Option<String> {
    if let Some(full) = row
        .get("references")
        .and_then(|r| r.get("full"))
        .and_then(|f| f.as_str())
    {
        let project = full.split('!').next()?;
        return project.rsplit('/').next().map(str::to_owned);
    }
    let path = web_url.split("/-/merge_requests").next()?;
    path.rsplit('/').next().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_mr_list_rows_to_markers() {
        let json = r#"[
          {"iid":12,"title":"ACME-1: fix thing","state":"opened","updated_at":"2026-09-02T15:30:32Z","web_url":"https://gitlab.com/group/repo/-/merge_requests/12","references":{"full":"group/repo!12"}},
          {"iid":3,"title":"no references","state":"merged","updated_at":"2026-09-01T18:22:19Z","web_url":"https://gitlab.com/other-group/other-repo/-/merge_requests/3"},
          {"iid":9,"title":"missing url","state":"opened","updated_at":"2026-09-01T18:22:19Z"}
        ]"#;
        let got = parse_mrs(json, ActivityKind::PrAuthored);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].repo, "repo");
        assert_eq!(got[0].kind, ActivityKind::PrAuthored);
        assert_eq!(
            got[0].summary.as_deref(),
            Some("!12 ACME-1: fix thing \u{b7} opened")
        );
        assert_eq!(
            got[0].ext_id.as_deref(),
            Some("https://gitlab.com/group/repo/-/merge_requests/12")
        );
        assert_eq!(
            got[0].ts,
            "2026-09-02T15:30:32Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(got[1].repo, "other-repo");
        assert_eq!(
            got[1].summary.as_deref(),
            Some("!3 no references \u{b7} merged")
        );
        assert!(parse_mrs("not json", ActivityKind::PrReviewed).is_empty());
    }

    #[test]
    fn is_gitlab_remote_matches_host_segment() {
        assert!(is_gitlab_remote("gitlab.com/group/repo"));
        assert!(is_gitlab_remote("gitlab.example.com/group/repo"));
        assert!(!is_gitlab_remote("github.com/org/repo"));
    }
}
