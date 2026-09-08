//! Day/week report aggregation over task intervals. Pure fold over
//! `storage::tasks_in_range` rows — day bucketing is local-civil-time (jiff),
//! which SQLite's UTC-only DATE() can't do. An interval is split across the
//! civil days it overlaps and clamped to the report range: declared-task
//! intervals can span many hours (real data has 10h+ crossing midnight), so
//! bucketing whole durations by start day would double-count them in
//! adjacent reports.

use std::fmt::Write;

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::digest::fmt_dur;
use crate::sessionizer::AFK_SPLIT_MINS;
use crate::storage::DaemonRun;
use crate::types::Task;

pub const UNTAGGED: &str = "(none)";

#[derive(Debug, Clone)]
pub struct ProjectTotal {
    pub project: String,
    pub total_ms: i64,
}

/// Interval duration clamped to `[lo, hi)` — the fetch query returns rows
/// that merely overlap the range, and long intervals cross its edges.
fn clamped_ms(t: &Task, lo: i64, hi: i64) -> i64 {
    t.weigh((t.end_ts.as_millisecond().min(hi) - t.start_ts.as_millisecond().max(lo)).max(0))
}

/// Per-project totals over `[lo, hi)`, biggest first. `None` projects group
/// under [`UNTAGGED`].
pub fn project_totals(tasks: &[Task], lo: i64, hi: i64) -> Vec<ProjectTotal> {
    let mut totals: Vec<ProjectTotal> = Vec::new();
    for t in tasks {
        let name = t.project.as_deref().unwrap_or(UNTAGGED);
        let ms = clamped_ms(t, lo, hi);
        if ms == 0 {
            continue;
        }
        match totals.iter_mut().find(|p| p.project == name) {
            Some(p) => p.total_ms += ms,
            None => totals.push(ProjectTotal {
                project: name.to_string(),
                total_ms: ms,
            }),
        }
    }
    totals.sort_by_key(|p| std::cmp::Reverse(p.total_ms));
    totals
}

#[derive(Debug, Clone)]
pub struct TaskTotal {
    pub task_id: i64,
    pub label: String,
    pub project: String,
    pub total_ms: i64,
}

/// Per-task totals over an exact `[lo, hi)`, biggest first. Separate from
/// [`build`], whose day columns force whole-civil-day bounds: a question
/// about this morning must not count the afternoon.
pub fn task_totals(tasks: &[Task], lo: i64, hi: i64) -> Vec<TaskTotal> {
    let mut rows: Vec<TaskTotal> = Vec::new();
    for t in tasks {
        let ms = clamped_ms(t, lo, hi);
        if ms == 0 {
            continue;
        }
        match rows.iter_mut().find(|r| r.task_id == t.id) {
            Some(r) => r.total_ms += ms,
            None => rows.push(TaskTotal {
                task_id: t.id,
                label: t.label.clone(),
                project: t.project.as_deref().unwrap_or(UNTAGGED).to_string(),
                total_ms: ms,
            }),
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.total_ms));
    rows
}

/// `task · project · total` markdown plus the range total — the figures a
/// chat answer quotes instead of adding rows up itself. The total covers
/// every row, including any past `max_rows`.
/// A `|` in a label or project would split the markdown row it sits in.
fn cell(s: &str) -> String {
    s.replace('|', "/")
}

pub fn totals_table(rows: &[TaskTotal], max_rows: usize) -> String {
    let mut out = String::from("| task | project | total |\n|---|---|---|\n");
    for r in rows.iter().take(max_rows) {
        let _ = writeln!(
            out,
            "| {} | {} | {} |",
            cell(&r.label),
            cell(&r.project),
            fmt_dur(r.total_ms)
        );
    }
    let _ = writeln!(
        out,
        "| all tasks | | {} |",
        fmt_dur(rows.iter().map(|r| r.total_ms).sum())
    );
    out
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub task_id: i64,
    pub label: String,
    pub project: String,
    /// Milliseconds per day, aligned index-for-index with
    /// [`RangeReport::days`].
    pub by_day: Vec<i64>,
    pub total_ms: i64,
    /// Milliseconds per kind of work (m30 chunk 5), biggest first; only
    /// the intervals that carry a kind count.
    pub by_kind: Vec<(String, i64)>,
}

/// "agent 1h 30m · review 25m": a task's kind mix, biggest first, up to
/// three kinds. Empty when nothing carries a kind.
pub fn kind_mix(by_kind: &[(String, i64)]) -> String {
    by_kind
        .iter()
        .take(3)
        .map(|(k, ms)| format!("{k} {}", fmt_dur(*ms)))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

#[derive(Debug, Clone)]
pub struct RangeReport {
    /// Ascending civil dates: 1 (day report) or 7 (week report).
    pub days: Vec<Date>,
    /// One row per task identity, biggest first.
    pub tasks: Vec<TaskRow>,
    pub projects: Vec<ProjectTotal>,
    pub grand_total_ms: i64,
    pub tz: TimeZone,
    /// Stretches inside the range with no capture (m32 chunk 0), from the
    /// daemon ledger via [`capture_gaps`]; `build` leaves it empty.
    pub gaps: Vec<(i64, i64)>,
    /// Captured span time no done batch covers yet; `build` leaves it 0.
    pub underived_ms: i64,
    /// Milliseconds per kind of work over every task, biggest first (m32
    /// chunk 1); only the intervals that carry a kind count.
    pub by_kind: Vec<(String, i64)>,
}

/// Kinds where the hands are off: watching an agent, reading, in a call.
const HANDS_OFF: [&str; 3] = ["supervise", "read", "meet"];

/// "hands-on 3h10m · hands-off 4h20m (supervise 2h00m · read 1h20m)": the
/// range's split by kind (m32 chunk 1). `break` counts in neither; the
/// hands-off detail lists its kinds biggest first. Empty when nothing
/// carries a kind.
pub fn hands_split(by_kind: &[(String, i64)]) -> String {
    let mut on = 0;
    let mut off = 0;
    let mut detail = Vec::new();
    for (kind, ms) in by_kind {
        if kind == "break" {
            continue;
        }
        if HANDS_OFF.contains(&kind.as_str()) {
            off += ms;
            detail.push(format!("{kind} {}", fmt_dur(*ms)));
        } else {
            on += ms;
        }
    }
    if on + off == 0 {
        return String::new();
    }
    let mut out = format!("hands-on {} \u{b7} hands-off {}", fmt_dur(on), fmt_dur(off));
    if !detail.is_empty() {
        out.push_str(&format!(" ({})", detail.join(" \u{b7} ")));
    }
    out
}

/// Stretches of `[lo, hi)` no ledger row covers, at least `AFK_SPLIT_MINS`
/// long: a restart's few seconds are not worth a line. Time before the
/// ledger's first row is unrecorded, not a gap; an open row runs to `hi`,
/// so pass `hi` clamped to now.
pub fn capture_gaps(
    runs: &[DaemonRun],
    ledger_start: Option<i64>,
    lo: i64,
    hi: i64,
) -> Vec<(i64, i64)> {
    let Some(first) = ledger_start else {
        return Vec::new();
    };
    let lo = lo.max(first);
    let mut gaps = Vec::new();
    let mut cursor = lo;
    for run in runs {
        if cursor >= hi {
            break;
        }
        if run.start_ts > cursor {
            gaps.push((cursor, run.start_ts.min(hi)));
        }
        cursor = cursor.max(run.end_ts.unwrap_or(hi));
    }
    if cursor < hi {
        gaps.push((cursor, hi));
    }
    gaps.retain(|(a, b)| b - a >= AFK_SPLIT_MINS * 60_000);
    gaps
}

/// "Thu 18:25 → Mon 10:30" in the report's zone.
pub fn fmt_gap(gap: (i64, i64), tz: &TimeZone) -> String {
    let at = |ms: i64| {
        Timestamp::from_millisecond(ms)
            .map(|t| t.to_zoned(tz.clone()).strftime("%a %H:%M").to_string())
            .unwrap_or_default()
    };
    format!("{} \u{2192} {}", at(gap.0), at(gap.1))
}

/// Bucket `tasks` (fetched via `tasks_in_range` for exactly the span of
/// `days`, which must be contiguous and ascending) into one row per task
/// identity with an ms-per-day column, splitting each interval across the
/// local civil days it overlaps.
pub fn build(tasks: &[Task], days: Vec<Date>, tz: &TimeZone) -> Result<RangeReport, jiff::Error> {
    // days.len() + 1 fenceposts: each day's local start, then the range end.
    let mut bounds = Vec::with_capacity(days.len() + 1);
    for d in &days {
        bounds.push(d.to_zoned(tz.clone())?.timestamp().as_millisecond());
    }
    let last = days.last().expect("at least one report day");
    bounds.push(
        last.to_zoned(tz.clone())?
            .checked_add(jiff::Span::new().days(1))?
            .timestamp()
            .as_millisecond(),
    );

    let mut rows: Vec<TaskRow> = Vec::new();
    let mut grand_total_ms = 0;
    for t in tasks {
        let (s, e) = (t.start_ts.as_millisecond(), t.end_ts.as_millisecond());
        for i in 0..days.len() {
            let ms = e.min(bounds[i + 1]) - s.max(bounds[i]);
            if ms <= 0 {
                continue;
            }
            let ms = t.weigh(ms);
            let row = match rows.iter_mut().find(|r| r.task_id == t.id) {
                Some(r) => r,
                None => {
                    rows.push(TaskRow {
                        task_id: t.id,
                        label: t.label.clone(),
                        project: t.project.as_deref().unwrap_or(UNTAGGED).to_string(),
                        by_day: vec![0; days.len()],
                        total_ms: 0,
                        by_kind: Vec::new(),
                    });
                    rows.last_mut().expect("just pushed")
                }
            };
            row.by_day[i] += ms;
            row.total_ms += ms;
            grand_total_ms += ms;
            if let Some(kind) = &t.kind {
                match row.by_kind.iter_mut().find(|(k, _)| k == kind) {
                    Some(e) => e.1 += ms,
                    None => row.by_kind.push((kind.clone(), ms)),
                }
            }
        }
    }
    let mut by_kind: Vec<(String, i64)> = Vec::new();
    for r in &mut rows {
        r.by_kind.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
        for (kind, ms) in &r.by_kind {
            match by_kind.iter_mut().find(|(k, _)| k == kind) {
                Some(e) => e.1 += ms,
                None => by_kind.push((kind.clone(), *ms)),
            }
        }
    }
    by_kind.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
    rows.sort_by_key(|r| std::cmp::Reverse(r.total_ms));
    let (lo, hi) = (bounds[0], bounds[days.len()]);
    Ok(RangeReport {
        days,
        tasks: rows,
        projects: project_totals(tasks, lo, hi),
        grand_total_ms,
        tz: tz.clone(),
        gaps: Vec::new(),
        underived_ms: 0,
        by_kind,
    })
}

fn hours(ms: i64) -> String {
    format!("{:.2}", ms as f64 / 3_600_000.0)
}

fn csv_field(s: &str) -> String {
    if s.contains(['"', ',', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Timesheet CSV: one row per task, decimal-hour day columns a spreadsheet
/// can SUM() directly.
pub fn to_csv(r: &RangeReport) -> String {
    let mut out = String::from("project,task");
    for d in &r.days {
        let _ = write!(out, ",{d}");
    }
    out.push_str(",total_hours\n");
    for t in &r.tasks {
        let _ = write!(out, "{},{}", csv_field(&t.project), csv_field(&t.label));
        for ms in &t.by_day {
            let _ = write!(out, ",{}", hours(*ms));
        }
        let _ = writeln!(out, ",{}", hours(t.total_ms));
    }
    out
}

/// Markdown timesheet: pipe table plus per-project totals.
pub fn to_md(r: &RangeReport) -> String {
    let mut out = String::from("| project | task |");
    for d in &r.days {
        let _ = write!(out, " {d} |");
    }
    out.push_str(" total |\n|---|---|");
    for _ in &r.days {
        out.push_str("---|");
    }
    out.push_str("---|\n");
    for t in &r.tasks {
        let _ = write!(out, "| {} | {} |", t.project, t.label);
        for ms in &t.by_day {
            let cell = if *ms == 0 {
                String::new()
            } else {
                fmt_dur(*ms)
            };
            let _ = write!(out, " {cell} |");
        }
        let _ = writeln!(out, " {} |", fmt_dur(t.total_ms));
    }
    out.push_str("\n## Totals by project\n");
    for p in &r.projects {
        let _ = writeln!(out, "- {}: {}", p.project, fmt_dur(p.total_ms));
    }
    let _ = writeln!(out, "- total: {}", fmt_dur(r.grand_total_ms));
    let split = hands_split(&r.by_kind);
    if !split.is_empty() {
        let _ = writeln!(out, "- {split}");
    }
    if !r.gaps.is_empty() {
        let gaps: Vec<String> = r.gaps.iter().map(|g| fmt_gap(*g, &r.tz)).collect();
        let _ = writeln!(out, "- not captured: {}", gaps.join("; "));
    }
    if r.underived_ms > 0 {
        let _ = writeln!(
            out,
            "- captured, not yet derived: {}",
            fmt_dur(r.underived_ms)
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;

    fn task(id: i64, project: Option<&str>, start: jiff::Zoned, mins: i64) -> Task {
        let start_ts = start.timestamp();
        Task {
            id,
            interval_id: id * 10,
            label: format!("task{id}"),
            project: project.map(str::to_string),
            start_ts,
            end_ts: start_ts + jiff::Span::new().minutes(mins),
            confidence: 1.0,
            declared: false,
            external_ref: None,
            description: None,
            kind: None,
            share: 1.0,
        }
    }

    fn at(date: Date, h: i8, m: i8) -> jiff::Zoned {
        date.at(h, m, 0, 0).to_zoned(TimeZone::UTC).unwrap()
    }

    /// Two rows over one range with shares that sum to 1 report as the
    /// range once, split by task and by project (m32 chunk 3).
    #[test]
    fn shared_rows_sum_to_wall_time() {
        let d: Date = "2026-09-03".parse().unwrap();
        let mut a = task(1, Some("mailer"), at(d, 15, 0), 104);
        a.share = 0.6;
        a.kind = Some("agent".into());
        let mut b = task(2, Some("chronicle"), at(d, 15, 0), 104);
        b.share = 0.4;
        b.kind = Some("supervise".into());
        let r = build(&[a, b], vec![d], &TimeZone::UTC).unwrap();
        assert_eq!(r.grand_total_ms, 104 * 60_000);
        assert_eq!(r.tasks[0].total_ms, 62 * 60_000 + 24_000);
        assert_eq!(r.tasks[1].total_ms, 41 * 60_000 + 36_000);
        assert_eq!(
            r.projects[0].total_ms + r.projects[1].total_ms,
            104 * 60_000
        );
        assert_eq!(
            r.by_kind,
            vec![
                ("agent".into(), 62 * 60_000 + 24_000),
                ("supervise".into(), 41 * 60_000 + 36_000)
            ]
        );
    }

    #[test]
    fn project_totals_groups_and_sorts() {
        let d = civil::date(2026, 8, 24);
        let tasks = vec![
            task(1, Some("chronicle"), at(d, 9, 0), 30),
            task(2, None, at(d, 10, 0), 90),
            task(1, Some("chronicle"), at(d, 11, 0), 30),
        ];
        let totals = project_totals(&tasks, i64::MIN, i64::MAX);
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].project, UNTAGGED);
        assert_eq!(totals[0].total_ms, 90 * 60_000);
        assert_eq!(totals[1].project, "chronicle");
        assert_eq!(totals[1].total_ms, 60 * 60_000);
    }

    fn week(monday: Date) -> Vec<Date> {
        (0..7)
            .map(|i| monday.checked_add(jiff::Span::new().days(i)).unwrap())
            .collect()
    }

    // m32 chunk 1: the range's hands-on / hands-off split by kind.
    #[test]
    fn hands_split_groups_kinds() {
        let by_kind = |v: &[(&str, i64)]| -> Vec<(String, i64)> {
            v.iter()
                .map(|(k, m)| ((*k).to_string(), m * 60_000))
                .collect()
        };
        assert_eq!(
            hands_split(&by_kind(&[
                ("agent", 130),
                ("supervise", 120),
                ("meet", 80),
                ("author", 60),
                ("read", 60),
                ("break", 15),
            ])),
            "hands-on 3h10m \u{b7} hands-off 4h20m (supervise 2h00m \u{b7} meet 1h20m \u{b7} read 1h00m)"
        );
        assert_eq!(
            hands_split(&by_kind(&[("author", 45)])),
            "hands-on 45m00s \u{b7} hands-off 0s"
        );
        assert_eq!(hands_split(&by_kind(&[("break", 15)])), "");
        assert_eq!(hands_split(&[]), "");
    }

    #[test]
    fn buckets_by_start_day() {
        let mon = civil::date(2026, 8, 24);
        let tasks = vec![
            task(1, Some("chronicle"), at(mon, 9, 0), 60),
            task(1, Some("chronicle"), at(mon.tomorrow().unwrap(), 9, 0), 30),
        ];
        let r = build(&tasks, week(mon), &TimeZone::UTC).unwrap();
        assert_eq!(r.tasks.len(), 1);
        assert_eq!(r.tasks[0].by_day[0], 60 * 60_000);
        assert_eq!(r.tasks[0].by_day[1], 30 * 60_000);
        assert_eq!(r.tasks[0].total_ms, 90 * 60_000);
        assert_eq!(r.grand_total_ms, 90 * 60_000);
    }

    #[test]
    fn midnight_span_splits_across_days() {
        let mon = civil::date(2026, 8, 24);
        let tasks = vec![task(1, None, at(mon, 23, 50), 20)];
        let r = build(&tasks, week(mon), &TimeZone::UTC).unwrap();
        assert_eq!(r.tasks[0].by_day[0], 10 * 60_000);
        assert_eq!(r.tasks[0].by_day[1], 10 * 60_000);
        assert_eq!(r.tasks[0].total_ms, 20 * 60_000);
    }

    #[test]
    fn range_edge_interval_clips_to_range() {
        let mon = civil::date(2026, 8, 24);
        let sunday_before = mon.yesterday().unwrap();
        let tasks = vec![
            // 23:55 Sun – 00:05 Mon: only the 5 min inside the range count.
            task(1, None, at(sunday_before, 23, 55), 10),
            // Entirely before the range: no row at all.
            task(2, None, at(sunday_before, 9, 0), 30),
        ];
        let r = build(&tasks, week(mon), &TimeZone::UTC).unwrap();
        assert_eq!(r.tasks.len(), 1);
        assert_eq!(r.tasks[0].task_id, 1);
        assert_eq!(r.tasks[0].by_day[0], 5 * 60_000);
        assert_eq!(r.tasks[0].total_ms, 5 * 60_000);
        assert_eq!(r.grand_total_ms, 5 * 60_000);
        assert!(r.projects.iter().all(|p| p.total_ms == 5 * 60_000));
    }

    #[test]
    fn csv_and_md_output() {
        let mon = civil::date(2026, 8, 24);
        let days = vec![mon, mon.tomorrow().unwrap()];
        let tasks = vec![
            task(1, Some("chronicle"), at(mon, 9, 0), 90),
            task(2, Some("a,b \"c\""), at(mon.tomorrow().unwrap(), 9, 0), 30),
        ];
        let r = build(&tasks, days, &TimeZone::UTC).unwrap();
        assert_eq!(
            to_csv(&r),
            "project,task,2026-08-24,2026-08-25,total_hours\n\
             chronicle,task1,1.50,0.00,1.50\n\
             \"a,b \"\"c\"\"\",task2,0.00,0.50,0.50\n"
        );
        assert_eq!(
            to_md(&r),
            "| project | task | 2026-08-24 | 2026-08-25 | total |\n\
             |---|---|---|---|---|\n\
             | chronicle | task1 | 1h30m |  | 1h30m |\n\
             | a,b \"c\" | task2 |  | 30m00s | 30m00s |\n\
             \n## Totals by project\n\
             - chronicle: 1h30m\n\
             - a,b \"c\": 30m00s\n\
             - total: 2h00m\n"
        );
    }

    // m32 chunk 0: ledger gaps. Time before the first row is unrecorded, an
    // open row runs to `hi`, and a restart's few seconds print nothing.
    #[test]
    fn capture_gaps_from_ledger() {
        let h = 3_600_000;
        let run = |id, start_ts, end_ts| DaemonRun {
            id,
            start_ts,
            end_ts,
            reason: None,
        };
        let runs = vec![
            run(1, h, Some(2 * h)),
            run(2, 2 * h + 10_000, Some(4 * h)),
            run(3, 6 * h, None),
        ];
        assert_eq!(capture_gaps(&runs, Some(h), 0, 8 * h), vec![(4 * h, 6 * h)]);
        assert_eq!(capture_gaps(&runs, Some(h), 0, 5 * h), vec![(4 * h, 5 * h)]);
        assert!(capture_gaps(&runs, None, 0, 8 * h).is_empty());
        assert!(capture_gaps(&[], Some(h), 0, h).is_empty());
        assert_eq!(capture_gaps(&[], Some(h), 0, 3 * h), vec![(h, 3 * h)]);
    }

    #[test]
    fn md_prints_gap_and_underived_lines() {
        let mon = civil::date(2026, 8, 31);
        let mut r = build(&[], vec![mon], &TimeZone::UTC).unwrap();
        let lo = at(mon, 0, 0).timestamp().as_millisecond();
        r.gaps = vec![(lo + 9 * 3_600_000, lo + 10 * 3_600_000 + 1_800_000)];
        r.underived_ms = 170 * 60_000;
        assert!(to_md(&r).ends_with(
            "- total: 0s\n\
             - not captured: Mon 09:00 \u{2192} Mon 10:30\n\
             - captured, not yet derived: 2h50m\n"
        ));
    }
}
