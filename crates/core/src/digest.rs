//! Batch spans → deterministic digest text for the derivation prompt.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use jiff::tz::TimeZone;

use crate::sessionizer::{SpanDraft, SpanKind};
use crate::types::{Correction, OpenTask, VcsEvent, VcsKind};

pub const MAX_TOKENS: usize = 3000;

/// Rough heuristic; the real tokenizer lives in the derive worker.
pub fn approx_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

pub fn build_digest(
    spans: &[SpanDraft],
    tz: &TimeZone,
    open_tasks: &[OpenTask],
    corrections: &[Correction],
    vcs: &[VcsEvent],
    mcp_context: Option<&str>,
) -> String {
    for (apps_cap, title_chars) in [(8, 120), (6, 80), (4, 48), (3, 24)] {
        let out = render(
            spans,
            tz,
            open_tasks,
            corrections,
            vcs,
            mcp_context,
            apps_cap,
            title_chars,
        );
        if approx_tokens(&out) <= MAX_TOKENS {
            return out;
        }
    }
    let mut out = render(spans, tz, open_tasks, corrections, vcs, mcp_context, 3, 24);
    let mut cut = (MAX_TOKENS * 4).min(out.len());
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    out
}

#[allow(clippy::too_many_arguments)]
fn render(
    spans: &[SpanDraft],
    tz: &TimeZone,
    open_tasks: &[OpenTask],
    corrections: &[Correction],
    vcs: &[VcsEvent],
    mcp_context: Option<&str>,
    apps_cap: usize,
    title_chars: usize,
) -> String {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
        return String::new();
    };

    let mut active_ms = 0i64;
    let mut afk_ms = 0i64;
    let mut switches = 0usize;
    let mut app_ms: HashMap<&str, i64> = HashMap::new();
    for span in spans {
        let dur = span.duration_ms();
        match span.kind {
            SpanKind::Focus => {
                active_ms += dur;
                switches += 1;
                *app_ms.entry(&span.app).or_default() += dur;
            }
            SpanKind::ContextSwitching => {
                active_ms += dur;
                switches += 1;
            }
            SpanKind::Afk => afk_ms += dur,
        }
    }

    let mut apps: Vec<(&str, i64)> = app_ms.into_iter().collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    apps.truncate(apps_cap);

    let start = first.start.to_zoned(tz.clone());
    let end = last.end.to_zoned(tz.clone());
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Activity {} {}\u{2013}{} ({})",
        start.strftime("%Y-%m-%d"),
        start.strftime("%H:%M"),
        end.strftime("%H:%M"),
        tz.iana_name().unwrap_or("local"),
    );
    let _ = writeln!(
        out,
        "active {} \u{b7} afk {} \u{b7} {switches} switches",
        fmt_dur(active_ms),
        fmt_dur(afk_ms),
    );

    let _ = writeln!(out, "\n## Apps by time");
    for (app, ms) in &apps {
        let _ = writeln!(out, "- {app}: {}", fmt_dur(*ms));
    }

    // Domain + first path segment groups browser time per site regardless of
    // page-title churn. Omitted entirely when no spans carry URLs, so
    // pre-M6 fixtures and their goldens are unchanged.
    let mut site_ms: HashMap<String, i64> = HashMap::new();
    for span in spans {
        if span.kind == SpanKind::Focus
            && let Some(url) = &span.url
        {
            *site_ms.entry(site_key(url)).or_default() += span.duration_ms();
        }
    }
    if !site_ms.is_empty() {
        let mut sites: Vec<(String, i64)> = site_ms.into_iter().collect();
        sites.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        sites.truncate(apps_cap);
        let _ = writeln!(out, "\n## Sites by time");
        for (site, ms) in &sites {
            let _ = writeln!(out, "- {site}: {}", fmt_dur(*ms));
        }
    }

    // Branch names and commit subjects are the strongest task-identity signal
    // in the window. Last 10 inside it; omitted when empty so git-less
    // digests (and their goldens) are unchanged.
    let win_lo = first.start.as_millisecond();
    let win_hi = last.end.as_millisecond();
    let in_window: Vec<&VcsEvent> = vcs
        .iter()
        .filter(|v| {
            let ms = v.ts.as_millisecond();
            ms >= win_lo && ms < win_hi
        })
        .collect();
    if !in_window.is_empty() {
        let _ = writeln!(out, "\n## Git activity");
        let skip = in_window.len().saturating_sub(10);
        for v in &in_window[skip..] {
            let hm = v.ts.to_zoned(tz.clone()).strftime("%H:%M");
            match v.kind {
                VcsKind::Checkout => {
                    let _ = writeln!(out, "- {hm} checkout {} \u{2192} {}", v.repo, v.branch);
                }
                VcsKind::Commit => {
                    let _ = write!(out, "- {hm} commit {}", v.repo);
                    if let Some(s) = &v.summary {
                        let _ = write!(out, " \"{}\"", clip(s, title_chars));
                    }
                    let _ = writeln!(out, " [{}]", v.branch);
                }
            }
        }
    }

    // Chronological view: without it the model has only aggregates and must
    // guess task offsets. Dominant activity per minute, run-length encoded —
    // smooths sub-minute interleaving into readable stretches while every
    // minute stays covered; sliver time still counts in the stats above.
    let win_start = first.start.as_millisecond();
    let win_end = last.end.as_millisecond();
    let mins = ((win_end - win_start + 59_999) / 60_000).max(0) as usize;
    let mut buckets: Vec<BTreeMap<(u8, &str, &str), i64>> = vec![BTreeMap::new(); mins];
    for span in spans {
        let key = match span.kind {
            SpanKind::Focus => (0u8, span.app.as_str(), span.title.as_str()),
            SpanKind::ContextSwitching => (1, "", ""),
            SpanKind::Afk => (2, "", ""),
        };
        let s_ms = span.start.as_millisecond() - win_start;
        let e_ms = span.end.as_millisecond() - win_start;
        let mut m = s_ms / 60_000;
        while m * 60_000 < e_ms && (m as usize) < mins {
            let overlap = e_ms.min((m + 1) * 60_000) - s_ms.max(m * 60_000);
            if overlap > 0 {
                *buckets[m as usize].entry(key).or_default() += overlap;
            }
            m += 1;
        }
    }
    // Dual-entry minutes: a focus runner-up holding ≥ ~25% of a minute renders
    // alongside the dominant activity, so interleaved work stays visible
    // instead of being hidden by dominant-minute RLE.
    const RUNNER_UP_MS: i64 = 15_000;
    type MinuteKey<'a> = (u8, &'a str, &'a str);
    let minute: Vec<Option<(MinuteKey, Option<MinuteKey>)>> = buckets
        .iter()
        .map(|b| {
            let dom = b.iter().max_by_key(|(_, ms)| *ms).map(|(k, _)| *k)?;
            let runner = b
                .iter()
                .filter(|(k, ms)| **k != dom && k.0 == 0 && **ms >= RUNNER_UP_MS)
                .max_by_key(|(_, ms)| *ms)
                .map(|(k, _)| *k)
                .filter(|_| dom.0 == 0);
            Some((dom, runner))
        })
        .collect();
    let _ = writeln!(out, "\n## Timeline (minute offsets from window start)");
    let mut m = 0usize;
    while m < mins {
        let Some((dom, runner)) = minute[m] else {
            m += 1;
            continue;
        };
        let mut end = m + 1;
        while end < mins && minute[end] == Some((dom, runner)) {
            end += 1;
        }
        match dom.0 {
            0 => {
                let _ = write!(
                    out,
                    "- {m}\u{2013}{end}m {}: {}",
                    dom.1,
                    clip(dom.2, title_chars)
                );
                // " + " on purpose: real window titles contain " | ".
                if let Some(r) = runner {
                    let _ = write!(out, " + {}: {}", r.1, clip(r.2, title_chars));
                }
                out.push('\n');
            }
            1 => {
                let _ = writeln!(out, "- {m}\u{2013}{end}m (rapid app switching)");
            }
            _ => {
                let _ = writeln!(out, "- {m}\u{2013}{end}m afk");
            }
        }
        m = end;
    }

    // Numbered so interval output can link by index ("ref"). Omitted when
    // empty, so open-task-free digests (and their goldens) are unchanged.
    if !open_tasks.is_empty() {
        let _ = writeln!(out, "\n## Open tasks");
        for (i, t) in open_tasks.iter().enumerate() {
            let _ = write!(out, "{}. {}", i + 1, t.label);
            if let Some(p) = &t.project {
                let _ = write!(out, " [{p}]");
            }
            if t.declared {
                let _ = write!(out, " (declared)");
            }
            out.push('\n');
        }
    }

    // Omitted entirely when empty so correction-free digests (and their
    // goldens) are unchanged.
    if !corrections.is_empty() {
        let _ = writeln!(out, "\n## Past corrections (user renamed similar work)");
        for c in corrections {
            let _ = write!(out, "- \"{}\" \u{2192} \"{}\"", c.old_label, c.new_label);
            if c.old_project != c.new_project {
                let _ = write!(
                    out,
                    " (project: {} \u{2192} {})",
                    c.old_project.as_deref().unwrap_or("none"),
                    c.new_project.as_deref().unwrap_or("none"),
                );
            }
            out.push('\n');
        }
    }

    // Pre-truncated (≤ ~800 tokens) by the MCP gatherer; untrusted text, the
    // GBNF grammar is the containment. Omitted when absent so MCP-less
    // digests (and their goldens) are unchanged.
    if let Some(mcp) = mcp_context.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "\n## Workspace context");
        let _ = writeln!(out, "{mcp}");
    }
    out
}

/// `https://docs.rs/axum/latest/` → `docs.rs/axum`.
pub(crate) fn site_key(url: &str) -> String {
    let host = crate::sessionizer::domain(url);
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split_once('/').map_or("", |(_, p)| p);
    match path.split(['/', '?', '#']).next().filter(|s| !s.is_empty()) {
        Some(seg) => format!("{host}/{seg}"),
        None => host.to_owned(),
    }
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

pub(crate) fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{sec:02}s")
    } else {
        format!("{sec}s")
    }
}
