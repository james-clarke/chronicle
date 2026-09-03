//! Deterministic linking + post-merge of model interval drafts
//! (model-independent dedup). Refs link intervals to open tasks by list
//! index; null-ref proposals near-matching an open task attach to it instead
//! of minting a drifted duplicate; the rest collapse among themselves when
//! near-identical with the same project. `coalesce` then repairs overlaps
//! and joins adjacent same-slot pieces; it never bridges an AFK gap of 5 min
//! or more, so AFK splits stay time-honest (M5 decision).

use crate::types::{IntervalDraft, OpenTask, TaskSlot};

/// Conservative on purpose: only near-verbatim labels merge. Genuinely
/// fragmented identities ("investigating X" vs "debugging Y") are the
/// ref-linking layer's job, not string similarity's.
const NEAR_DUP: f64 = 0.9;

pub fn near_identical(a: &str, b: &str) -> bool {
    let (a, b) = (norm(a), norm(b));
    a == b || strsim::normalized_levenshtein(&a, &b) >= NEAR_DUP
}

fn norm(s: &str) -> String {
    s.trim().to_lowercase()
}

fn same_project(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => norm(a) == norm(b),
        (None, None) => true,
        _ => false,
    }
}

/// Repair untrusted model output: refs outside 1..=open_count go null; a
/// null-ref interval with no (or blank) label is dropped; a ref'd interval's
/// label is discarded (the task already has a name).
pub fn sanitize_intervals(raw: Vec<IntervalDraft>, open_count: usize) -> Vec<IntervalDraft> {
    raw.into_iter()
        .filter_map(|mut d| {
            if let Some(r) = d.task_ref
                && !(1..=open_count as i64).contains(&r)
            {
                d.task_ref = None;
            }
            match d.task_ref {
                Some(_) => {
                    d.label = None;
                    Some(d)
                }
                None => {
                    d.label = d
                        .label
                        .map(|l| l.trim().to_owned())
                        .filter(|l| !l.is_empty());
                    d.label.is_some().then_some(d)
                }
            }
        })
        .collect()
}

/// An interval resolved to a task slot; offsets still in digest minutes.
#[derive(Debug, Clone)]
pub struct LinkedInterval {
    pub slot: usize,
    pub start_offset_min: i64,
    pub end_offset_min: i64,
    pub confidence: f64,
}

/// Resolve sanitized drafts to task slots. Refs bind to the open task at
/// their index; null-ref proposals attach to an open task with a
/// near-identical label, else collapse onto an earlier near-identical
/// same-project proposal, else open a new slot (earliest proposal is
/// canonical).
pub fn link_intervals(
    drafts: &[IntervalDraft],
    open: &[OpenTask],
) -> (Vec<TaskSlot>, Vec<LinkedInterval>) {
    let mut slots: Vec<TaskSlot> = Vec::new();
    let mut intervals: Vec<LinkedInterval> = Vec::new();
    let mut order: Vec<usize> = (0..drafts.len()).collect();
    order.sort_by_key(|&i| (drafts[i].start_offset_min, i));

    let slot_for_task = |slots: &mut Vec<TaskSlot>, id: i64| -> usize {
        match slots
            .iter()
            .position(|s| matches!(s, TaskSlot::Existing(t) if *t == id))
        {
            Some(i) => i,
            None => {
                slots.push(TaskSlot::Existing(id));
                slots.len() - 1
            }
        }
    };

    for i in order {
        let d = &drafts[i];
        let slot = match d.task_ref {
            Some(r) => slot_for_task(&mut slots, open[(r - 1) as usize].id),
            None => {
                let label = d.label.as_deref().unwrap_or_default();
                if let Some(t) = open.iter().find(|t| near_identical(&t.label, label)) {
                    slot_for_task(&mut slots, t.id)
                } else if let Some(j) = slots.iter().position(|s| {
                    matches!(s, TaskSlot::New { label: l, project: p }
                        if near_identical(l, label)
                            && same_project(p.as_deref(), d.project.as_deref()))
                }) {
                    j
                } else {
                    slots.push(TaskSlot::New {
                        label: label.to_owned(),
                        project: d.project.clone(),
                    });
                    slots.len() - 1
                }
            }
        };
        intervals.push(LinkedInterval {
            slot,
            start_offset_min: d.start_offset_min,
            end_offset_min: d.end_offset_min,
            confidence: d.confidence,
        });
    }
    (slots, intervals)
}

/// Overlap repair, then adjacency merge, in whatever unit the caller's
/// offsets and `afk_gaps` use (digest minutes in the worker, ms in the
/// backfill). Sorted by start; an interval starting inside the previous one
/// is trimmed to its end, one ending inside it is dropped. Then an interval
/// joins the previous one when both share a slot, the unlabelled gap between
/// them is under `max_gap`, and no AFK gap touches that space. Both models
/// emit one interval per Timeline line, so without this a 30-minute stretch
/// of one goal lands as eight one-minute rows. Confidence is the
/// duration-weighted mean of the pieces.
pub fn coalesce(
    mut linked: Vec<LinkedInterval>,
    afk_gaps: &[(i64, i64)],
    max_gap: i64,
) -> Vec<LinkedInterval> {
    linked.retain(|iv| iv.end_offset_min > iv.start_offset_min);
    linked.sort_by(|a, b| {
        a.start_offset_min
            .cmp(&b.start_offset_min)
            .then(b.confidence.total_cmp(&a.confidence))
    });
    let mut out: Vec<LinkedInterval> = Vec::with_capacity(linked.len());
    for mut iv in linked {
        if let Some(prev) = out.last_mut() {
            if iv.start_offset_min < prev.end_offset_min {
                if iv.end_offset_min <= prev.end_offset_min {
                    continue;
                }
                iv.start_offset_min = prev.end_offset_min;
            }
            let gap = iv.start_offset_min - prev.end_offset_min;
            let afk_between = afk_gaps
                .iter()
                .any(|&(s, e)| s < iv.start_offset_min && e > prev.end_offset_min);
            if iv.slot == prev.slot && gap < max_gap && !afk_between {
                let a = prev.end_offset_min - prev.start_offset_min;
                let b = iv.end_offset_min - iv.start_offset_min;
                prev.confidence =
                    (prev.confidence * a as f64 + iv.confidence * b as f64) / (a + b) as f64;
                prev.end_offset_min = iv.end_offset_min;
                continue;
            }
        }
        out.push(iv);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked(slot: usize, start: i64, end: i64, confidence: f64) -> LinkedInterval {
        LinkedInterval {
            slot,
            start_offset_min: start,
            end_offset_min: end,
            confidence,
        }
    }

    #[test]
    fn coalesce_joins_adjacent_same_slot_pieces() {
        // Batch 67 with 4B: eight consecutive one-minute intervals, one label.
        let pieces: Vec<_> = (0..8).map(|m| linked(0, m, m + 1, 0.9)).collect();
        let out = coalesce(pieces, &[], 2);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start_offset_min, out[0].end_offset_min), (0, 8));
        assert!((out[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn coalesce_keeps_slot_changes_and_weights_confidence() {
        let out = coalesce(
            vec![
                linked(0, 0, 6, 1.0),
                linked(0, 7, 9, 0.4), // 1-minute unlabelled gap: joins
                linked(1, 9, 12, 0.8),
                linked(0, 12, 15, 0.6), // slot 1 lies between: stays apart
            ],
            &[],
            2,
        );
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].start_offset_min, out[0].end_offset_min), (0, 9));
        assert!((out[0].confidence - (6.0 * 1.0 + 2.0 * 0.4) / 8.0).abs() < 1e-9);
        assert_eq!(out[1].slot, 1);
        assert_eq!(out[2].slot, 0);
    }

    #[test]
    fn coalesce_never_bridges_afk() {
        let out = coalesce(
            vec![linked(0, 0, 10, 0.9), linked(0, 11, 20, 0.9)],
            &[(10, 11)],
            2,
        );
        assert_eq!(out.len(), 2);
        // Wide unlabelled gap without AFK also stays apart.
        let out = coalesce(vec![linked(0, 0, 10, 0.9), linked(0, 13, 20, 0.9)], &[], 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn coalesce_repairs_overlaps() {
        // Batch 68 with 4B: 0–28 and 13–15 as separate intervals.
        let out = coalesce(
            vec![
                linked(0, 0, 28, 0.9),
                linked(1, 13, 15, 0.7), // nested: dropped
                linked(1, 25, 30, 0.8), // overlapping: trimmed to 28–30
                linked(2, 5, 0, 0.5),   // inverted: dropped
            ],
            &[],
            2,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            (out[1].slot, out[1].start_offset_min, out[1].end_offset_min),
            (1, 28, 30)
        );
    }

    fn draft(
        task_ref: Option<i64>,
        label: Option<&str>,
        project: Option<&str>,
        start: i64,
    ) -> IntervalDraft {
        IntervalDraft {
            task_ref,
            label: label.map(str::to_owned),
            project: project.map(str::to_owned),
            start_offset_min: start,
            end_offset_min: start + 5,
            confidence: 0.9,
        }
    }

    fn open(id: i64, label: &str, project: Option<&str>, declared: bool) -> OpenTask {
        OpenTask {
            id,
            label: label.into(),
            project: project.map(str::to_owned),
            declared,
        }
    }

    #[test]
    fn sanitize_repairs_untrusted_output() {
        let out = sanitize_intervals(
            vec![
                draft(Some(3), Some("ignored"), None, 0), // valid ref: label dropped
                draft(Some(9), Some("kept"), None, 5),    // ref out of range: proposal
                draft(None, Some("  "), None, 10),        // blank label: dropped
                draft(None, None, None, 15),              // no label: dropped
            ],
            3,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].task_ref, Some(3));
        assert_eq!(out[0].label, None);
        assert_eq!(out[1].task_ref, None);
        assert_eq!(out[1].label.as_deref(), Some("kept"));
    }

    #[test]
    fn refs_and_snaps_share_one_slot() {
        // The morning duplicate: batch 8 re-proposes batch 7's open task in
        // slightly different case; both a ref and a near-identical proposal
        // must land on the same existing identity.
        let open = [open(
            41,
            "verifying M7 human-verified progress in email system",
            Some("chronicle"),
            false,
        )];
        let (slots, intervals) = link_intervals(
            &[
                draft(Some(1), None, None, 0),
                draft(
                    None,
                    Some("Verifying M7 human-verified progress in email system"),
                    Some("mailer"),
                    10,
                ),
            ],
            &open,
        );
        assert_eq!(slots.len(), 1);
        assert!(matches!(slots[0], TaskSlot::Existing(41)));
        assert_eq!(intervals[0].slot, 0);
        assert_eq!(intervals[1].slot, 0);
    }

    #[test]
    fn near_dup_proposals_collapse_to_earliest() {
        let (slots, intervals) = link_intervals(
            &[
                draft(
                    None,
                    Some("debugging SMS send script"),
                    Some("mailer"),
                    10,
                ),
                draft(
                    None,
                    Some("debugging sms send script."),
                    Some("mailer"),
                    0,
                ),
                draft(
                    None,
                    Some("reviewing digest grouping"),
                    Some("chronicle"),
                    20,
                ),
            ],
            &[],
        );
        assert_eq!(slots.len(), 2);
        // Earliest (offset 0) is canonical for the shared slot.
        assert!(
            matches!(&slots[0], TaskSlot::New { label, .. } if label == "debugging sms send script.")
        );
        let sms_slots: Vec<usize> = intervals
            .iter()
            .filter(|iv| iv.start_offset_min <= 10)
            .map(|iv| iv.slot)
            .collect();
        assert_eq!(sms_slots, [0, 0]);
    }

    #[test]
    fn different_project_never_merges() {
        let (slots, _) = link_intervals(
            &[
                draft(None, Some("fixing login bug"), Some("app-a"), 0),
                draft(None, Some("fixing login bug"), Some("app-b"), 10),
            ],
            &[],
        );
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn distinct_labels_stay_separate() {
        // Real morning fragmentation: below the conservative threshold on
        // purpose — bridging these is the model's job via refs, not strsim's.
        let (slots, _) = link_intervals(
            &[
                draft(
                    None,
                    Some("investigating SMS campaign sending failures in email system"),
                    Some("mailer"),
                    6,
                ),
                draft(
                    None,
                    Some("debugging and testing local SMS send script in email system"),
                    Some("mailer"),
                    19,
                ),
            ],
            &[],
        );
        assert_eq!(slots.len(), 2);
    }
}
