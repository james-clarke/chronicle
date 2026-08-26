//! Batch spans → deterministic digest text for the derivation prompt.
//! Corrections few-shot lands in M5, URL domains in M6, MCP context in M8.

use std::collections::HashMap;
use std::fmt::Write;

use jiff::tz::TimeZone;

use crate::sessionizer::{SpanDraft, SpanKind};

pub const MAX_TOKENS: usize = 3000;

/// Rough heuristic; the real tokenizer lives in the derive worker.
pub fn approx_tokens(s: &str) -> usize {
    s.chars().count() / 4
}

pub fn build_digest(spans: &[SpanDraft], tz: &TimeZone, recent_labels: &[String]) -> String {
    for (apps_cap, titles_cap) in [(8, 6), (6, 4), (4, 2), (3, 1)] {
        let out = render(spans, tz, recent_labels, apps_cap, titles_cap);
        if approx_tokens(&out) <= MAX_TOKENS {
            return out;
        }
    }
    let mut out = render(spans, tz, recent_labels, 3, 1);
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
    titles_cap: usize,
) -> String {
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
        return String::new();
    };

    let mut active_ms = 0i64;
    let mut afk_ms = 0i64;
    let mut switches = 0usize;
    let mut app_ms: HashMap<&str, i64> = HashMap::new();
    let mut title_ms: HashMap<(&str, &str), i64> = HashMap::new();
    for span in spans {
        let dur = span.duration_ms();
        match span.kind {
            SpanKind::Focus => {
                active_ms += dur;
                switches += 1;
                *app_ms.entry(&span.app).or_default() += dur;
                *title_ms.entry((&span.app, &span.title)).or_default() += dur;
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

    let _ = writeln!(out, "\n## Top titles");
    for (app, ms) in &apps {
        let _ = writeln!(out, "### {app} ({})", fmt_dur(*ms));
        let mut titles: Vec<(&str, i64)> = title_ms
            .iter()
            .filter(|((a, _), _)| a == app)
            .map(|((_, t), ms)| (*t, *ms))
            .collect();
        titles.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        titles.truncate(titles_cap);
        for (title, ms) in titles {
            let _ = writeln!(out, "- {title} \u{2014} {}", fmt_dur(ms));
        }
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
