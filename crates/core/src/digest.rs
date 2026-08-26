//! Batch spans → deterministic digest text for the derivation prompt.
//! Corrections few-shot lands in M5, URL domains in M6, MCP context in M8.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

use jiff::tz::TimeZone;

use crate::sessionizer::{SpanDraft, SpanKind};

pub const MAX_TOKENS: usize = 3000;

/// Rough heuristic; the real tokenizer lives in the derive worker.
pub fn approx_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

pub fn build_digest(spans: &[SpanDraft], tz: &TimeZone, recent_labels: &[String]) -> String {
    for (apps_cap, title_chars) in [(8, 120), (6, 80), (4, 48), (3, 24)] {
        let out = render(spans, tz, recent_labels, apps_cap, title_chars);
        if approx_tokens(&out) <= MAX_TOKENS {
            return out;
        }
    }
    let mut out = render(spans, tz, recent_labels, 3, 24);
    let mut cut = (MAX_TOKENS * 4).min(out.len());
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    out
}

fn render(
    spans: &[SpanDraft],
    tz: &TimeZone,
    recent_labels: &[String],
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
    let dominant: Vec<Option<(u8, &str, &str)>> = buckets
        .iter()
        .map(|b| b.iter().max_by_key(|(_, ms)| *ms).map(|(k, _)| *k))
        .collect();
    let _ = writeln!(out, "\n## Timeline (minute offsets from window start)");
    let mut m = 0usize;
    while m < mins {
        let Some(key) = dominant[m] else {
            m += 1;
            continue;
        };
        let mut end = m + 1;
        while end < mins && dominant[end] == Some(key) {
            end += 1;
        }
        match key.0 {
            0 => {
                let _ = writeln!(
                    out,
                    "- {m}\u{2013}{end}m {}: {}",
                    key.1,
                    clip(key.2, title_chars)
                );
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

    let _ = writeln!(out, "\n## Recent task labels");
    if recent_labels.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for label in recent_labels.iter().take(3) {
        let _ = writeln!(out, "- {label}");
    }
    out
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

fn fmt_dur(ms: i64) -> String {
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
