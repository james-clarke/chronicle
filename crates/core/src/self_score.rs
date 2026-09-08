//! The derivation's daily self-score (m32 chunk 6): per local day, what
//! capture saw, how much of it derivation placed, and what the user undid
//! — tasks minted and merged away within a day, ejects, renames, and the
//! verdict log's confident-wrong rate. The daemon recomputes the last
//! [`DAYS`] days once a day (a correction lands after the day it judges);
//! `chronicle status` and Settings › Derivation read the rows back.
//! `bench --calibrate` stays the offline check of the same verdicts.

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use rusqlite::Connection;

use crate::report;
use crate::storage::{self, SelfScore, StorageError};
use crate::types::ts_to_ms;

/// Days shown, today included.
pub const DAYS: usize = 7;

const META_KEY: &str = "self_score_ts";
const EVERY_MS: i64 = 86_400_000;

/// Score one local day as of `now` (today's row ends at `now`, not
/// midnight, so its gaps and underived time are real).
pub fn score_day(
    conn: &Connection,
    day: Date,
    tz: &TimeZone,
    now: Timestamp,
) -> Result<SelfScore, StorageError> {
    let now_ms = ts_to_ms(now);
    let bound = |d: Date| d.to_zoned(tz.clone()).map(|z| ts_to_ms(z.timestamp()));
    let lo = bound(day).map_err(jiff_err)?;
    let hi = day
        .tomorrow()
        .map_err(jiff_err)
        .and_then(|d| bound(d).map_err(jiff_err))?
        .min(now_ms);
    let mut row = storage::self_score_counts(conn, lo, hi)?;
    let (first, runs) = storage::runs_for_report(conn, lo, hi)?;
    row.uncaptured_ms = report::capture_gaps(&runs, first, lo, hi)
        .iter()
        .map(|(a, b)| b - a)
        .sum();
    row.day = day.to_string();
    row.computed_ts = now_ms;
    Ok(row)
}

/// Recompute and store the last [`DAYS`] days, oldest first.
pub fn refresh(
    conn: &Connection,
    now: Timestamp,
    tz: &TimeZone,
) -> Result<Vec<SelfScore>, StorageError> {
    let today = now.to_zoned(tz.clone()).date();
    let mut rows = Vec::with_capacity(DAYS);
    for back in (0..DAYS as i64).rev() {
        let day = today
            .checked_sub(jiff::Span::new().days(back))
            .map_err(jiff_err)?;
        let row = score_day(conn, day, tz, now)?;
        storage::upsert_self_score(conn, &row)?;
        rows.push(row);
    }
    Ok(rows)
}

/// Once a day from the daemon tick (stamp in `meta`); true when it ran.
pub fn daily(conn: &Connection, now: Timestamp, tz: &TimeZone) -> Result<bool, StorageError> {
    let now_ms = ts_to_ms(now);
    let last = storage::get_meta(conn, META_KEY)?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now_ms - last < EVERY_MS {
        return Ok(false);
    }
    refresh(conn, now, tz)?;
    storage::set_meta(conn, META_KEY, Some(&now_ms.to_string()))?;
    Ok(true)
}

fn jiff_err(e: jiff::Error) -> StorageError {
    StorageError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

/// The rows folded into one: the numbers `chronicle status` prints.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub days: usize,
    pub first_day: String,
    pub last_day: String,
    pub computed_ts: i64,
    pub active_ms: i64,
    pub uncaptured_ms: i64,
    pub underived_ms: i64,
    pub placed_ms: i64,
    pub minted: i64,
    pub merged: i64,
    pub placements: i64,
    pub ejects: i64,
    pub renames: i64,
    pub verdicts: i64,
    pub wrong: i64,
    pub confident: i64,
    pub confident_wrong: i64,
}

impl Summary {
    pub fn of(rows: &[SelfScore]) -> Self {
        let mut s = Summary {
            days: rows.len(),
            first_day: rows.first().map(|r| r.day.clone()).unwrap_or_default(),
            last_day: rows.last().map(|r| r.day.clone()).unwrap_or_default(),
            ..Summary::default()
        };
        for r in rows {
            s.computed_ts = s.computed_ts.max(r.computed_ts);
            s.active_ms += r.active_ms;
            s.uncaptured_ms += r.uncaptured_ms;
            s.underived_ms += r.underived_ms;
            s.placed_ms += r.placed_ms;
            s.minted += r.minted;
            s.merged += r.merged;
            s.placements += r.placements;
            s.ejects += r.ejects;
            s.renames += r.renames;
            s.verdicts += r.verdicts;
            s.wrong += r.wrong;
            s.confident += r.confident;
            s.confident_wrong += r.confident_wrong;
        }
        s
    }

    /// Placed time over active time, capped at 100 %.
    pub fn coverage(&self) -> Option<f64> {
        (self.active_ms > 0).then(|| (self.placed_ms as f64 / self.active_ms as f64).min(1.0))
    }

    /// The status lines under a `self-score` heading, one per family.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(4);
        let mut capture = match self.coverage() {
            Some(_) => format!(
                "coverage {} of {} active",
                pct(self.placed_ms.min(self.active_ms), self.active_ms),
                fmt_ms(self.active_ms)
            ),
            None => "nothing active".to_owned(),
        };
        if self.uncaptured_ms > 0 {
            capture.push_str(&format!(", not captured {}", fmt_ms(self.uncaptured_ms)));
        }
        if self.underived_ms > 0 {
            capture.push_str(&format!(", not derived {}", fmt_ms(self.underived_ms)));
        }
        out.push(capture);
        out.push(format!(
            "tasks minted {}, merged within a day {} ({})",
            self.minted,
            self.merged,
            pct(self.merged, self.minted)
        ));
        out.push(format!(
            "ejects {} of {} placements ({}), renames {}",
            self.ejects,
            self.placements,
            pct(self.ejects, self.placements),
            self.renames
        ));
        out.push(format!(
            "verdicts {} closed, {} wrong ({}); confident-wrong {} of {} ({})",
            self.verdicts,
            self.wrong,
            pct(self.wrong, self.verdicts),
            self.confident_wrong,
            self.confident,
            pct(self.confident_wrong, self.confident)
        ));
        out
    }
}

/// "12%", or "–" when there is nothing to take a share of.
pub fn pct(part: i64, whole: i64) -> String {
    if whole <= 0 {
        "\u{2013}".to_owned()
    } else {
        format!("{:.0}%", part as f64 / whole as f64 * 100.0)
    }
}

/// "3h10m" / "45m" / "0m".
pub fn fmt_ms(ms: i64) -> String {
    let mins = ms / 60_000;
    if mins >= 60 {
        format!("{}h{:02}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;
    use rusqlite::params;

    fn at(day: Date, h: i8, m: i8) -> i64 {
        ts_to_ms(
            day.at(h, m, 0, 0)
                .to_zoned(TimeZone::UTC)
                .unwrap()
                .timestamp(),
        )
    }

    // Two days of fixtures: spans, placements, a merge of a day-old task, an
    // eject, a rename and closed verdicts; each lands on the day it belongs
    // to, and the rows agree with the tables they were counted from.
    #[test]
    fn scores_each_day_from_its_own_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        storage::test_migrate(&mut conn);
        let tz = TimeZone::UTC;
        let d1 = Date::constant(2026, 9, 7);
        let d2 = Date::constant(2026, 9, 8);
        let now = ms_to_ts(at(d2, 12, 0));
        // Day 1: 2 h of focus, 1 h AFK; day 2: 1 h of focus, half of it in a
        // done batch.
        conn.execute(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, ?1, ?2, 'done'), (2, ?3, ?4, 'pending')",
            params![at(d1, 9, 0), at(d1, 11, 0), at(d2, 9, 0), at(d2, 9, 30)],
        )
        .unwrap();
        for (lo, hi, kind, batch) in [
            (at(d1, 9, 0), at(d1, 11, 0), "focus", Some(1)),
            (at(d1, 11, 0), at(d1, 12, 0), "afk", None),
            (at(d2, 9, 0), at(d2, 9, 30), "focus", Some(2)),
            (at(d2, 9, 30), at(d2, 10, 0), "focus", None),
        ] {
            conn.execute(
                "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, 'a', 't', ?3, ?4)",
                params![lo, hi, kind, batch],
            )
            .unwrap();
        }
        // Tasks: 10 and 11 minted on day 1, 12 (user) declared on day 1.
        for (id, source, created) in [
            (10, "derived", at(d1, 9, 5)),
            (11, "derived", at(d1, 10, 0)),
            (12, "user", at(d1, 8, 0)),
        ] {
            conn.execute(
                "INSERT INTO tasks (id, label, status, source, created_ts) VALUES (?1, 't', 'open', ?2, ?3)",
                params![id, source, created],
            )
            .unwrap();
        }
        // Day 1's two hours placed under 10 (share 0.5) and 11 (0.5), both
        // written on day 1; a user row on day 2 is not a placement.
        for (task, lo, hi, share, source, created) in [
            (
                10,
                at(d1, 9, 0),
                at(d1, 11, 0),
                0.5,
                "segment",
                at(d1, 11, 1),
            ),
            (
                11,
                at(d1, 9, 0),
                at(d1, 11, 0),
                0.5,
                "segment",
                at(d1, 11, 1),
            ),
            (12, at(d2, 9, 0), at(d2, 9, 30), 1.0, "user", at(d2, 9, 31)),
        ] {
            conn.execute(
                "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, share, source, created_ts)
                 VALUES (?1, 1, ?2, ?3, 0.9, ?4, ?5, ?6)",
                params![task, lo, hi, share, source, created],
            )
            .unwrap();
        }
        // Verdicts placed on day 1: three closed (one confident and wrong),
        // one still open.
        for (ts, confident, outcome) in [
            (at(d1, 9, 10), 1, Some("right")),
            (at(d1, 9, 20), 1, Some("wrong")),
            (at(d1, 9, 30), 0, Some("wrong")),
            (at(d1, 9, 40), 1, None),
        ] {
            conn.execute(
                "INSERT INTO verdict_log (ts, task_id, margin, confident, outcome) VALUES (?1, 10, 0.2, ?2, ?3)",
                params![ts, confident, outcome],
            )
            .unwrap();
        }
        // Day 2: 11 (born day 1, 23 h earlier) merges into 10, and its row
        // goes; then an eject and a rename.
        storage::merge_task(&mut conn, ms_to_ts(at(d2, 9, 0)), 11, 10).unwrap();
        let gone: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id=11", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gone, 0, "a derived source with no corrections is removed");
        for (kind, ts) in [("eject", at(d2, 9, 5)), ("rename", at(d2, 9, 6))] {
            conn.execute(
                "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind) VALUES (?1, 10, 'a', 'b', '', ?2)",
                params![ts, kind],
            )
            .unwrap();
        }
        // Ledger: one run over day 1 from 08:00 to 22:00, none on day 2.
        conn.execute(
            "INSERT INTO daemon_runs (start_ts, end_ts, reason) VALUES (?1, ?2, 'shutdown')",
            params![at(d1, 8, 0), at(d1, 22, 0)],
        )
        .unwrap();

        let rows = refresh(&conn, now, &tz).unwrap();
        assert_eq!(rows.len(), DAYS);
        let day1 = rows.iter().find(|r| r.day == "2026-09-07").unwrap();
        let day2 = rows.iter().find(|r| r.day == "2026-09-08").unwrap();
        let h = 3_600_000;
        assert_eq!(
            (
                day1.active_ms,
                day1.placed_ms,
                day1.underived_ms,
                day1.uncaptured_ms
            ),
            (2 * h, 2 * h, 0, 2 * h),
            "day 1: two hours active and placed; the run ends at 22:00"
        );
        assert_eq!(
            (day1.minted, day1.merged, day1.placements),
            (2, 1, 2),
            "day 1 minted 10 and the merged-away 11; 11 went within a day"
        );
        assert_eq!((day1.ejects, day1.renames), (0, 0));
        assert_eq!(
            (
                day1.verdicts,
                day1.wrong,
                day1.confident,
                day1.confident_wrong
            ),
            (3, 2, 2, 1)
        );
        assert_eq!(
            (
                day2.active_ms,
                day2.placed_ms,
                day2.underived_ms,
                day2.uncaptured_ms
            ),
            (h, h / 2, h, 12 * h),
            "day 2: the pending batch and the tail are underived; no ledger row"
        );
        assert_eq!((day2.minted, day2.merged, day2.placements), (0, 0, 0));
        assert_eq!((day2.ejects, day2.renames), (1, 1));
        assert_eq!(day2.verdicts, 0);
        // Stored, and read back oldest first.
        let back = storage::self_scores(&conn, DAYS).unwrap();
        assert_eq!(back, rows);
        // The stamp gates the daily run.
        assert!(daily(&conn, now, &tz).unwrap());
        assert!(!daily(&conn, now, &tz).unwrap());

        let s = Summary::of(&rows);
        assert_eq!(
            (s.minted, s.merged, s.ejects, s.confident_wrong),
            (2, 1, 1, 1)
        );
        assert_eq!(s.coverage().map(|c| (c * 100.0).round()), Some(83.0));
        let lines = s.lines();
        assert_eq!(lines[1], "tasks minted 2, merged within a day 1 (50%)");
        assert_eq!(
            lines[3],
            "verdicts 3 closed, 2 wrong (67%); confident-wrong 1 of 2 (50%)"
        );
    }

    #[test]
    fn pct_and_ms_render() {
        assert_eq!(pct(1, 3), "33%");
        assert_eq!(pct(0, 0), "\u{2013}");
        assert_eq!(fmt_ms(3_600_000 * 3 + 600_000), "3h10m");
        assert_eq!(fmt_ms(59_000), "0m");
    }
}
