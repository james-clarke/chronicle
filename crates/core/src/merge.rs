//! Deterministic linking + post-merge of model interval drafts
//! (model-independent dedup). Refs link intervals to open tasks by list
//! index; null-ref proposals near-matching an open task attach to it instead
//! of minting a drifted duplicate; the rest collapse among themselves when
//! near-identical with the same project. Time intervals are never coalesced —
//! AFK splits stay time-honest (M5 decision).

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

#[cfg(test)]
mod tests {
    use super::*;

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
