//! Day/week report aggregation over task intervals. Pure fold over
//! `storage::tasks_in_range` rows — day bucketing is local-civil-time (jiff),
//! which SQLite's UTC-only DATE() can't do. An interval is split across the
//! civil days it overlaps and clamped to the report range: declared-task
//! intervals can span many hours (real data has 10h+ crossing midnight), so
//! bucketing whole durations by start day would double-count them in
//! adjacent reports.

use std::fmt::Write;

use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::digest::fmt_dur;
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
    (t.end_ts.as_millisecond().min(hi) - t.start_ts.as_millisecond().max(lo)).max(0)
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
pub struct TaskRow {
    pub task_id: i64,
    pub label: String,
    pub project: String,
    /// Milliseconds per day, aligned index-for-index with
    /// [`RangeReport::days`].
    pub by_day: Vec<i64>,
    pub total_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RangeReport {
    /// Ascending civil dates: 1 (day report) or 7 (week report).
    pub days: Vec<Date>,
    /// One row per task identity, biggest first.
    pub tasks: Vec<TaskRow>,
    pub projects: Vec<ProjectTotal>,
    pub grand_total_ms: i64,
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
            let row = match rows.iter_mut().find(|r| r.task_id == t.id) {
                Some(r) => r,
                None => {
                    rows.push(TaskRow {
                        task_id: t.id,
                        label: t.label.clone(),
                        project: t.project.as_deref().unwrap_or(UNTAGGED).to_string(),
                        by_day: vec![0; days.len()],
                        total_ms: 0,
                    });
                    rows.last_mut().expect("just pushed")
                }
            };
            row.by_day[i] += ms;
            row.total_ms += ms;
            grand_total_ms += ms;
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.total_ms));
    let (lo, hi) = (bounds[0], bounds[days.len()]);
    Ok(RangeReport {
        days,
        tasks: rows,
        projects: project_totals(tasks, lo, hi),
        grand_total_ms,
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
        }
    }

    fn at(date: Date, h: i8, m: i8) -> jiff::Zoned {
        date.at(h, m, 0, 0).to_zoned(TimeZone::UTC).unwrap()
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
}
