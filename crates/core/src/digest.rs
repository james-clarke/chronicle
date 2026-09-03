//! Batch spans → deterministic digest text for the derivation prompt.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use jiff::tz::TimeZone;

use crate::sessionizer::{SpanDraft, SpanKind};
use crate::storage::Placement;
use crate::types::{ActivityEvent, ActivityKind, Correction, OpenTask};

/// Digest budget in `approx_tokens`. The derive runner has 4096 − 600 gen −
/// 64 = 3432 tokens for the whole prompt; the v4 instruction text plus chat
/// template takes ~1100 (Qwen3 tokenizer), leaving ~2300 for the digest.
/// 2200 keeps a margin for template drift, so the render ladder shortens
/// titles and app lists instead of the runner cutting the tail, where the
/// open tasks, hints and corrections live.
pub const MAX_TOKENS: usize = 2200;

/// Rough heuristic; the real tokenizer lives in the derive worker. Digest
/// text runs 2.5–3.0 chars per token (timestamps, dashes, paths, JSON
/// workspace context), measured against the Qwen3 tokenizer on fixture
/// goldens and a live batch; 8/3 lands a few percent over on each.
pub fn approx_tokens(s: &str) -> usize {
    s.chars().count() * 3 / 8
}

/// Character budget for `tokens` under the same heuristic.
pub const fn max_chars(tokens: usize) -> usize {
    tokens * 8 / 3
}

/// `hints` are the pre-pass's provisional placements over the window; each
/// must name a task in `open_tasks` (the worker appends missing ones) or it
/// is left out of the digest.
#[allow(clippy::too_many_arguments)]
pub fn build_digest(
    spans: &[SpanDraft],
    tz: &TimeZone,
    open_tasks: &[OpenTask],
    corrections: &[Correction],
    hints: &[Placement],
    vcs: &[ActivityEvent],
    mcp_context: Option<&str>,
    ticket_re: Option<&regex::Regex>,
) -> String {
    for (apps_cap, title_chars) in [(8, 120), (6, 80), (4, 48), (3, 24)] {
        let out = render(
            spans,
            tz,
            open_tasks,
            corrections,
            hints,
            vcs,
            mcp_context,
            ticket_re,
            apps_cap,
            title_chars,
        );
        if approx_tokens(&out) <= MAX_TOKENS {
            return out;
        }
    }
    let mut out = render(
        spans,
        tz,
        open_tasks,
        corrections,
        hints,
        vcs,
        mcp_context,
        ticket_re,
        3,
        24,
    );
    let mut cut = max_chars(MAX_TOKENS).min(out.len());
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
    hints: &[Placement],
    vcs: &[ActivityEvent],
    mcp_context: Option<&str>,
    ticket_re: Option<&regex::Regex>,
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

    // Branch names, commit subjects, AI sessions, PR events and calls are
    // the strongest task-identity signal in the window. Last 10 inside it;
    // omitted when empty so evidence-less digests (and their goldens) are
    // unchanged.
    let win_lo = first.start.as_millisecond();
    let win_hi = last.end.as_millisecond();
    let in_window: Vec<&ActivityEvent> = vcs
        .iter()
        .filter(|v| {
            let ms = v.ts.as_millisecond();
            ms >= win_lo && ms < win_hi
        })
        .collect();
    if !in_window.is_empty() {
        let _ = writeln!(out, "\n## Activity");
        let skip = in_window.len().saturating_sub(10);
        for v in &in_window[skip..] {
            let _ = writeln!(out, "{}", activity_line(v, tz, title_chars));
        }
    }

    // Ticket keys on screen and the working directories titles name (m27
    // chunk 5): deterministic identity evidence the model would otherwise
    // have to spot in the timeline. Omitted when empty so evidence-less
    // digests (and their goldens) are unchanged.
    let keys = ticket_re
        .map(|re| crate::evidence::keys_in_spans(spans, re, win_lo, win_hi))
        .unwrap_or_default();
    let cwds = crate::evidence::cwd_repos(spans, win_lo, win_hi);
    if !keys.is_empty() || !cwds.is_empty() {
        let _ = writeln!(out, "\n## Keys seen");
        if !keys.is_empty() {
            let line: Vec<String> = keys
                .iter()
                .take(6)
                .map(|k| format!("{} {} ({})", k.key, fmt_dur(k.ms), k.app))
                .collect();
            let _ = writeln!(out, "{}", line.join(", "));
        }
        if !cwds.is_empty() {
            let line: Vec<String> = cwds
                .iter()
                .take(4)
                .map(|(repo, ms)| format!("{repo} {}", fmt_dur(*ms)))
                .collect();
            let _ = writeln!(out, "cwd {}", line.join(", "));
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

    // The pre-pass's provisional placements, as minute ranges → open-task
    // index with the rule that matched, so the model confirms or overrides a
    // deterministic guess instead of starting cold. A hint whose task is not
    // in the list (the worker appends them, so only a race) is skipped.
    // Omitted when empty so hint-free digests (and their goldens) are unchanged.
    let hint_lines: Vec<String> = hints
        .iter()
        .filter_map(|h| {
            let idx = open_tasks.iter().position(|t| t.id == h.task_id)? + 1;
            let lo = ((h.start_ts - win_start).max(0) / 60_000) as usize;
            let hi = (((h.end_ts - win_start) + 59_999) / 60_000).clamp(0, mins as i64) as usize;
            (hi > lo).then(|| {
                format!(
                    "- {lo}\u{2013}{hi}m \u{2192} {idx} ({}: {})",
                    hint_strength(&h.reason),
                    h.reason
                )
            })
        })
        .collect();
    if !hint_lines.is_empty() {
        let _ = writeln!(
            out,
            "\n## Pre-pass hints (rule-based guesses; confirm or override)"
        );
        for l in &hint_lines {
            let _ = writeln!(out, "{l}");
        }
    }

    // Omitted entirely when empty so correction-free digests (and their
    // goldens) are unchanged. Ejects render apart from the renames: the
    // quoted side is the work the user pulled out, the task is what it is
    // not — a negative example, where the renames are positive ones.
    let (ejects, renames): (Vec<&Correction>, Vec<&Correction>) =
        corrections.iter().partition(|c| c.kind == "eject");
    if !renames.is_empty() {
        let _ = writeln!(out, "\n## Past corrections (user renamed similar work)");
        for c in renames {
            // An assign has no old label: the work it was made over is the
            // left-hand side, like an eject, never "(unassigned)".
            let old = if c.kind == "assign" || c.old_label == "(unassigned)" {
                // Pre-m23 assigns carry no context: nothing to quote, skip.
                let Some(work) = c.ctx.lines().find(|l| !l.trim().is_empty()) else {
                    continue;
                };
                clip(crate::evidence::strip_glyphs(work.trim()), title_chars)
            } else {
                crate::evidence::strip_glyphs(&c.old_label).to_owned()
            };
            let _ = write!(
                out,
                "- \"{old}\" \u{2192} \"{}\"",
                crate::evidence::strip_glyphs(&c.new_label)
            );
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
    if !ejects.is_empty() {
        let _ = writeln!(out, "\n## Ejected (user pulled similar work out of a task)");
        for c in ejects {
            let work = c.ctx.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let _ = write!(
                out,
                "- \"{}\" \u{2717} \"{}\"",
                clip(crate::evidence::strip_glyphs(work.trim()), title_chars),
                crate::evidence::strip_glyphs(&c.old_label)
            );
            if let Some(p) = &c.old_project {
                let _ = write!(out, " [{p}]");
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

/// One digest/journal line for an activity event: `- HH:MM <kind> …`.
/// How firmly a pre-pass rule's placement should be read: a branch or a
/// window title naming the task's ticket is strong; a shared repo or a
/// similar past correction is weak.
pub fn hint_strength(reason: &str) -> &'static str {
    if reason.starts_with("branch ") || reason.starts_with("title ") {
        "strong"
    } else {
        "weak"
    }
}

pub fn activity_line(v: &ActivityEvent, tz: &TimeZone, title_chars: usize) -> String {
    let hm = v.ts.to_zoned(tz.clone()).strftime("%H:%M");
    let dur = v
        .end_ts
        .map(|e| fmt_dur(e.as_millisecond() - v.ts.as_millisecond()))
        .unwrap_or_default();
    let summary = v.summary.as_deref().map(|s| clip(s, title_chars));
    match v.kind {
        ActivityKind::Checkout => {
            format!("- {hm} checkout {} \u{2192} {}", v.repo, v.branch)
        }
        ActivityKind::Commit => {
            let mut line = format!("- {hm} commit {}", v.repo);
            if let Some(s) = summary {
                let _ = write!(line, " \"{s}\"");
            }
            let _ = write!(line, " [{}]", v.branch);
            line
        }
        ActivityKind::AiSession => {
            let mut line = format!("- {hm} claude {}", v.repo);
            if !v.branch.is_empty() {
                let _ = write!(line, "@{}", v.branch);
            }
            if !dur.is_empty() {
                let _ = write!(line, " {dur}");
            }
            if let Some(s) = summary {
                let _ = write!(line, " \"{s}\"");
            }
            line
        }
        ActivityKind::PrAuthored | ActivityKind::PrReviewed => {
            let what = if v.kind == ActivityKind::PrAuthored {
                "PR authored"
            } else {
                "PR reviewed"
            };
            let mut line = format!("- {hm} {what} {}", v.repo);
            if let Some(s) = summary {
                let _ = write!(line, " {s}");
            }
            line
        }
        ActivityKind::Call => {
            let mut line = format!("- {hm} call");
            if !dur.is_empty() {
                let _ = write!(line, " {dur}");
            } else {
                line.push_str(" (ongoing)");
            }
            if let Some(s) = summary {
                let _ = write!(line, " ({s})");
            }
            line
        }
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
