//! Corrections replay eval (m27 chunk 2). Every correction the user made is a
//! labelled example of a batch the pipeline got wrong; this module turns the
//! correction rows into *probes* (a minute range, the task the user ended on,
//! one check) and scores a re-derived batch against them. Pure functions over
//! plain rows so the scoring is unit-testable; `chronicle bench --replay`
//! loads the rows and runs the model.

use std::collections::BTreeSet;

use crate::types::OpenTask;

/// A `corrections` row, as loaded from storage.
#[derive(Debug, Clone)]
pub struct CorrectionRow {
    pub id: i64,
    pub ts: i64,
    pub kind: String,
    pub task_id: i64,
    pub old_label: String,
    pub new_label: String,
    pub old_project: Option<String>,
    pub new_project: Option<String>,
    pub interval_id: Option<i64>,
}

/// An `intervals` row: `task_id` is where the row sits *today*, after every
/// correction that touched it.
#[derive(Debug, Clone)]
pub struct IntervalRow {
    pub id: i64,
    pub task_id: i64,
    pub batch_id: Option<i64>,
    pub start_ts: i64,
    pub end_ts: i64,
    /// The task the row was first placed under (m30 chunk 2.5); `None`
    /// for rows older than migration 018.
    pub origin_task_id: Option<i64>,
    /// An unsure placement nobody has kept or corrected yet and passive
    /// acceptance has not closed (m32 chunk 4): it feeds no profile.
    pub pending: bool,
}

/// Which corrections become probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeSet {
    /// Every kind; merge probes span the target's intervals in the batch
    /// (the model's regression gate since m27).
    #[default]
    All,
    /// `assign`, `reassign` and `eject` only: the direct placements, the
    /// scorer's own unit.
    Direct,
    /// As `All`, but a merge probe spans only the intervals that were the
    /// *source* task's (by `origin_task_id`), not the target's union.
    Source,
}

impl ProbeSet {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "direct" => Some(Self::Direct),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskRow {
    pub id: i64,
    pub label: String,
    pub project: Option<String>,
    pub declared: bool,
    pub closed: bool,
    pub created_ts: i64,
    pub closed_ts: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct BatchRow {
    pub id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// The corrected range resolves to the task the user ended on.
    Placed,
    /// A rename's distinctive tokens appear in the model's label.
    Label,
    /// An ejected range does not land on the ejected task.
    NotEjected,
}

impl Check {
    pub fn name(self) -> &'static str {
        match self {
            Check::Placed => "placed",
            Check::Label => "label",
            Check::NotEjected => "not_ejected",
        }
    }
}

/// One scorable expectation over one batch.
#[derive(Debug, Clone)]
pub struct Probe {
    pub correction_id: i64,
    pub kind: String,
    pub batch_id: i64,
    /// `[start, end)` in UTC ms.
    pub range: (i64, i64),
    /// The task the range should (`Placed`/`Label`) or must not
    /// (`NotEjected`) resolve to.
    pub task_id: i64,
    /// That task's label as the correction left it (its new label for a
    /// rename/assign, the task label at the time for an eject).
    pub task_label: String,
    /// The label the user moved away from; tokens shared with it do not count
    /// as distinctive.
    pub old_label: String,
    pub check: Check,
    /// Tasks the user later merged into `task_id` (transitively). Landing
    /// the range on one of them is landing it on the same work; the merge
    /// happened after the batch, so no predictor could know it yet.
    pub also: Vec<i64>,
}

/// The eval's view of one replayed interval, already linked and resolved.
#[derive(Debug, Clone)]
pub struct Replayed {
    /// Existing task the slot bound to; None for a new-label proposal.
    pub task_id: Option<i64>,
    pub label: String,
    pub start_offset_min: i64,
    pub end_offset_min: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeResult {
    pub correction_id: i64,
    pub batch_id: i64,
    /// The probe's target task.
    pub task_id: i64,
    /// The probe's range as batch-relative minutes, `[lo, hi)`.
    pub range_min: (i64, i64),
    pub kind: String,
    pub check: Check,
    pub pass: bool,
    /// `pass`, or a "new task" verdict over a range whose task did not
    /// exist at the batch's end: the predictor was right to start one, it
    /// just could not name it in the eval's terms.
    pub lenient: bool,
    pub detail: String,
}

pub struct Rows<'a> {
    pub corrections: &'a [CorrectionRow],
    pub intervals: &'a [IntervalRow],
    pub tasks: &'a [TaskRow],
    pub batches: &'a [BatchRow],
}

const EJECT_PAIR_MS: i64 = 120_000;
const KINDS: [&str; 5] = ["rename", "reassign", "merge", "eject", "assign"];

/// Turn corrections into probes. Returns the probes plus one line per
/// correction that could not be scored and why.
pub fn build_probes(rows: &Rows<'_>, set: ProbeSet) -> (Vec<Probe>, Vec<String>) {
    let mut probes = Vec::new();
    let mut skipped = Vec::new();
    let task = |id: i64| rows.tasks.iter().find(|t| t.id == id);
    let interval = |id: i64| rows.intervals.iter().find(|i| i.id == id);
    let mut skip = |c: &CorrectionRow, why: &str| {
        skipped.push(format!("c{} {}: {why}", c.id, c.kind));
    };

    for c in rows.corrections {
        if !KINDS.contains(&c.kind.as_str()) {
            continue;
        }
        if set == ProbeSet::Direct && matches!(c.kind.as_str(), "merge" | "rename") {
            continue;
        }
        match c.kind.as_str() {
            "assign" | "reassign" => {
                let Some(iv) = c.interval_id.and_then(interval) else {
                    skip(c, "no surviving interval");
                    continue;
                };
                let Some(t) = task(iv.task_id) else {
                    skip(c, "target task gone");
                    continue;
                };
                let Some(batch_id) = batch_for(iv, rows.batches) else {
                    skip(c, "no done batch over the range");
                    continue;
                };
                let Some(range) = clip_to_batch((iv.start_ts, iv.end_ts), batch_id, rows.batches)
                else {
                    skip(c, "interval lies outside its batch window");
                    continue;
                };
                probes.push(Probe {
                    correction_id: c.id,
                    kind: c.kind.clone(),
                    batch_id,
                    range,
                    task_id: t.id,
                    task_label: t.label.clone(),
                    old_label: c.old_label.clone(),
                    check: Check::Placed,
                    also: Vec::new(),
                });
            }
            "merge" | "rename" => {
                if c.kind == "rename" && c.old_label.trim() == c.new_label.trim() {
                    skip(c, "no-op rename");
                    continue;
                }
                let target = final_target(c.task_id, c.ts, rows);
                let Some(t) = task(target) else {
                    skip(c, "target task gone");
                    continue;
                };
                let check = if c.kind == "rename" {
                    Check::Label
                } else {
                    Check::Placed
                };
                // One probe per batch: the task's intervals there, as a
                // range from the first start to the last end. Under
                // `Source` a merge spans only what the folded task owned.
                let source = if set == ProbeSet::Source && c.kind == "merge" {
                    match merge_source(c, rows) {
                        Some(id)
                            if rows
                                .intervals
                                .iter()
                                .any(|iv| iv.origin_task_id == Some(id)) =>
                        {
                            Some(id)
                        }
                        Some(_) => {
                            skip(c, "no origin rows (pre-018)");
                            continue;
                        }
                        None => {
                            skip(c, "source task not found");
                            continue;
                        }
                    }
                } else {
                    None
                };
                let owned = |iv: &IntervalRow| match source {
                    Some(id) => iv.origin_task_id == Some(id),
                    None => iv.task_id == target,
                };
                let mut per_batch: std::collections::BTreeMap<i64, (i64, i64)> =
                    std::collections::BTreeMap::new();
                for iv in rows.intervals.iter().filter(|iv| owned(iv)) {
                    let Some(batch_id) = batch_for(iv, rows.batches) else {
                        continue;
                    };
                    let ended_before = rows
                        .batches
                        .iter()
                        .any(|b| b.id == batch_id && b.end_ts <= c.ts);
                    if !ended_before {
                        continue;
                    }
                    per_batch
                        .entry(batch_id)
                        .and_modify(|r| {
                            r.0 = r.0.min(iv.start_ts);
                            r.1 = r.1.max(iv.end_ts);
                        })
                        .or_insert((iv.start_ts, iv.end_ts));
                }
                let mut n = 0;
                for (batch_id, range) in per_batch {
                    let Some(range) = clip_to_batch(range, batch_id, rows.batches) else {
                        continue;
                    };
                    n += 1;
                    probes.push(Probe {
                        correction_id: c.id,
                        kind: c.kind.clone(),
                        batch_id,
                        range,
                        task_id: t.id,
                        task_label: if c.kind == "rename" {
                            c.new_label.clone()
                        } else {
                            t.label.clone()
                        },
                        old_label: c.old_label.clone(),
                        check,
                        also: Vec::new(),
                    });
                }
                if n == 0 {
                    skip(c, "target has no derived intervals before the correction");
                }
            }
            "eject" => {
                // The ejected range is not stored; the assign the user made
                // seconds later over the same time is.
                let pair = rows.corrections.iter().find(|a| {
                    a.kind == "assign"
                        && a.ts >= c.ts
                        && a.ts - c.ts <= EJECT_PAIR_MS
                        && a.interval_id.is_some()
                });
                let Some(iv) = pair.and_then(|a| a.interval_id).and_then(interval) else {
                    skip(c, "no assign within 120 s to recover the range");
                    continue;
                };
                let Some(batch_id) = batch_for(iv, rows.batches) else {
                    skip(c, "no done batch over the range");
                    continue;
                };
                let Some(range) = clip_to_batch((iv.start_ts, iv.end_ts), batch_id, rows.batches)
                else {
                    skip(c, "interval lies outside its batch window");
                    continue;
                };
                probes.push(Probe {
                    correction_id: c.id,
                    kind: c.kind.clone(),
                    batch_id,
                    range,
                    task_id: c.task_id,
                    task_label: c.old_label.clone(),
                    old_label: String::new(),
                    check: Check::NotEjected,
                    also: Vec::new(),
                });
            }
            _ => {}
        }
    }
    for p in &mut probes {
        let batch_end = rows
            .batches
            .iter()
            .find(|b| b.id == p.batch_id)
            .map_or(p.range.1, |b| b.end_ts);
        p.also = merged_into(p.task_id, batch_end, rows);
    }
    (probes, skipped)
}

/// Closed tasks whose merge chain after `after_ts` ends at `target`.
pub fn merged_into(target: i64, after_ts: i64, rows: &Rows<'_>) -> Vec<i64> {
    rows.tasks
        .iter()
        .filter(|t| t.id != target && t.closed)
        .filter(|t| final_target(t.id, after_ts, rows) == target)
        .map(|t| t.id)
        .collect()
}

/// The task a `merge` correction folded away: the closed task carrying
/// `old_label` that closed when the correction was written.
const MERGE_CLOSE_SLACK_MS: i64 = 2_000;
fn merge_source(c: &CorrectionRow, rows: &Rows<'_>) -> Option<i64> {
    rows.tasks
        .iter()
        .filter(|t| t.closed && t.label == c.old_label)
        .filter(|t| {
            t.closed_ts
                .is_some_and(|ts| (ts - c.ts).abs() <= MERGE_CLOSE_SLACK_MS)
        })
        .min_by_key(|t| (t.closed_ts.unwrap_or(0) - c.ts).abs())
        .map(|t| t.id)
}

/// Follow merges forward from `task_id`: a closed task whose label a later
/// `merge` correction names as `old_label` was folded into that correction's
/// task.
pub fn final_target(mut task_id: i64, after_ts: i64, rows: &Rows<'_>) -> i64 {
    let mut seen = BTreeSet::new();
    while seen.insert(task_id) {
        let Some(t) = rows.tasks.iter().find(|t| t.id == task_id) else {
            break;
        };
        if !t.closed {
            break;
        }
        let hop = rows
            .corrections
            .iter()
            .filter(|c| c.kind == "merge" && c.ts >= after_ts && c.old_label == t.label)
            .min_by_key(|c| c.ts);
        match hop {
            Some(c) => task_id = c.task_id,
            None => break,
        }
    }
    task_id
}

/// The interval's own batch, else the done batch overlapping it most.
fn batch_for(iv: &IntervalRow, batches: &[BatchRow]) -> Option<i64> {
    if let Some(id) = iv.batch_id
        && batches.iter().any(|b| b.id == id)
    {
        return Some(id);
    }
    batches
        .iter()
        .map(|b| (b.end_ts.min(iv.end_ts) - b.start_ts.max(iv.start_ts), b.id))
        .filter(|(overlap, _)| *overlap > 0)
        .max_by_key(|(overlap, _)| *overlap)
        .map(|(_, id)| id)
}

/// Clip a probe range to its batch window (a stored row can outlive the
/// window it was derived in); None when nothing is left.
fn clip_to_batch(range: (i64, i64), batch_id: i64, batches: &[BatchRow]) -> Option<(i64, i64)> {
    let b = batches.iter().find(|b| b.id == batch_id)?;
    let lo = range.0.max(b.start_ts);
    let hi = range.1.min(b.end_ts);
    (hi > lo).then_some((lo, hi))
}

/// The open-task list as the worker would have built it at `end_ts`: tasks
/// that existed and were open then, declared first, derived ones only with an
/// interval before then (newest first), capped; labels and projects rewound
/// through later renames so a corrected label cannot leak into the prompt.
pub fn open_tasks_at(
    tasks: &[TaskRow],
    intervals: &[IntervalRow],
    corrections: &[CorrectionRow],
    end_ts: i64,
    cap: usize,
) -> Vec<OpenTask> {
    let existed = |t: &TaskRow| t.created_ts < end_ts && t.closed_ts.is_none_or(|c| c > end_ts);
    let as_open = |t: &TaskRow| {
        let (label, project) = label_at(t, end_ts, corrections);
        OpenTask {
            id: t.id,
            label,
            project,
            declared: t.declared,
        }
    };
    let mut declared: Vec<&TaskRow> = tasks.iter().filter(|t| t.declared && existed(t)).collect();
    declared.sort_by_key(|t| (t.created_ts, t.id));
    let mut out: Vec<OpenTask> = declared.into_iter().map(as_open).collect();

    let mut derived: Vec<(i64, &TaskRow)> = tasks
        .iter()
        .filter(|t| !t.declared && existed(t))
        .filter_map(|t| {
            intervals
                .iter()
                .filter(|iv| iv.task_id == t.id && iv.end_ts <= end_ts)
                .map(|iv| iv.end_ts)
                .max()
                .map(|last| (last, t))
        })
        .collect();
    derived.sort_by_key(|(last, t)| (std::cmp::Reverse(*last), t.id));
    for (_, t) in derived {
        if out.len() >= cap {
            break;
        }
        out.push(as_open(t));
    }
    out
}

/// The task's label/project at `at_ts`: the `old_*` side of the earliest
/// rename recorded after that instant, else what it carries now.
pub fn label_at(
    t: &TaskRow,
    at_ts: i64,
    corrections: &[CorrectionRow],
) -> (String, Option<String>) {
    corrections
        .iter()
        .filter(|c| c.kind == "rename" && c.task_id == t.id && c.ts > at_ts)
        .min_by_key(|c| c.ts)
        .map(|c| (c.old_label.clone(), c.old_project.clone()))
        .unwrap_or_else(|| (t.label.clone(), t.project.clone()))
}

/// Lowercase alphanumeric tokens of `label` (≥ 3 chars) not present in
/// `against`; falls back to all of `label`'s tokens when nothing is
/// distinctive.
pub fn distinctive_tokens(label: &str, against: &str) -> Vec<String> {
    let toks = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .map(str::to_owned)
            .collect()
    };
    let all = toks(label);
    let other = toks(against);
    let distinct: Vec<String> = all.iter().filter(|w| !other.contains(w)).cloned().collect();
    if distinct.is_empty() { all } else { distinct }
}

fn shares_token(label: &str, tokens: &[String]) -> bool {
    let l = label.to_lowercase();
    tokens.iter().any(|t| l.contains(t.as_str()))
}

/// Score one probe against a batch's replayed intervals. `batch_start` is
/// the offset origin of `replayed`.
pub fn score(
    probe: &Probe,
    batch_start: i64,
    replayed: &[Replayed],
    open_at: &[OpenTask],
) -> ProbeResult {
    let lo = (probe.range.0 - batch_start) / 60_000;
    let hi = (probe.range.1 - batch_start + 59_999) / 60_000;
    let dominant = replayed
        .iter()
        .map(|r| (hi.min(r.end_offset_min) - lo.max(r.start_offset_min), r))
        .filter(|(overlap, _)| *overlap > 0)
        .max_by_key(|(overlap, _)| *overlap)
        .map(|(_, r)| r);
    let base = |pass: bool, detail: String| ProbeResult {
        correction_id: probe.correction_id,
        batch_id: probe.batch_id,
        task_id: probe.task_id,
        range_min: (lo, hi),
        kind: probe.kind.clone(),
        check: probe.check,
        pass,
        lenient: pass,
        detail,
    };
    let is_target =
        |id: Option<i64>| id.is_some_and(|id| id == probe.task_id || probe.also.contains(&id));
    let Some(d) = dominant else {
        return base(
            probe.check == Check::NotEjected,
            format!("{lo}–{hi}m: nothing replayed over the range"),
        );
    };
    let tokens = distinctive_tokens(&probe.task_label, &probe.old_label);
    let where_ = format!("{lo}–{hi}m → {}", d.label);
    match probe.check {
        Check::Placed => {
            if d.task_id == Some(probe.task_id) {
                return base(true, format!("{where_} (ref)"));
            }
            if is_target(d.task_id) {
                return base(true, format!("{where_} (merged into it later)"));
            }
            let in_list = open_at.iter().any(|t| t.id == probe.task_id);
            if !in_list && shares_token(&d.label, &tokens) {
                return base(true, format!("{where_} (by label; task not open then)"));
            }
            let mut r = base(false, format!("{where_}; wanted {}", probe.task_label));
            if !in_list && d.task_id.is_none() {
                r.lenient = true;
                r.detail.push_str(" (new; task not open then)");
            }
            r
        }
        Check::Label => {
            let pass = shares_token(&d.label, &tokens);
            base(pass, format!("{where_}; wanted one of {tokens:?}"))
        }
        Check::NotEjected => {
            let same_task = is_target(d.task_id);
            let same_label = shares_token(&d.label, &tokens);
            base(
                !(same_task || same_label),
                format!("{where_}; ejected {}", probe.task_label),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: i64, label: &str, declared: bool, closed_ts: Option<i64>) -> TaskRow {
        TaskRow {
            id,
            label: label.into(),
            project: None,
            declared,
            closed: closed_ts.is_some(),
            created_ts: 0,
            closed_ts,
        }
    }
    fn iv(id: i64, task_id: i64, batch_id: i64, lo: i64, hi: i64) -> IntervalRow {
        IntervalRow {
            id,
            task_id,
            batch_id: Some(batch_id),
            start_ts: lo,
            end_ts: hi,
            origin_task_id: Some(task_id),
            pending: false,
        }
    }
    fn corr(
        id: i64,
        ts: i64,
        kind: &str,
        task_id: i64,
        old: &str,
        new: &str,
        interval_id: Option<i64>,
    ) -> CorrectionRow {
        CorrectionRow {
            id,
            ts,
            kind: kind.into(),
            task_id,
            old_label: old.into(),
            new_label: new.into(),
            old_project: None,
            new_project: None,
            interval_id,
        }
    }
    const M: i64 = 60_000;
    fn batches() -> Vec<BatchRow> {
        vec![
            BatchRow {
                id: 1,
                start_ts: 0,
                end_ts: 30 * M,
            },
            BatchRow {
                id: 2,
                start_ts: 30 * M,
                end_ts: 60 * M,
            },
        ]
    }

    #[test]
    fn direct_and_source_probe_sets() {
        // Task 10 ("old work") was merged into 20 at 50m; interval 8 was
        // 10's own (origin), interval 5 was always 20's.
        let tasks = vec![
            task(10, "old work", false, Some(50 * M)),
            task(20, "real work", true, None),
        ];
        let mut own = iv(8, 20, 1, 14 * M, 16 * M);
        own.origin_task_id = Some(10);
        let intervals = vec![
            iv(5, 20, 1, 2 * M, 12 * M),
            own,
            iv(6, 20, 2, 31 * M, 40 * M),
        ];
        let corrections = vec![
            corr(1, 45 * M, "reassign", 20, "x", "real work", Some(5)),
            corr(2, 50 * M, "merge", 20, "old work", "real work", None),
        ];
        let b = batches();
        let rows = Rows {
            corrections: &corrections,
            intervals: &intervals,
            tasks: &tasks,
            batches: &b,
        };
        let (all, _) = build_probes(&rows, ProbeSet::All);
        let merge_all: Vec<_> = all.iter().filter(|p| p.kind == "merge").collect();
        assert_eq!(merge_all.len(), 1);
        assert_eq!(merge_all[0].range, (2 * M, 16 * M), "target's union");
        let (direct, _) = build_probes(&rows, ProbeSet::Direct);
        assert!(direct.iter().all(|p| p.kind == "reassign"), "{direct:?}");
        assert_eq!(direct.len(), 1);
        let (source, _) = build_probes(&rows, ProbeSet::Source);
        let merge_src: Vec<_> = source.iter().filter(|p| p.kind == "merge").collect();
        assert_eq!(merge_src.len(), 1);
        assert_eq!(
            merge_src[0].range,
            (14 * M, 16 * M),
            "the folded task's own rows"
        );
        assert_eq!(merge_src[0].task_id, 20);
        // Without origin rows the merge is skipped with a reason, not scored.
        let legacy: Vec<IntervalRow> = intervals
            .iter()
            .cloned()
            .map(|mut r| {
                r.origin_task_id = None;
                r
            })
            .collect();
        let rows = Rows {
            corrections: &corrections,
            intervals: &legacy,
            tasks: &tasks,
            batches: &b,
        };
        let (source, skipped) = build_probes(&rows, ProbeSet::Source);
        assert!(source.iter().all(|p| p.kind != "merge"));
        assert!(skipped.iter().any(|s| s.contains("pre-018")), "{skipped:?}");
    }

    #[test]
    fn probes_per_kind_and_chain_hop() {
        // Task 10 (closed) merged into 20; interval 5 reassigned, now on 20.
        let tasks = vec![
            task(10, "old work", false, Some(50 * M)),
            task(20, "real work", true, None),
            task(30, "wordle", false, None),
        ];
        let intervals = vec![
            iv(5, 20, 1, 2 * M, 12 * M),
            iv(8, 20, 1, 14 * M, 16 * M),
            iv(6, 20, 2, 31 * M, 40 * M),
            iv(7, 30, 2, 41 * M, 43 * M),
        ];
        let corrections = vec![
            corr(1, 45 * M, "reassign", 10, "x", "old work", Some(5)),
            corr(2, 50 * M, "merge", 20, "old work", "real work", None),
            corr(
                3,
                70 * M,
                "rename",
                20,
                "real work",
                "chronicle replay eval",
                None,
            ),
            corr(4, 70 * M, "rename", 20, "same", "same", None),
            corr(5, 80 * M, "eject", 30, "wordle", "(unassigned)", None),
            corr(
                6,
                80 * M + 5_000,
                "assign",
                20,
                "(unassigned)",
                "real work",
                Some(7),
            ),
            corr(7, 90 * M, "assign", 20, "(unassigned)", "real work", None),
        ];
        let b = batches();
        let rows = Rows {
            corrections: &corrections,
            intervals: &intervals,
            tasks: &tasks,
            batches: &b,
        };
        let (probes, skipped) = build_probes(&rows, ProbeSet::All);
        assert_eq!(final_target(10, 45 * M, &rows), 20);
        let kinds: Vec<(i64, &str, i64)> = probes
            .iter()
            .map(|p| (p.correction_id, p.kind.as_str(), p.batch_id))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (1, "reassign", 1),
                (2, "merge", 1),
                (3, "rename", 1),
                (3, "rename", 2),
                (5, "eject", 2),
                (6, "assign", 2),
            ],
            "{probes:?}"
        );
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped.iter().any(|s| s.contains("no-op rename")));
        assert!(skipped.iter().any(|s| s.starts_with("c7 ")));
        let rename1 = probes.iter().find(|p| p.kind == "rename").unwrap();
        assert_eq!(rename1.range, (2 * M, 16 * M));
        let eject = probes.iter().find(|p| p.kind == "eject").unwrap();
        assert_eq!(eject.range, (41 * M, 43 * M));
        assert_eq!(eject.task_id, 30);
    }

    #[test]
    fn open_list_rewinds_renames_and_orders() {
        let tasks = vec![
            task(1, "new name", true, None),
            task(2, "derived a", false, None),
            task(3, "derived b", false, None),
            task(4, "later", true, None),
            task(5, "closed early", true, Some(5 * M)),
        ];
        let mut tasks = tasks;
        tasks[3].created_ts = 40 * M;
        let intervals = vec![
            iv(1, 2, 1, 0, 10 * M),
            iv(2, 3, 1, 12 * M, 20 * M),
            iv(3, 3, 2, 35 * M, 40 * M),
        ];
        let corrections = vec![corr(1, 50 * M, "rename", 1, "old name", "new name", None)];
        let open = open_tasks_at(&tasks, &intervals, &corrections, 30 * M, 8);
        let ids: Vec<i64> = open.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![1, 3, 2]);
        assert_eq!(open[0].label, "old name");
        let capped = open_tasks_at(&tasks, &intervals, &corrections, 30 * M, 2);
        assert_eq!(capped.len(), 2);
    }

    #[test]
    fn scoring_checks() {
        let open = vec![OpenTask {
            id: 20,
            label: "real work".into(),
            project: None,
            declared: true,
        }];
        let replayed = vec![
            Replayed {
                task_id: Some(20),
                label: "real work".into(),
                start_offset_min: 0,
                end_offset_min: 15,
            },
            Replayed {
                task_id: None,
                label: "playing wordle".into(),
                start_offset_min: 15,
                end_offset_min: 20,
            },
        ];
        let probe = |check, task_id, task_label: &str, old: &str, range| Probe {
            correction_id: 1,
            kind: "x".into(),
            batch_id: 1,
            range,
            task_id,
            task_label: task_label.into(),
            old_label: old.into(),
            check,
            also: Vec::new(),
        };
        assert!(
            score(
                &probe(Check::Placed, 20, "real work", "", (2 * M, 12 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert!(
            !score(
                &probe(Check::Placed, 20, "real work", "", (16 * M, 19 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        // Task not in the open list: label match counts.
        let r = score(
            &probe(Check::Placed, 99, "wordle break", "", (16 * M, 19 * M)),
            0,
            &replayed,
            &open,
        );
        assert!(r.pass && r.detail.contains("by label"), "{r:?}");
        assert!(
            !score(
                &probe(
                    Check::Label,
                    20,
                    "real chronicle work",
                    "real work",
                    (0, 10 * M)
                ),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert!(
            score(
                &probe(Check::Label, 20, "wordle", "games", (16 * M, 19 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert!(
            !score(
                &probe(Check::NotEjected, 20, "real work", "", (0, 10 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert!(
            score(
                &probe(Check::NotEjected, 20, "real work", "", (16 * M, 19 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert!(
            score(
                &probe(Check::NotEjected, 20, "real work", "", (25 * M, 29 * M)),
                0,
                &replayed,
                &open
            )
            .pass
        );
        assert_eq!(
            distinctive_tokens("chronicle replay eval", "real work"),
            vec!["chronicle", "replay", "eval"]
        );
        assert_eq!(
            distinctive_tokens("real work", "real work"),
            vec!["real", "work"]
        );
    }
}
