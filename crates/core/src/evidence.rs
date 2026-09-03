//! Deterministic evidence pulled from window titles and URLs (m27 chunk 5):
//! ticket keys on screen, the working directory a terminal or editor title
//! names, distraction matching, and the status glyphs Claude Code prefixes
//! to terminal titles.

use std::sync::LazyLock;

use regex::Regex;

use crate::sessionizer::{SpanDraft, SpanKind};

/// Claude Code prefixes its terminal title with a spinner/status glyph
/// (✳ ◑ ☐ …) that means nothing to a reader or a label; the leading run of
/// such glyphs is dropped wherever a title or label is shown or quoted.
pub fn strip_glyphs(s: &str) -> &str {
    const STATUS_GLYPHS: &[char] = &[
        '\u{2733}', '\u{273b}', '\u{273d}', '\u{2736}', '\u{2722}', '\u{2749}', '\u{25d0}',
        '\u{25d1}', '\u{25d2}', '\u{25d3}', '\u{2610}', '\u{23fa}', '\u{00b7}', '\u{2731}',
        '\u{2732}',
    ];
    s.trim_start_matches(|c: char| c.is_whitespace() || STATUS_GLYPHS.contains(&c))
}

/// The repo a title names by path: `~/dev/<name>`, `/home/<u>/dev/<name>`,
/// including the `<user>@<host>:~/dev/<name>` shell-prompt form.
pub fn cwd_repo(title: &str) -> Option<String> {
    static CWD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?:~|/home/[^/\s]+)/dev/([A-Za-z0-9_.-]+)").unwrap());
    CWD.captures(title).map(|c| c[1].to_owned())
}

/// Focus time per repo named by titles inside `[lo, hi)`, most first.
pub fn cwd_repos(spans: &[SpanDraft], lo: i64, hi: i64) -> Vec<(String, i64)> {
    let mut ms: Vec<(String, i64)> = Vec::new();
    for s in focus_in(spans, lo, hi) {
        let Some(repo) = cwd_repo(&s.0.title) else {
            continue;
        };
        match ms.iter_mut().find(|(r, _)| *r == repo) {
            Some(e) => e.1 += s.1,
            None => ms.push((repo, s.1)),
        }
    }
    ms.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ms
}

/// A ticket key seen in titles or URLs: minutes on screen and the app that
/// showed it most.
#[derive(Debug, Clone, PartialEq)]
pub struct KeySeen {
    pub key: String,
    pub ms: i64,
    pub app: String,
}

/// Ticket keys (`ticket_regex`) in focus titles/URLs inside `[lo, hi)`, most
/// time first. A span contributes to the first key its title or URL names.
pub fn keys_in_spans(spans: &[SpanDraft], re: &Regex, lo: i64, hi: i64) -> Vec<KeySeen> {
    let mut seen: Vec<(KeySeen, Vec<(String, i64)>)> = Vec::new();
    for (s, ms) in focus_in(spans, lo, hi) {
        let key = re
            .find(&s.title)
            .or_else(|| s.url.as_deref().and_then(|u| re.find(u)))
            .map(|m| m.as_str().to_owned());
        let Some(key) = key else { continue };
        let entry = match seen.iter_mut().find(|(k, _)| k.key == key) {
            Some(e) => e,
            None => {
                seen.push((
                    KeySeen {
                        key,
                        ms: 0,
                        app: String::new(),
                    },
                    Vec::new(),
                ));
                seen.last_mut().expect("just pushed")
            }
        };
        entry.0.ms += ms;
        match entry.1.iter_mut().find(|(a, _)| *a == s.app) {
            Some(a) => a.1 += ms,
            None => entry.1.push((s.app.clone(), ms)),
        }
    }
    let mut out: Vec<KeySeen> = seen
        .into_iter()
        .map(|(mut k, apps)| {
            k.app = apps
                .into_iter()
                .max_by_key(|(_, ms)| *ms)
                .map(|(a, _)| a)
                .unwrap_or_default();
            k
        })
        .collect();
    out.sort_by(|a, b| b.ms.cmp(&a.ms).then_with(|| a.key.cmp(&b.key)));
    out
}

fn focus_in(spans: &[SpanDraft], lo: i64, hi: i64) -> Vec<(&SpanDraft, i64)> {
    spans
        .iter()
        .filter(|s| s.kind == SpanKind::Focus)
        .filter_map(|s| {
            let ms = s.end.as_millisecond().min(hi) - s.start.as_millisecond().max(lo);
            (ms > 0).then_some((s, ms))
        })
        .collect()
}

/// `config.distraction_patterns` compiled; bad patterns are skipped.
pub fn compile_patterns(patterns: &[String]) -> Vec<Regex> {
    patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
}

/// An app/title line the distraction patterns match (app name or title —
/// runs carry no URL, so the site name in the title stands in for the site
/// key that insights match).
pub fn is_distraction(app: &str, title: &str, patterns: &[Regex]) -> bool {
    patterns
        .iter()
        .any(|p| p.is_match(app) || p.is_match(title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;

    fn span(lo: i64, hi: i64, app: &str, title: &str, url: Option<&str>) -> SpanDraft {
        SpanDraft {
            start: ms_to_ts(lo),
            end: ms_to_ts(hi),
            app: app.into(),
            title: title.into(),
            kind: SpanKind::Focus,
            url: url.map(str::to_owned),
        }
    }

    #[test]
    fn cwd_forms() {
        assert_eq!(
            cwd_repo("sam@box:~/dev/mailer").as_deref(),
            Some("mailer")
        );
        assert_eq!(
            cwd_repo("models.py (~/dev/mailer/mailer) - VIM").as_deref(),
            Some("mailer")
        );
        assert_eq!(
            cwd_repo("/home/james/dev/chronicle - fish").as_deref(),
            Some("chronicle")
        );
        assert_eq!(cwd_repo("GitHub - chronicle"), None);
        assert_eq!(strip_glyphs("\u{2733} Complete m25"), "Complete m25");
        assert_eq!(strip_glyphs("plain"), "plain");
    }

    #[test]
    fn keys_and_cwd_aggregate_by_time() {
        let re = Regex::new(r"[A-Z][A-Z0-9]+-\d+").unwrap();
        let spans = vec![
            span(
                0,
                9 * 60_000,
                "chrome",
                "[ACME-11382] SMS rules - Jira",
                None,
            ),
            span(
                9 * 60_000,
                10 * 60_000,
                "chrome",
                "Board",
                Some("https://x.atlassian.net/browse/ACME-11374"),
            ),
            span(
                10 * 60_000,
                12 * 60_000,
                "Terminator",
                "sam@box:~/dev/mailer",
                None,
            ),
            span(
                12 * 60_000,
                13 * 60_000,
                "Terminator",
                "ACME-11382 tests",
                None,
            ),
        ];
        let keys = keys_in_spans(&spans, &re, 0, 13 * 60_000);
        assert_eq!(keys.len(), 2);
        assert_eq!(
            (keys[0].key.as_str(), keys[0].ms, keys[0].app.as_str()),
            ("ACME-11382", 10 * 60_000, "chrome")
        );
        assert_eq!(keys[1].key, "ACME-11374");
        assert_eq!(
            cwd_repos(&spans, 0, 13 * 60_000),
            vec![("mailer".to_owned(), 2 * 60_000)]
        );
        let pats = compile_patterns(&["(?i)youtube".into(), "[".into()]);
        assert_eq!(pats.len(), 1);
        assert!(is_distraction("firefox", "Cats - YouTube", &pats));
        assert!(!is_distraction("Terminator", "ACME-11382 tests", &pats));
    }
}
