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
    plan: Option<&str>,
) -> String {
    let Some(agg) = aggregate(spans, tz, vcs, ticket_re) else {
        return String::new();
    };
    for (apps_cap, title_chars) in [(8, 120), (6, 80), (4, 48), (3, 24)] {
        let out = render(
            &agg,
            tz,
            open_tasks,
            corrections,
            hints,
            mcp_context,
            plan,
            apps_cap,
            title_chars,
        );
        if approx_tokens(&out) <= MAX_TOKENS {
            return out;
        }
    }
    let mut out = render(
        &agg,
        tz,
        open_tasks,
        corrections,
        hints,
        mcp_context,
        plan,
        3,
        24,
    );
    truncate_chars(&mut out, max_chars(MAX_TOKENS));
    out
}

/// Cut `s` to at most `max` bytes, backing off to the nearest preceding
/// char boundary — the hard size cap once the render ladder's cheapest rung
/// still overflows the token budget. No ellipsis: the caller already chose
/// this size on purpose.
pub(crate) fn truncate_chars(s: &mut String, max: usize) {
    let mut cut = max.min(s.len());
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
}

/// Dual-entry minutes: a focus runner-up holding ≥ ~25% of a minute renders
/// alongside the dominant activity, so interleaved work stays visible
/// instead of being hidden by dominant-minute RLE.
const RUNNER_UP_MS: i64 = 15_000;
type MinuteKey<'a> = (u8, &'a str, &'a str);

/// The aggregates every render-ladder rung needs, computed once from `spans`
/// regardless of how the rung will cap/clip them — `render` only re-does the
/// truncation and title clipping per rung.
struct Agg<'a> {
    start: jiff::Zoned,
    end: jiff::Zoned,
    active_ms: i64,
    afk_ms: i64,
    switches: usize,
    /// Sorted, full (not capped to a rung's `apps_cap`).
    apps: Vec<(&'a str, i64)>,
    /// Sorted, full (not capped).
    sites: Vec<(String, i64)>,
    in_window: Vec<&'a ActivityEvent>,
    keys: Vec<crate::evidence::KeySeen>,
    cwds: Vec<(String, i64)>,
    docs: Vec<(String, i64)>,
    people: Vec<(String, i64)>,
    win_start: i64,
    mins: usize,
    minute: Vec<Option<(MinuteKey<'a>, Option<MinuteKey<'a>>)>>,
}

fn aggregate<'a>(
    spans: &'a [SpanDraft],
    tz: &TimeZone,
    vcs: &'a [ActivityEvent],
    ticket_re: Option<&regex::Regex>,
) -> Option<Agg<'a>> {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
        return None;
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

    let start = first.start.to_zoned(tz.clone());
    let end = last.end.to_zoned(tz.clone());

    // Domain + first path segment groups browser time per site regardless of
    // page-title churn.
    let mut site_ms: HashMap<String, i64> = HashMap::new();
    for span in spans {
        if span.kind == SpanKind::Focus
            && let Some(url) = &span.url
        {
            *site_ms.entry(site_key(url)).or_default() += span.duration_ms();
        }
    }
    let mut sites: Vec<(String, i64)> = site_ms.into_iter().collect();
    sites.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    // Branch names, commit subjects, AI sessions, PR events and calls are
    // the strongest task-identity signal in the window.
    let win_lo = first.start.as_millisecond();
    let win_hi = last.end.as_millisecond();
    let in_window: Vec<&ActivityEvent> = vcs
        .iter()
        // Cwd probes and browse visits are anchor evidence, not activity
        // worth a line.
        .filter(|v| !matches!(v.kind, ActivityKind::Cwd | ActivityKind::Browse))
        .filter(|v| {
            let ms = v.ts.as_millisecond();
            ms >= win_lo && ms < win_hi
        })
        .collect();

    // Ticket keys on screen and the working directories titles name (m27
    // chunk 5): deterministic identity evidence the model would otherwise
    // have to spot in the timeline.
    let keys = ticket_re
        .map(|re| crate::evidence::keys_in_spans(spans, re, win_lo, win_hi))
        .unwrap_or_default();
    let cwds = crate::evidence::cwd_repos(spans, win_lo, win_hi);
    // Documents and people the anchors see (m30 chunk 1): the evidence the
    // timeline titles carry only implicitly, aggregated by focus time.
    let (docs, people) = anchor_evidence(spans, vcs, ticket_re, win_lo, win_hi);

    // Chronological view: without it the model has only aggregates and must
    // guess task offsets. Dominant activity per minute, run-length encoded —
    // smooths sub-minute interleaving into readable stretches while every
    // minute stays covered; sliver time still counts in the stats above.
    let win_start = win_lo;
    let mins = ((win_hi - win_start + 59_999) / 60_000).max(0) as usize;
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

    Some(Agg {
        start,
        end,
        active_ms,
        afk_ms,
        switches,
        apps,
        sites,
        in_window,
        keys,
        cwds,
        docs,
        people,
        win_start,
        mins,
        minute,
    })
}

#[allow(clippy::too_many_arguments)]
fn render(
    agg: &Agg,
    tz: &TimeZone,
    open_tasks: &[OpenTask],
    corrections: &[Correction],
    hints: &[Placement],
    mcp_context: Option<&str>,
    plan: Option<&str>,
    apps_cap: usize,
    title_chars: usize,
) -> String {
    let mut apps = agg.apps.clone();
    apps.truncate(apps_cap);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Activity {} {}\u{2013}{} ({})",
        agg.start.strftime("%Y-%m-%d"),
        agg.start.strftime("%H:%M"),
        agg.end.strftime("%H:%M"),
        tz.iana_name().unwrap_or("local"),
    );
    let _ = writeln!(
        out,
        "active {} \u{b7} afk {} \u{b7} {} switches",
        fmt_dur(agg.active_ms),
        fmt_dur(agg.afk_ms),
        agg.switches,
    );

    let _ = writeln!(out, "\n## Apps by time");
    for (app, ms) in &apps {
        let _ = writeln!(out, "- {app}: {}", fmt_dur(*ms));
    }

    // Omitted entirely when no spans carry URLs, so pre-M6 fixtures and
    // their goldens are unchanged.
    if !agg.sites.is_empty() {
        let mut sites = agg.sites.clone();
        sites.truncate(apps_cap);
        let _ = writeln!(out, "\n## Sites by time");
        for (site, ms) in &sites {
            let _ = writeln!(out, "- {site}: {}", fmt_dur(*ms));
        }
    }

    // Last 10 inside the window; omitted when empty so evidence-less
    // digests (and their goldens) are unchanged.
    if !agg.in_window.is_empty() {
        let _ = writeln!(out, "\n## Activity");
        let skip = agg.in_window.len().saturating_sub(10);
        for v in &agg.in_window[skip..] {
            let _ = writeln!(out, "{}", activity_line(v, tz, title_chars));
        }
    }

    // Omitted when empty so evidence-less digests (and their goldens) are
    // unchanged.
    if !agg.keys.is_empty() || !agg.cwds.is_empty() {
        let _ = writeln!(out, "\n## Keys seen");
        if !agg.keys.is_empty() {
            let line: Vec<String> = agg
                .keys
                .iter()
                .take(6)
                .map(|k| format!("{} {} ({})", k.key, fmt_dur(k.ms), k.app))
                .collect();
            let _ = writeln!(out, "{}", line.join(", "));
        }
        if !agg.cwds.is_empty() {
            let line: Vec<String> = agg
                .cwds
                .iter()
                .take(4)
                .map(|(repo, ms)| format!("{repo} {}", fmt_dur(*ms)))
                .collect();
            let _ = writeln!(out, "cwd {}", line.join(", "));
        }
    }

    if !agg.docs.is_empty() {
        let line: Vec<String> = agg
            .docs
            .iter()
            .take(5)
            .map(|(d, ms)| format!("{} {}", clip(d, title_chars.min(60)), fmt_dur(*ms)))
            .collect();
        let _ = writeln!(out, "\n## Documents\n{}", line.join(", "));
    }
    if !agg.people.is_empty() {
        let line: Vec<String> = agg
            .people
            .iter()
            .take(4)
            .map(|(p, ms)| format!("{p} {}", fmt_dur(*ms)))
            .collect();
        let _ = writeln!(out, "\n## People\n{}", line.join(", "));
    }

    let _ = writeln!(out, "\n## Timeline (minute offsets from window start)");
    let mut m = 0usize;
    while m < agg.mins {
        let Some((dom, runner)) = agg.minute[m] else {
            m += 1;
            continue;
        };
        let mut end = m + 1;
        while end < agg.mins && agg.minute[end] == Some((dom, runner)) {
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

    // What the user said in the morning the day was for (m26). Omitted when
    // no intent was set, so plan-free digests (and their goldens) are
    // unchanged.
    if let Some(plan) = plan.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "\n## Plan");
        // Clipped: a long intent must not push `## Open tasks` past the
        // digest's own truncation.
        let _ = writeln!(out, "{}", clip(plan, 400));
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
    let task_idx: HashMap<i64, usize> = {
        let mut m = HashMap::new();
        for (i, t) in open_tasks.iter().enumerate() {
            m.entry(t.id).or_insert(i + 1);
        }
        m
    };
    let hint_lines: Vec<String> = hints
        .iter()
        .filter_map(|h| {
            let idx = *task_idx.get(&h.task_id)?;
            let lo = ((h.start_ts - agg.win_start).max(0) / 60_000) as usize;
            let hi =
                (((h.end_ts - agg.win_start) + 59_999) / 60_000).clamp(0, agg.mins as i64) as usize;
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
        ActivityKind::Meeting
        | ActivityKind::Edit
        | ActivityKind::Shell
        | ActivityKind::Cwd
        | ActivityKind::Browse
        | ActivityKind::Note => {
            let mut line = format!("- {hm} {}", v.kind.as_str());
            if !v.repo.is_empty() {
                let _ = write!(line, " {}", v.repo);
            }
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
    }
}

/// Ground truth for a narrative (m32 chunk 5): the rows that cannot be
/// wrong about what happened — commit subjects, AI sessions with their
/// prompts and last reply, notes, PR events and meetings — one line each,
/// ending in the source tag a claim built on it must carry (`[commit
/// 35c60ec]`, `[session 13:06]`, `[note 13:06]`, `[pr 42]`, `[meeting
/// 09:00]`). Checkouts, shells, cwd probes, edits and calls say where and
/// how long, never what, and are left out. Rows come out in the order
/// given.
pub fn ground_truth(events: &[ActivityEvent], tz: &TimeZone) -> String {
    events
        .iter()
        .filter_map(|v| truth_block(v, tz))
        .map(|b| b.text)
        .collect()
}

/// [`ground_truth`] under a character budget: commits first, then notes,
/// PRs, meetings and sessions, each in the order given, until the budget
/// is spent; what is kept comes out by time. A local model's window is
/// small and the tail of a long prompt is what gets cut, so the digest
/// chooses what goes rather than the tokenizer.
pub fn ground_truth_within(events: &[ActivityEvent], tz: &TimeZone, max_chars: usize) -> String {
    let mut blocks: Vec<(usize, TruthBlock)> = events
        .iter()
        .filter_map(|v| truth_block(v, tz))
        .enumerate()
        .collect();
    blocks.sort_by_key(|(i, b)| (b.priority, *i));
    let mut kept: Vec<(usize, TruthBlock)> = Vec::new();
    let mut used = 0;
    for (i, b) in blocks {
        let n = b.text.chars().count();
        if used + n > max_chars {
            continue;
        }
        used += n;
        kept.push((i, b));
    }
    kept.sort_by_key(|(i, b)| (b.ts, *i));
    kept.into_iter().map(|(_, b)| b.text).collect()
}

struct TruthBlock {
    /// Lower first under a budget: what a commit says beats what a
    /// session was asked.
    priority: u8,
    ts: i64,
    text: String,
}

fn truth_block(v: &ActivityEvent, tz: &TimeZone) -> Option<TruthBlock> {
    let hm = v.ts.to_zoned(tz.clone()).strftime("%H:%M");
    let dur_ms = v.end_ts.map(|e| e.as_millisecond() - v.ts.as_millisecond());
    let dur = dur_ms.map(fmt_dur).unwrap_or_default();
    let at = |s: &mut String| {
        let _ = write!(s, " {}", v.repo);
        if !v.branch.is_empty() {
            let _ = write!(s, "@{}", v.branch);
        }
    };
    let mut out = String::new();
    let priority = match v.kind {
        ActivityKind::Commit => {
            let subject = v.summary.as_deref()?;
            let short: String = v.ext_id.as_deref().unwrap_or("").chars().take(7).collect();
            let mut line = format!("- {hm} commit");
            at(&mut line);
            let _ = writeln!(out, "{line} \"{}\" [commit {short}]", clip(subject, 160));
            0
        }
        ActivityKind::Note => {
            let body = v.summary.as_deref()?;
            let mut line = format!("- {hm} note");
            at(&mut line);
            let _ = writeln!(out, "{line} \"{}\" [note {hm}]", clip(body, 300));
            1
        }
        ActivityKind::PrAuthored | ActivityKind::PrReviewed => {
            let what = if v.kind == ActivityKind::PrAuthored {
                "PR authored"
            } else {
                "PR reviewed"
            };
            let number = v
                .ext_id
                .as_deref()
                .and_then(|id| id.rsplit('/').next())
                .unwrap_or("");
            let mut line = format!("- {hm} {what} {}", v.repo);
            if let Some(s) = v.summary.as_deref() {
                let _ = write!(line, " {}", clip(s, 160));
            }
            let _ = writeln!(out, "{line} [pr {number}]");
            2
        }
        ActivityKind::Meeting => {
            let title = v.summary.as_deref()?;
            let mut line = format!("- {hm} meeting");
            if !dur.is_empty() {
                let _ = write!(line, " {dur}");
            }
            let _ = writeln!(out, "{line} \"{}\" [meeting {hm}]", clip(title, 160));
            3
        }
        ActivityKind::AiSession => {
            let detail: Option<serde_json::Value> = v
                .detail
                .as_deref()
                .and_then(|d| serde_json::from_str(d).ok());
            let prompts: Vec<&str> = detail
                .as_ref()
                .and_then(|d| d.get("prompts")?.as_array())
                .map(|a| a.iter().filter_map(|p| p.as_str()).collect())
                .unwrap_or_default();
            let prompts: Vec<&str> = if prompts.is_empty() {
                v.summary.as_deref().into_iter().collect()
            } else {
                prompts
            };
            // Interrupt markers and pasted-image placeholders are not
            // what was asked.
            let prompts: Vec<&str> = prompts
                .into_iter()
                .filter(|p| !p.trim_start().starts_with('['))
                .collect();
            let reply = detail
                .as_ref()
                .and_then(|d| d.get("last_assistant")?.as_str());
            // A session that was asked nothing and lasted under a minute
            // (a resumed transcript's stub) says nothing.
            if prompts.is_empty() && reply.is_none() && dur_ms.unwrap_or(0) < 60_000 {
                return None;
            }
            let mut line = format!("- {hm} session");
            at(&mut line);
            if !dur.is_empty() {
                let _ = write!(line, " {dur}");
            }
            let _ = writeln!(out, "{line} [session {hm}]");
            for p in prompts.iter().take(TRUTH_PROMPTS) {
                let _ = writeln!(out, "    prompt: \"{}\"", clip(p, 160));
            }
            if let Some(reply) = reply {
                let _ = writeln!(out, "    reply: \"{}\"", clip(reply, 240));
            }
            4
        }
        ActivityKind::Checkout
        | ActivityKind::Call
        | ActivityKind::Edit
        | ActivityKind::Shell
        | ActivityKind::Cwd
        | ActivityKind::Browse => return None,
    };
    Some(TruthBlock {
        priority,
        ts: v.ts.as_millisecond(),
        text: out,
    })
}

/// Prompts quoted per session in [`ground_truth`].
const TRUTH_PROMPTS: usize = 4;

/// Focus time per document and per person named by the spans' anchors in
/// `[lo, hi)`, most first; ties by name so the order is stable.
fn anchor_evidence(
    spans: &[SpanDraft],
    vcs: &[ActivityEvent],
    ticket_re: Option<&regex::Regex>,
    lo: i64,
    hi: i64,
) -> (Vec<(String, i64)>, Vec<(String, i64)>) {
    #![allow(clippy::type_complexity)]
    use crate::extract::{self, AnchorKind};
    static NEVER: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new("$^").unwrap());
    let re = ticket_re.unwrap_or(&NEVER);
    let mut docs: HashMap<String, i64> = HashMap::new();
    let mut people: HashMap<String, i64> = HashMap::new();
    for s in spans.iter().filter(|s| s.kind == SpanKind::Focus) {
        let (a, b) = (
            s.start.as_millisecond().max(lo),
            s.end.as_millisecond().min(hi),
        );
        if b <= a {
            continue;
        }
        let own = extract::extract(&s.app, &s.title, s.url.as_deref(), re);
        let more = extract::from_activity(&s.app, &s.title, a, b, &own, vcs, re);
        for anchor in extract::merge(own, more) {
            match anchor.kind {
                AnchorKind::Doc => *docs.entry(anchor.value).or_default() += b - a,
                AnchorKind::People => *people.entry(anchor.value).or_default() += b - a,
                _ => {}
            }
        }
    }
    let sorted = |m: HashMap<String, i64>| {
        let mut v: Vec<(String, i64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    (sorted(docs), sorted(people))
}

pub(crate) fn clip(s: &str, max_chars: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;

    fn ev(kind: ActivityKind, ts_ms: i64, ext_id: &str, summary: Option<&str>) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(ts_ms),
            end_ts: None,
            repo: "app".into(),
            branch: "main".into(),
            kind,
            ext_id: Some(ext_id.into()),
            summary: summary.map(str::to_owned),
            detail: None,
        }
    }

    #[test]
    fn ground_truth_quotes_sources_and_tags_them() {
        let tz = TimeZone::UTC;
        let h = 3_600_000;
        let mut session = ev(
            ActivityKind::AiSession,
            13 * h + 6 * 60_000,
            "s1",
            Some("first prompt"),
        );
        session.end_ts = Some(ms_to_ts(13 * h + 31 * 60_000));
        session.detail = Some(
            r#"{"prompts":["fix the test","now the docs"],"last_assistant":"Done: 3 tests pass."}"#
                .into(),
        );
        let events = [
            ev(
                ActivityKind::Commit,
                14 * h,
                "35c60ecabcdef",
                Some("feat(core): anchor hygiene"),
            ),
            session,
            ev(
                ActivityKind::Note,
                15 * h,
                "note:app:d",
                Some("plan drafted"),
            ),
            ev(
                ActivityKind::PrAuthored,
                16 * h,
                "https://github.com/o/app/pull/42",
                Some("PR #42 hygiene"),
            ),
            ev(ActivityKind::Checkout, 17 * h, "", None),
            ev(ActivityKind::Shell, 18 * h, "sh", Some("cargo")),
            ev(ActivityKind::Commit, 19 * h, "deadbeef", None),
        ];
        let got = ground_truth(&events, &tz);
        assert_eq!(
            got,
            "- 14:00 commit app@main \"feat(core): anchor hygiene\" [commit 35c60ec]\n\
             - 13:06 session app@main 25m00s [session 13:06]\n\
             \x20   prompt: \"fix the test\"\n\
             \x20   prompt: \"now the docs\"\n\
             \x20   reply: \"Done: 3 tests pass.\"\n\
             - 15:00 note app@main \"plan drafted\" [note 15:00]\n\
             - 16:00 PR authored app PR #42 hygiene [pr 42]\n"
        );
        // A session with no detail falls back to its first prompt.
        let bare = ev(ActivityKind::AiSession, 13 * h, "s2", Some("first prompt"));
        let got = ground_truth(std::slice::from_ref(&bare), &tz);
        assert!(got.contains("prompt: \"first prompt\""), "{got}");
        assert!(!got.contains("reply:"), "{got}");
        // An interrupt marker is not a prompt, and a stub session with
        // nothing asked is nothing.
        let stub = ev(
            ActivityKind::AiSession,
            13 * h,
            "s3",
            Some("[Request interrupted by user]"),
        );
        assert_eq!(ground_truth(&[stub], &tz), "");
    }

    #[test]
    fn a_budget_keeps_commits_over_sessions_and_restores_time_order() {
        let tz = TimeZone::UTC;
        let h = 3_600_000;
        let mut session = ev(ActivityKind::AiSession, 9 * h, "s1", Some("fix the test"));
        session.end_ts = Some(ms_to_ts(10 * h));
        let events = [
            session,
            ev(ActivityKind::Note, 11 * h, "n", Some("wrote the plan")),
            ev(ActivityKind::Commit, 12 * h, "abc1234", Some("feat: plan")),
        ];
        let full = ground_truth(&events, &tz);
        assert_eq!(ground_truth_within(&events, &tz, 10_000), full);
        let commit_line = "- 12:00 commit app@main \"feat: plan\" [commit abc1234]\n";
        let note_line = "- 11:00 note app@main \"wrote the plan\" [note 11:00]\n";
        let budget = commit_line.len() + note_line.len();
        assert_eq!(
            ground_truth_within(&events, &tz, budget),
            format!("{note_line}{commit_line}")
        );
        assert_eq!(
            ground_truth_within(&events, &tz, commit_line.len()),
            commit_line
        );
    }
}
