//! Notes collector (m32 chunk 5): the entries of every configured repo's
//! `.remember/today-*.md`, one `note` row each. A file is re-read only when
//! its size or mtime moves; the first poll reads them all, so a fresh daemon
//! backfills every note it can see. Ground truth for narratives: what was
//! written down, when, on which branch. The note body is stored clipped;
//! nothing else in the repo is read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);
/// The summary is the body on one line, this long.
const SUMMARY_CHARS: usize = 300;
/// The detail keeps this much of the body.
const BODY_CHARS: usize = 2000;

pub struct NotesProvider {
    dirs: Vec<(String, PathBuf)>,
    seen: HashMap<PathBuf, (SystemTime, u64)>,
    tz: TimeZone,
}

impl NotesProvider {
    /// `repos` are working trees; each one's `.remember` is watched under
    /// the repo's directory name.
    pub fn new(repos: &[PathBuf]) -> Self {
        let dirs = repos
            .iter()
            .map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                (name, p.join(".remember"))
            })
            .collect();
        Self {
            dirs,
            seen: HashMap::new(),
            tz: TimeZone::system(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// Every note in a file that changed since the last poll.
    pub fn poll(&mut self) -> Vec<ActivityEvent> {
        let mut out = Vec::new();
        for (repo, dir) in &self.dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(date) = note_file_date(&path) else {
                    continue;
                };
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                let stamp = (
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                );
                if self.seen.get(&path) == Some(&stamp) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                self.seen.insert(path.clone(), stamp);
                let file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.extend(parse_notes(&text, repo, date, &file, &self.tz));
            }
        }
        out
    }
}

impl FocusProvider for NotesProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

/// The day a `today-YYYY-MM-DD.md` (or `.done.md`) file covers.
fn note_file_date(path: &Path) -> Option<Date> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("today-")?.strip_suffix(".md")?;
    rest.get(..10)?.parse().ok()
}

/// `## HH:MM | branch` or `## HH:MM-HH:MM | branch` (en dash too) heads
/// an entry; the lines until the next head are its body.
pub fn parse_notes(
    text: &str,
    repo: &str,
    date: Date,
    file: &str,
    tz: &TimeZone,
) -> Vec<ActivityEvent> {
    let mut out: Vec<ActivityEvent> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    let mut head: Option<(Timestamp, Option<Timestamp>, String)> = None;
    let flush = |head: &mut Option<(Timestamp, Option<Timestamp>, String)>,
                 body: &mut Vec<&str>,
                 out: &mut Vec<ActivityEvent>| {
        if let Some((ts, end_ts, branch)) = head.take() {
            let text = body.join("\n");
            let text = text.trim();
            if !text.is_empty() {
                let hm = ts.to_zoned(tz.clone()).strftime("%H:%M").to_string();
                let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                out.push(ActivityEvent {
                    ts,
                    end_ts,
                    repo: repo.to_owned(),
                    branch,
                    kind: ActivityKind::Note,
                    ext_id: Some(format!("note:{repo}:{date}T{hm}")),
                    summary: Some(clip(&one_line, SUMMARY_CHARS)),
                    detail: Some(
                        serde_json::json!({ "body": clip(text, BODY_CHARS), "file": file })
                            .to_string(),
                    ),
                });
            }
        }
        body.clear();
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            flush(&mut head, &mut body, &mut out);
            let (times, branch) = rest.split_once('|').unwrap_or((rest, ""));
            let mut times = times
                .split(['-', '\u{2013}', '\u{2014}'])
                .map(str::trim)
                .filter_map(|t| local_time(date, t, tz));
            if let Some(ts) = times.next() {
                head = Some((ts, times.next(), branch.trim().to_owned()));
            }
        } else if head.is_some() {
            body.push(line);
        }
    }
    flush(&mut head, &mut body, &mut out);
    out
}

fn local_time(date: Date, hm: &str, tz: &TimeZone) -> Option<Timestamp> {
    let (h, m) = hm.split_once(':')?;
    let (h, m): (i8, i8) = (h.parse().ok()?, m.parse().ok()?);
    let zoned: Zoned = date.at(h, m, 0, 0).to_zoned(tz.clone()).ok()?;
    Some(zoned.timestamp())
}

fn clip(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "## 08:45-09:35 | main\n\
        theme foundation; 85 tests\n\
        ## 11:13 | feat/x\n\
        Audited spacing.\n\
        Second line.\n\
        ## 11:44\u{2013}12:17 | main\n\
        ch1\u{2013}5 done\n\
        ## bogus | main\n\
        not a note\n";

    #[test]
    fn heads_become_rows_with_local_times() {
        let tz = TimeZone::UTC;
        let date: Date = "2026-09-02".parse().unwrap();
        let got = parse_notes(FILE, "app", date, "today-2026-09-02.md", &tz);
        assert_eq!(got.len(), 3, "{got:?}");
        let first = &got[0];
        assert_eq!(first.kind, ActivityKind::Note);
        assert_eq!(first.repo, "app");
        assert_eq!(first.branch, "main");
        assert_eq!(
            first.ts,
            "2026-09-02T08:45:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(
            first.end_ts,
            Some("2026-09-02T09:35:00Z".parse::<Timestamp>().unwrap())
        );
        assert_eq!(first.ext_id.as_deref(), Some("note:app:2026-09-02T08:45"));
        assert_eq!(first.summary.as_deref(), Some("theme foundation; 85 tests"));
        let second = &got[1];
        assert_eq!(second.branch, "feat/x");
        assert_eq!(second.end_ts, None);
        assert_eq!(
            second.summary.as_deref(),
            Some("Audited spacing. Second line.")
        );
        let d: serde_json::Value = serde_json::from_str(second.detail.as_deref().unwrap()).unwrap();
        assert_eq!(d["body"], "Audited spacing.\nSecond line.");
        assert_eq!(d["file"], "today-2026-09-02.md");
        assert_eq!(got[2].ext_id.as_deref(), Some("note:app:2026-09-02T11:44"));
        assert!(got[2].end_ts.is_some());
    }

    #[test]
    fn file_names_carry_the_date() {
        assert_eq!(
            note_file_date(Path::new("/r/.remember/today-2026-09-02.md")),
            Some("2026-09-02".parse().unwrap())
        );
        assert_eq!(
            note_file_date(Path::new("/r/.remember/today-2026-09-02.done.md")),
            Some("2026-09-02".parse().unwrap())
        );
        assert_eq!(note_file_date(Path::new("/r/.remember/now.md")), None);
        assert_eq!(note_file_date(Path::new("/r/.remember/recent.md")), None);
    }

    #[test]
    fn poll_reads_a_file_once_until_it_moves() {
        let root = std::env::temp_dir().join(format!("chronicle-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("app");
        std::fs::create_dir_all(repo.join(".remember")).unwrap();
        let path = repo.join(".remember").join("today-2026-09-02.md");
        std::fs::write(&path, "## 10:00 | main\nfirst\n").unwrap();
        std::fs::write(
            repo.join(".remember").join("now.md"),
            "## 10:05 | main\nbuffer\n",
        )
        .unwrap();
        let mut p = NotesProvider::new(std::slice::from_ref(&repo));
        let got = p.poll();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].summary.as_deref(), Some("first"));
        assert!(p.poll().is_empty(), "nothing moved");
        std::fs::write(&path, "## 10:00 | main\nfirst\n## 10:30 | main\nsecond\n").unwrap();
        let got = p.poll();
        assert_eq!(got.len(), 2, "{got:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
