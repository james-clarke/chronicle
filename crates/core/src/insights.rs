//! Focus-quality metrics, app aggregates, and period deltas over the same
//! rows the reports use. Pure folds like `report` — no storage access.

use jiff::tz::TimeZone;

use crate::report::RangeReport;
use crate::sessionizer::{SpanDraft, SpanKind};
use crate::types::Task;

/// Display gap under which adjacent intervals of one task merge into a
/// session (the same threshold the timeline UI uses).
pub const SESSION_GAP_MS: i64 = 5 * 60 * 1000;

/// Sessions counting toward deep work must be at least this long.
pub const DEEP_WORK_MIN_MS: i64 = 25 * 60 * 1000;

/// One merged run of a task's intervals, clamped to the queried range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub task_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Merge `tasks` rows (interval-joined, as returned by
/// `storage::tasks_in_range`) into per-task display sessions clamped to
/// `[lo, hi)`, ordered by start across all tasks.
pub fn sessions_from_tasks(tasks: &[Task], lo: i64, hi: i64) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    for t in tasks {
        let s = t.start_ts.as_millisecond().max(lo);
        let e = t.end_ts.as_millisecond().min(hi);
        if e <= s {
            continue;
        }
        // Rows arrive ordered by interval start, so the latest session of
        // this task is the merge candidate.
        match sessions
            .iter_mut()
            .rev()
            .find(|sess| sess.task_id == t.id)
        {
            Some(sess) if s - sess.end_ms <= SESSION_GAP_MS && s >= sess.start_ms => {
                sess.end_ms = sess.end_ms.max(e);
            }
            _ => sessions.push(Session {
                task_id: t.id,
                start_ms: s,
                end_ms: e,
            }),
        }
    }
    sessions.sort_by_key(|s| s.start_ms);
    sessions
}

/// Focus-quality summary of a range. A "switch" is a task-identity
/// transition between adjacent sessions — Chronicle already groups
/// multi-app work under one task, so app changes within a task don't count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusMetrics {
    /// Longest single session.
    pub longest_block_ms: i64,
    /// Task-identity transitions between adjacent sessions.
    pub switch_count: usize,
    /// Total time in sessions of at least [`DEEP_WORK_MIN_MS`].
    pub deep_work_ms: i64,
    /// Local hour (0-23) with the most session starts; None under two
    /// sessions (no fragmentation to speak of).
    pub most_fragmented_hour: Option<i8>,
}

pub fn focus_metrics(sessions: &[Session], tz: &TimeZone) -> FocusMetrics {
    let mut longest = 0i64;
    let mut deep = 0i64;
    let mut switches = 0usize;
    let mut starts_by_hour = [0u32; 24];
    for (i, s) in sessions.iter().enumerate() {
        let dur = s.end_ms - s.start_ms;
        longest = longest.max(dur);
        if dur >= DEEP_WORK_MIN_MS {
            deep += dur;
        }
        if i > 0 && sessions[i - 1].task_id != s.task_id {
            switches += 1;
        }
        let hour = crate::types::ms_to_ts(s.start_ms)
            .to_zoned(tz.clone())
            .hour();
        starts_by_hour[hour as usize] += 1;
    }
    let most_fragmented_hour = if sessions.len() < 2 {
        None
    } else {
        starts_by_hour
            .iter()
            .enumerate()
            .max_by_key(|&(h, n)| (n, std::cmp::Reverse(h)))
            .filter(|&(_, &n)| n > 0)
            .map(|(h, _)| h as i8)
    };
    FocusMetrics {
        longest_block_ms: longest,
        switch_count: switches,
        deep_work_ms: deep,
        most_fragmented_hour,
    }
}

/// Focus time per app over `[lo, hi)`, biggest first, at most `cap` rows.
/// Browser apps stay one row (per-site split already happens upstream when
/// configured).
pub fn top_apps(spans: &[SpanDraft], lo: i64, hi: i64, cap: usize) -> Vec<(String, i64)> {
    let mut totals: Vec<(String, i64)> = Vec::new();
    for s in spans {
        if s.kind != SpanKind::Focus {
            continue;
        }
        let ms = (s.end.as_millisecond().min(hi) - s.start.as_millisecond().max(lo)).max(0);
        if ms == 0 {
            continue;
        }
        match totals.iter_mut().find(|(app, _)| *app == s.app) {
            Some((_, total)) => *total += ms,
            None => totals.push((s.app.clone(), ms)),
        }
    }
    totals.sort_by_key(|&(_, ms)| std::cmp::Reverse(ms));
    totals.truncate(cap);
    totals
}

/// Focus time in spans whose app or browser site key matches any of the
/// configured distraction regexes. Empty patterns = 0 (feature off).
pub fn distraction_ms(spans: &[SpanDraft], patterns: &[regex::Regex], lo: i64, hi: i64) -> i64 {
    if patterns.is_empty() {
        return 0;
    }
    let mut total = 0i64;
    for s in spans {
        if s.kind != SpanKind::Focus {
            continue;
        }
        let ms = (s.end.as_millisecond().min(hi) - s.start.as_millisecond().max(lo)).max(0);
        if ms == 0 {
            continue;
        }
        let site = s.url.as_deref().map(crate::digest::site_key);
        let hit = patterns.iter().any(|p| {
            p.is_match(&s.app) || site.as_deref().is_some_and(|k| p.is_match(k))
        });
        if hit {
            total += ms;
        }
    }
    total
}

/// The contiguous date range of the same length immediately before `days`
/// (for period-over-period deltas). None if `days` is empty or the shift
/// leaves the calendar.
pub fn prior_period(days: &[jiff::civil::Date]) -> Option<Vec<jiff::civil::Date>> {
    let len = i64::try_from(days.len()).ok()?;
    if len == 0 {
        return None;
    }
    days.iter()
        .map(|d| d.checked_sub(jiff::Span::new().days(len)).ok())
        .collect()
}

/// Per-project and total change between two reports (current minus prior).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub grand_total_delta_ms: i64,
    /// (project, delta_ms), sorted by absolute change, biggest first.
    /// Projects present in only one period still appear.
    pub per_project: Vec<(String, i64)>,
}

pub fn delta(current: &RangeReport, prior: &RangeReport) -> Delta {
    let mut per_project: Vec<(String, i64)> = Vec::new();
    for p in &current.projects {
        per_project.push((p.project.clone(), p.total_ms));
    }
    for p in &prior.projects {
        match per_project.iter_mut().find(|(name, _)| *name == p.project) {
            Some((_, d)) => *d -= p.total_ms,
            None => per_project.push((p.project.clone(), -p.total_ms)),
        }
    }
    per_project.retain(|&(_, d)| d != 0);
    per_project.sort_by_key(|&(_, d)| std::cmp::Reverse(d.abs()));
    Delta {
        grand_total_delta_ms: current.grand_total_ms - prior.grand_total_ms,
        per_project,
    }
}

/// Cheap deterministic fold over a report's numbers; changes whenever the
/// underlying data does. Shared by the UI (staleness check) and the
/// narrative worker (stamped into the cache) — the two must agree.
pub fn report_data_hash(r: &RangeReport) -> i64 {
    let mut h = r.grand_total_ms.wrapping_mul(31);
    for t in &r.tasks {
        h = h.wrapping_mul(31).wrapping_add(t.total_ms ^ t.task_id);
    }
    h
}

/// Compact stats digest fed to the narrative prompt: numbers only, rendered
/// from data the worker recomputed itself.
pub fn narrative_digest(
    r: &RangeReport,
    m: &FocusMetrics,
    apps: &[(String, i64)],
    d: Option<&Delta>,
) -> String {
    use std::fmt::Write;
    let fmt = crate::digest::fmt_dur;
    let mut out = String::new();
    if let (Some(first), Some(last)) = (r.days.first(), r.days.last()) {
        let _ = writeln!(out, "Range: {first} to {last}");
    }
    let _ = writeln!(out, "Total focus: {}", fmt(r.grand_total_ms));
    let _ = writeln!(out, "\nTop tasks:");
    for t in r.tasks.iter().take(5) {
        let _ = writeln!(out, "- {} [{}]: {}", t.label, t.project, fmt(t.total_ms));
    }
    if !r.projects.is_empty() {
        let _ = writeln!(out, "\nProjects:");
        for p in &r.projects {
            let _ = writeln!(out, "- {}: {}", p.project, fmt(p.total_ms));
        }
    }
    if !apps.is_empty() {
        let _ = writeln!(out, "\nTop apps:");
        for (app, ms) in apps.iter().take(5) {
            let _ = writeln!(out, "- {app}: {}", fmt(*ms));
        }
    }
    let _ = writeln!(
        out,
        "\nFocus quality: longest block {}, deep work {}, {} task switches{}",
        fmt(m.longest_block_ms),
        fmt(m.deep_work_ms),
        m.switch_count,
        m.most_fragmented_hour
            .map(|h| format!(", most fragmented hour {h:02}:00"))
            .unwrap_or_default()
    );
    if let Some(d) = d {
        let sign = |v: i64| if v >= 0 { "+" } else { "-" };
        let _ = writeln!(
            out,
            "\nVersus prior period: total {}{}",
            sign(d.grand_total_delta_ms),
            fmt(d.grand_total_delta_ms.abs())
        );
        for (p, v) in d.per_project.iter().take(5) {
            let _ = writeln!(out, "- {p}: {}{}", sign(*v), fmt(v.abs()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::{Date, date};

    fn task(id: i64, start_min: i64, end_min: i64) -> Task {
        Task {
            id,
            interval_id: id * 100 + start_min,
            label: format!("task{id}"),
            project: None,
            start_ts: crate::types::ms_to_ts(start_min * 60_000),
            end_ts: crate::types::ms_to_ts(end_min * 60_000),
            confidence: 1.0,
            declared: false,
            description: None,
        }
    }

    #[test]
    fn sessions_merge_within_gap_only() {
        // 0-10, gap 4 (merges), 14-20; then gap 20 (new session) 40-50.
        let tasks = [task(1, 0, 10), task(1, 14, 20), task(1, 40, 50)];
        let s = sessions_from_tasks(&tasks, 0, i64::MAX);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].start_ms, s[0].end_ms), (0, 20 * 60_000));
        assert_eq!((s[1].start_ms, s[1].end_ms), (40 * 60_000, 50 * 60_000));
    }

    #[test]
    fn switch_count_is_task_transitions() {
        // A, B, back to A (gaps > SESSION_GAP so no merging): 2 switches.
        let tasks = [task(1, 0, 10), task(2, 20, 30), task(1, 40, 55)];
        let s = sessions_from_tasks(&tasks, 0, i64::MAX);
        let m = focus_metrics(&s, &jiff::tz::TimeZone::UTC);
        assert_eq!(m.switch_count, 2);
        assert_eq!(m.longest_block_ms, 15 * 60_000);
    }

    #[test]
    fn deep_work_excludes_short_sessions() {
        // 30min (deep) + 10min (not).
        let tasks = [task(1, 0, 30), task(2, 60, 70)];
        let s = sessions_from_tasks(&tasks, 0, i64::MAX);
        let m = focus_metrics(&s, &jiff::tz::TimeZone::UTC);
        assert_eq!(m.deep_work_ms, 30 * 60_000);
    }

    #[test]
    fn fragmented_hour_needs_two_sessions() {
        let one = sessions_from_tasks(&[task(1, 0, 10)], 0, i64::MAX);
        assert_eq!(
            focus_metrics(&one, &jiff::tz::TimeZone::UTC).most_fragmented_hour,
            None
        );
        // Three starts in hour 0 (UTC epoch).
        let many = sessions_from_tasks(
            &[task(1, 0, 5), task(2, 10, 15), task(1, 20, 25)],
            0,
            i64::MAX,
        );
        assert_eq!(
            focus_metrics(&many, &jiff::tz::TimeZone::UTC).most_fragmented_hour,
            Some(0)
        );
    }

    fn report(days: Vec<Date>, projects: Vec<(&str, i64)>, total: i64) -> RangeReport {
        RangeReport {
            days,
            tasks: Vec::new(),
            projects: projects
                .into_iter()
                .map(|(p, ms)| crate::report::ProjectTotal {
                    project: p.into(),
                    total_ms: ms,
                })
                .collect(),
            grand_total_ms: total,
        }
    }

    #[test]
    fn delta_covers_projects_in_either_period() {
        let days = vec![date(2026, 9, 1)];
        let cur = report(days.clone(), vec![("a", 100), ("b", 50)], 150);
        let pri = report(vec![date(2026, 8, 31)], vec![("a", 30), ("c", 40)], 70);
        let d = delta(&cur, &pri);
        assert_eq!(d.grand_total_delta_ms, 80);
        assert_eq!(
            d.per_project,
            vec![("a".into(), 70), ("b".into(), 50), ("c".into(), -40)]
        );
    }

    #[test]
    fn prior_period_shifts_back_by_len() {
        let days: Vec<Date> = (1..=7).map(|d| date(2026, 9, d)).collect();
        let prior = prior_period(&days).unwrap();
        assert_eq!(prior[0], date(2026, 8, 25));
        assert_eq!(prior[6], date(2026, 8, 31));
    }
}
