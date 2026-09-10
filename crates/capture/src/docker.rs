//! Docker Compose poller (m37 chunk 1): every 60 s, `docker ps` reports
//! each running container's compose working directory — a `cwd` event per
//! container, keyed by place (shell_hook.rs's `place_of`), so a stack's
//! directory is a place while it runs, the same anchor shell_hook.rs and
//! tmux.rs give a shell or pane in it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::shell_hook::place_of;
use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);
const WORKING_DIR_LABEL: &str = "com.docker.compose.project.working_dir";
const FORMAT: &str = "{{.ID}}\t{{.Label \"com.docker.compose.project.working_dir\"}}\t{{.Label \"com.docker.compose.project\"}}";

pub struct DockerProvider {
    docker: PathBuf,
    /// First-seen time per `ext_id`, so a repeat upserts a longer span;
    /// replaced each poll so a container not seen this round is dropped.
    seen: HashMap<String, Timestamp>,
    /// Last stderr line warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl DockerProvider {
    /// `Some` when `docker` resolves on `PATH` (a pure env lookup, no
    /// shelling out).
    pub fn detect() -> Option<Self> {
        let docker = chronicle_core::config::resolve_command("docker");
        if !docker.contains('/') {
            return None;
        }
        Some(Self {
            docker: PathBuf::from(docker),
            seen: HashMap::new(),
            last_err: None,
        })
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let result = Command::new(&self.docker)
            .args([
                "ps",
                "--filter",
                &format!("label={WORKING_DIR_LABEL}"),
                "--format",
                FORMAT,
            ])
            .output();
        let out = match result {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or("docker ps failed")
                    .to_owned();
                self.warn_once(err);
                self.seen.clear();
                return Vec::new();
            }
            Err(e) => {
                self.warn_once(e.to_string());
                self.seen.clear();
                return Vec::new();
            }
        };
        self.last_err = None;
        let now = Timestamp::now();
        let mut next_seen = HashMap::new();
        let mut events = Vec::new();
        for c in parse_containers(&String::from_utf8_lossy(&out.stdout)) {
            let place = place_of(Path::new(&c.working_dir));
            let ext_id = format!("docker:{}:{place}", c.id);
            let first = self.seen.get(&ext_id).copied().unwrap_or(now);
            next_seen.insert(ext_id.clone(), first);
            events.push(ActivityEvent {
                ts: first,
                end_ts: Some(now),
                repo: place,
                branch: String::new(),
                kind: ActivityKind::Cwd,
                ext_id: Some(ext_id),
                summary: Some(c.project).filter(|p| !p.is_empty()),
                detail: None,
            });
        }
        self.seen = next_seen;
        events
    }

    fn warn_once(&mut self, err: String) {
        if self.last_err.as_deref() != Some(err.as_str()) {
            tracing::warn!("docker ps: {err}");
            self.last_err = Some(err);
        }
    }
}

impl FocusProvider for DockerProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Container {
    id: String,
    working_dir: String,
    project: String,
}

/// One `Container` per line of `docker ps --format` output ([`FORMAT`]).
/// A row missing the working-dir label (the `--filter` should have
/// excluded it, but a stale label edge case is possible) or malformed is
/// skipped.
fn parse_containers(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let working_dir = (*f.get(1)?).to_owned();
            if working_dir.is_empty() {
                return None;
            }
            Some(Container {
                id: (*f.first()?).to_owned(),
                working_dir,
                project: f.get(2).copied().unwrap_or("").to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "abc123\t/home/x/dev/chronicle\tchronicle\n\
def456\t/home/x/dev/other-stack\tother-stack\n\
ghi789\t\t\n";

    #[test]
    fn parses_containers_and_skips_rows_without_a_working_dir() {
        let containers = parse_containers(FIXTURE);
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].id, "abc123");
        assert_eq!(containers[0].working_dir, "/home/x/dev/chronicle");
        assert_eq!(containers[0].project, "chronicle");
        assert_eq!(containers[1].id, "def456");
        assert_eq!(containers[1].project, "other-stack");
    }
}
