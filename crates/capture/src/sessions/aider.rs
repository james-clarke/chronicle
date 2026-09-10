//! Aider: `.aider.chat.history.md` at a repo root. No fixed root of its
//! own — `roots` is empty so `AiSessionProvider` calls `scan` once per
//! configured cwd candidate (repos, worktrees) instead.
//!
//! The file has no per-turn timestamps, only a header
//! (`# aider chat started at 2026-09-09 10:14:32`, local time) that opens
//! the session; the header time stands for every prompt except the last,
//! which gets the file's mtime (interpolating between them buys nothing).

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;

use super::{Rec, SessionFormat, Source, clip};

pub struct Aider;

const HEADER_PREFIX: &str = "# aider chat started at ";

impl SessionFormat for Aider {
    fn name(&self) -> &'static str {
        "aider"
    }

    fn roots(&self, _home: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    /// `root` here is one cwd candidate, handed to us by the provider.
    fn scan(&self, root: &Path) -> Vec<Source> {
        let path = root.join(".aider.chat.history.md");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let Some(header_ts) = header_ts(&text) else {
            return Vec::new();
        };
        let base = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let session_id = format!("{base}@{}", header_ts.as_millisecond());
        vec![Source {
            path,
            session_id,
            cwd_hint: Some(root.to_string_lossy().into_owned()),
        }]
    }

    /// The cursor packs two counts already emitted: prompts in the low 32
    /// bits, applied-edit paths in the high 32 (the file is re-read whole
    /// each time; there is no line-offset cursor that survives edits to
    /// earlier turns the way a strictly-append log's would).
    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let Ok(text) = std::fs::read_to_string(&source.path) else {
            return (Vec::new(), cursor);
        };
        let Some(header) = header_ts(&text) else {
            return (Vec::new(), cursor);
        };
        let mtime = mtime_ts(&source.path).unwrap_or(header);
        let last_ts = if mtime > header { mtime } else { header };

        let mut prompts = Vec::new();
        let mut paths: Vec<String> = Vec::new();
        for line in text.lines() {
            if let Some(p) = line.strip_prefix("#### ") {
                if let Some(c) = clip(p) {
                    prompts.push(c);
                }
            } else if let Some(rest) = line.strip_prefix("> Applying edit to ") {
                let p = rest.trim().trim_end_matches('.').to_owned();
                if !p.is_empty() && !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }

        let prompts_seen = (cursor & 0xffff_ffff) as usize;
        let paths_seen = (cursor >> 32) as usize;
        let mut out = Vec::new();
        let n = prompts.len();
        for (i, p) in prompts.iter().enumerate().skip(prompts_seen) {
            let ts = if i + 1 == n { last_ts } else { header };
            out.push(Rec {
                ts,
                cwd: None,
                branch: None,
                prompt: Some(p.clone()),
                paths: Vec::new(),
                write: false,
                title: None,
                session_id: source.session_id.clone(),
            });
        }
        if paths.len() > paths_seen {
            out.push(Rec {
                ts: last_ts,
                cwd: None,
                branch: None,
                prompt: None,
                paths: paths[paths_seen..].to_vec(),
                write: true,
                title: None,
                session_id: source.session_id.clone(),
            });
        }
        let next_cursor = (n as u64) | ((paths.len() as u64) << 32);
        (out, next_cursor)
    }
}

fn header_ts(text: &str) -> Option<Timestamp> {
    let line = text.lines().find(|l| l.starts_with(HEADER_PREFIX))?;
    let raw = line[HEADER_PREFIX.len()..].trim();
    let iso = raw.replacen(' ', "T", 1);
    let dt: DateTime = iso.parse().ok()?;
    dt.to_zoned(TimeZone::system()).ok().map(|z| z.timestamp())
}

fn mtime_ts(path: &Path) -> Option<Timestamp> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()
        .and_then(|t| Timestamp::try_from(t).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/sessions/aider"
        ))
    }

    #[test]
    fn scans_and_reads_a_chat_history() {
        let root = fixture_dir().join("repo");
        let sources = Aider.scan(&root);
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0].cwd_hint.as_deref(), Some(root.to_str().unwrap()));
        assert!(sources[0].session_id.starts_with("repo@"));

        let (recs, cursor) = Aider.read(&sources[0], 0);
        assert!(cursor > 0);
        let first = recs.iter().find(|r| r.prompt.is_some()).unwrap();
        assert_eq!(first.prompt.as_deref(), Some("add a retry to the fetch"));

        let write = recs.iter().find(|r| !r.paths.is_empty()).unwrap();
        assert_eq!(write.paths, vec!["src/fetch.ts".to_string()]);
        assert!(write.write);

        let (more, _) = Aider.read(&sources[0], cursor);
        assert!(more.is_empty());
    }
}
