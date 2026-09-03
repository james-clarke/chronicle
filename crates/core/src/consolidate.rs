//! Day-tier consolidation (m27 chunk 6): fold the day's orphan derived
//! tasks into the task around them, then let the model merge duplicates
//! and rename fresh vague labels — every model suggestion passes the guards
//! here before storage applies it, and the whole run is one `consolidate`
//! correction whose `ctx` holds the before-state for undo.

use std::collections::BTreeSet;
use std::fmt::Write as _;

/// One task with intervals in the day, as the model and the guards see it.
#[derive(Debug, Clone, PartialEq)]
pub struct DayTask {
    pub id: i64,
    pub label: String,
    pub project: Option<String>,
    pub external_ref: Option<String>,
    /// Declared by the user (`source='user'`): listed for reference, never
    /// merged into or out of, never renamed.
    pub locked: bool,
    pub total_ms: i64,
    pub intervals: usize,
    /// Distinct batches the intervals came from; renames only touch tasks
    /// from a single batch (fresh labels).
    pub batches: usize,
    /// Journal entries or a checkpoint exist: the task is "real".
    pub has_notes: bool,
    /// The user renamed it at some point: keep their wording.
    pub user_renamed: bool,
    /// `(app, title, ms)` strongest first, at most three.
    pub evidence: Vec<(String, String, i64)>,
}

/// The model's answer, as the grammar shapes it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct ModelPlan {
    #[serde(default)]
    pub merges: Vec<ModelMerge>,
    #[serde(default)]
    pub renames: Vec<ModelRename>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelMerge {
    pub into: i64,
    pub from: Vec<i64>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ModelRename {
    pub id: i64,
    pub label: String,
}

/// What storage applies: `(from, into)` merges and `(id, new label)` renames.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub merges: Vec<(i64, i64)>,
    pub renames: Vec<(i64, String)>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.merges.is_empty() && self.renames.is_empty()
    }
}

/// Before-state of one applied run, stored as JSON in the `consolidate`
/// correction's `ctx` so "undo tidy" can reverse it in one transaction.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Before {
    pub day: String,
    pub merges: Vec<MergeBefore>,
    pub renames: Vec<RenameBefore>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MergeBefore {
    pub from: i64,
    pub into: i64,
    pub label: String,
    pub project: Option<String>,
    pub source: String,
    pub created_ts: i64,
    pub external_ref: Option<String>,
    pub interval_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RenameBefore {
    pub id: i64,
    pub old_label: String,
    pub new_label: String,
}

/// Orphans fold without the model: a derived task under this much total
/// time, with no project, ticket, or notes, whose every interval sits
/// between two intervals of one other task.
pub const ORPHAN_MAX_MS: i64 = 3 * 60_000;
/// Model merges applied per run, at most.
pub const MAX_MERGES: usize = 6;

/// Deterministic first pass over the day's intervals (`(task_id, start,
/// end)` in time order): `(orphan, surrounding task)` pairs.
pub fn orphan_folds(tasks: &[DayTask], intervals: &[(i64, i64, i64)]) -> Vec<(i64, i64)> {
    let is_orphan = |t: &DayTask| {
        !t.locked
            && t.total_ms < ORPHAN_MAX_MS
            && t.project.is_none()
            && t.external_ref.is_none()
            && !t.has_notes
    };
    let mut out = Vec::new();
    for t in tasks.iter().filter(|t| is_orphan(t)) {
        let mut around: Option<i64> = None;
        let mut ok = true;
        for (i, iv) in intervals.iter().enumerate() {
            if iv.0 != t.id {
                continue;
            }
            let prev = i.checked_sub(1).map(|p| intervals[p].0);
            let next = intervals.get(i + 1).map(|n| n.0);
            match (prev, next) {
                (Some(p), Some(n)) if p == n && p != t.id && around.is_none_or(|a| a == p) => {
                    around = Some(p);
                }
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        // The surrounding task must be a keeper, not another orphan.
        if let (true, Some(into)) = (ok, around)
            && !tasks.iter().any(|o| o.id == into && is_orphan(o))
        {
            out.push((t.id, into));
        }
    }
    out
}

/// The model's input: today's derived tasks with evidence, then the locked
/// ones for reference.
pub fn render_input(tasks: &[DayTask]) -> String {
    let mut out = String::new();
    let line = |out: &mut String, t: &DayTask| {
        let _ = write!(out, "- id {}: \"{}\"", t.id, t.label);
        if let Some(p) = &t.project {
            let _ = write!(out, " [{p}]");
        }
        if let Some(k) = &t.external_ref {
            let _ = write!(out, " ({k})");
        }
        let _ = writeln!(
            out,
            " {}m, {} interval{}, {} batch{}",
            t.total_ms / 60_000,
            t.intervals,
            if t.intervals == 1 { "" } else { "s" },
            t.batches,
            if t.batches == 1 { "" } else { "es" }
        );
        for (app, title, ms) in t.evidence.iter().take(3) {
            let _ = writeln!(out, "    {}m {app}: {title}", (*ms + 30_000) / 60_000);
        }
    };
    let _ = writeln!(out, "## Derived today");
    for t in tasks.iter().filter(|t| !t.locked) {
        line(&mut out, t);
    }
    let locked: Vec<&DayTask> = tasks.iter().filter(|t| t.locked).collect();
    if !locked.is_empty() {
        let _ = writeln!(out, "\n## Locked (named by the user)");
        for t in locked {
            line(&mut out, t);
        }
    }
    out
}

/// Apply the guards to the model's answer. Merges: both sides known and
/// unlocked, distinct, no two different projects or ticket keys, no task
/// folded twice, at most `max_merges`. Renames: known, unlocked, from one
/// batch, never user-renamed, a non-empty new wording.
pub fn guard(tasks: &[DayTask], model: &ModelPlan, max_merges: usize) -> Plan {
    let task = |id: i64| tasks.iter().find(|t| t.id == id);
    let mut plan = Plan::default();
    let mut folded: BTreeSet<i64> = BTreeSet::new();
    let mut targets: BTreeSet<i64> = BTreeSet::new();
    for m in &model.merges {
        let Some(into) = task(m.into) else { continue };
        if into.locked || folded.contains(&into.id) {
            continue;
        }
        for &from_id in &m.from {
            if plan.merges.len() >= max_merges {
                break;
            }
            let Some(from) = task(from_id) else { continue };
            // A merge target never becomes a source later in the plan.
            if from.locked
                || from.id == into.id
                || folded.contains(&from.id)
                || targets.contains(&from.id)
            {
                continue;
            }
            let differs = |a: &Option<String>, b: &Option<String>| matches!((a, b), (Some(x), Some(y)) if !x.eq_ignore_ascii_case(y));
            if differs(&from.project, &into.project)
                || differs(&from.external_ref, &into.external_ref)
            {
                continue;
            }
            folded.insert(from.id);
            targets.insert(into.id);
            plan.merges.push((from.id, into.id));
        }
    }
    for r in &model.renames {
        let Some(t) = task(r.id) else { continue };
        let label = r.label.trim();
        if t.locked
            || folded.contains(&t.id)
            || t.batches != 1
            || t.user_renamed
            || label.is_empty()
            || label.eq_ignore_ascii_case(&t.label)
        {
            continue;
        }
        plan.renames.push((t.id, label.to_owned()));
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: i64, label: &str, project: Option<&str>, key: Option<&str>, locked: bool) -> DayTask {
        DayTask {
            id,
            label: label.into(),
            project: project.map(str::to_owned),
            external_ref: key.map(str::to_owned),
            locked,
            total_ms: 10 * 60_000,
            intervals: 1,
            batches: 1,
            has_notes: false,
            user_renamed: false,
            evidence: vec![],
        }
    }

    #[test]
    fn guards_reject_locked_cross_project_and_double_folds() {
        let mut tasks = vec![
            t(1, "sms fix", Some("mailer"), Some("ACME-1"), false),
            t(2, "sms campaign failures", Some("mailer"), None, false),
            t(3, "chronicle feed", Some("chronicle"), None, false),
            t(4, "Complete m25", None, None, true),
            t(5, "wordle", None, None, false),
            t(6, "other ticket", Some("mailer"), Some("ACME-2"), false),
        ];
        tasks[2].batches = 2;
        tasks[4].user_renamed = true;
        let model = ModelPlan {
            merges: vec![
                ModelMerge {
                    into: 1,
                    from: vec![2, 3, 4, 6, 1, 99],
                },
                ModelMerge {
                    into: 4,
                    from: vec![5],
                },
                ModelMerge {
                    into: 3,
                    from: vec![2],
                },
            ],
            renames: vec![
                ModelRename {
                    id: 3,
                    label: "chronicle feed redesign".into(),
                },
                ModelRename {
                    id: 5,
                    label: "playing wordle".into(),
                },
                ModelRename {
                    id: 2,
                    label: "x".into(),
                },
                ModelRename {
                    id: 4,
                    label: "nope".into(),
                },
                ModelRename {
                    id: 6,
                    label: "  ".into(),
                },
                ModelRename {
                    id: 6,
                    label: "OTHER TICKET".into(),
                },
                ModelRename {
                    id: 6,
                    label: "investigating ACME-2".into(),
                },
            ],
        };
        let plan = guard(&tasks, &model, MAX_MERGES);
        // 2 → 1 only: 3 is another project, 4 locked, 6 another key, 1 is
        // itself, 99 unknown; 4 locked as a target; 2 already folded.
        assert_eq!(plan.merges, vec![(2, 1)]);
        // A target earlier in the plan cannot be folded later.
        let chain = guard(
            &tasks,
            &ModelPlan {
                merges: vec![
                    ModelMerge {
                        into: 1,
                        from: vec![2],
                    },
                    ModelMerge {
                        into: 6,
                        from: vec![1],
                    },
                ],
                renames: vec![],
            },
            MAX_MERGES,
        );
        assert_eq!(chain.merges, vec![(2, 1)]);
        // 3 spans two batches, 5 was user-renamed, 2 was folded, 4 locked,
        // blank and same-wording renames dropped.
        assert_eq!(plan.renames, vec![(6, "investigating ACME-2".to_owned())]);
        let capped = guard(
            &tasks,
            &ModelPlan {
                merges: vec![
                    ModelMerge {
                        into: 1,
                        from: vec![2],
                    },
                    ModelMerge {
                        into: 6,
                        from: vec![],
                    },
                ],
                renames: vec![],
            },
            0,
        );
        assert!(capped.merges.is_empty());
    }

    #[test]
    fn orphans_fold_only_when_surrounded_by_one_task() {
        let mut tasks = vec![
            t(1, "real work", Some("mailer"), None, false),
            t(2, "wordle", None, None, false),
            t(3, "f1 stats", None, None, false),
            t(4, "note", None, None, false),
        ];
        for id in [2, 3, 4] {
            tasks[id - 1].total_ms = 60_000;
        }
        tasks[3].has_notes = true;
        let m = 60_000;
        let intervals = vec![
            (1, 0, 10 * m),
            (2, 10 * m, 11 * m),
            (1, 11 * m, 20 * m),
            (3, 20 * m, 21 * m),
            (4, 21 * m, 22 * m),
            (1, 22 * m, 30 * m),
        ];
        assert_eq!(orphan_folds(&tasks, &intervals), vec![(2, 1)]);
        // Two adjacent orphans around a third: nothing folds into an orphan.
        let pair = vec![(2, 0, m), (3, m, 2 * m), (2, 2 * m, 3 * m)];
        assert!(orphan_folds(&tasks, &pair).is_empty());
        let input = render_input(&tasks);
        assert!(
            input.starts_with(
                "## Derived today\n- id 1: \"real work\" [mailer] 10m, 1 interval, 1 batch\n"
            ),
            "{input}"
        );
        assert!(!input.contains("## Locked"));
    }
}
