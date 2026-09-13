//! Deterministic scoring of model task JSON against fixture expectations
//! (`fixtures/<name>.expect.json`). Encodes the real derivation failures the
//! task-identity work must fix; bench prints the report per fixture case.

use serde::Deserialize;

use crate::merge::{LinkedInterval, near_identical};
use crate::types::{OpenTask, TaskDraft, TaskSlot};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectations {
    /// Open tasks injected into the fixture digest (numbered list the model
    /// can ref), simulating declared or carried-over identities.
    #[serde(default)]
    pub open_tasks: Vec<ExpectOpenTask>,
    /// Sets of minute ranges that must resolve to one task identity.
    #[serde(default)]
    pub groups: Vec<GroupExpect>,
    /// Tokens that must not appear in any label (cross-context title bleed).
    #[serde(default)]
    pub forbid_label_tokens: Vec<String>,
    /// Cap on distinct task identities (over-fragmentation guard).
    #[serde(default)]
    pub max_tasks: Option<usize>,
    /// Cap on intervals after coalescing (one-row-per-Timeline-line guard).
    #[serde(default)]
    pub max_intervals: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectOpenTask {
    pub label: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub declared: bool,
}

impl Expectations {
    /// The injected open-task list as digest/linking input (ids are 1-based
    /// positions; fixtures have no real task rows).
    pub fn open_task_list(&self) -> Vec<OpenTask> {
        self.open_tasks
            .iter()
            .enumerate()
            .map(|(i, t)| OpenTask {
                id: (i + 1) as i64,
                label: t.label.clone(),
                project: t.project.clone(),
                declared: t.declared,
            })
            .collect()
    }
}

/// Flatten linked output into resolved rows for printing + scoring: each
/// interval carries its task identity's label/project.
pub fn resolve(
    slots: &[TaskSlot],
    intervals: &[LinkedInterval],
    open: &[OpenTask],
) -> Vec<TaskDraft> {
    intervals
        .iter()
        .filter_map(|iv| {
            let (label, project) = match slots.get(iv.slot)? {
                TaskSlot::Existing(id) => {
                    let t = open.iter().find(|t| t.id == *id)?;
                    (t.label.clone(), t.project.clone())
                }
                TaskSlot::New { label, project } => (label.clone(), project.clone()),
            };
            Some(TaskDraft {
                label,
                project,
                start_offset_min: iv.start_offset_min,
                end_offset_min: iv.end_offset_min,
                confidence: iv.confidence,
            })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupExpect {
    pub name: String,
    /// Minute offsets from window start, end-exclusive.
    pub ranges: Vec<(i64, i64)>,
    #[serde(default)]
    pub project: Option<String>,
    /// Dominant labels must contain at least one of these (case-insensitive).
    #[serde(default)]
    pub label_any: Vec<String>,
}

pub struct Check {
    pub name: String,
    pub pass: bool,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.pass)
    }
    pub fn summary(&self) -> String {
        let ok = self.checks.iter().filter(|c| c.pass).count();
        format!("{ok}/{} checks pass", self.checks.len())
    }
}

fn ident(t: &TaskDraft) -> (String, Option<String>) {
    (
        t.label.trim().to_lowercase(),
        t.project.as_deref().map(|p| p.trim().to_lowercase()),
    )
}

/// Task with the largest overlap with `range`; ties go to the earlier draft.
fn dominant(tasks: &[TaskDraft], range: (i64, i64)) -> Option<&TaskDraft> {
    tasks
        .iter()
        .map(|t| {
            (
                range.1.min(t.end_offset_min) - range.0.max(t.start_offset_min),
                t,
            )
        })
        .filter(|(overlap, _)| *overlap > 0)
        .max_by_key(|(overlap, _)| *overlap)
        .map(|(_, t)| t)
}

pub fn score(tasks: &[TaskDraft], exp: &Expectations) -> Report {
    let mut checks = Vec::new();
    let mut check = |name: String, pass: bool, detail: String| {
        checks.push(Check { name, pass, detail });
    };

    for g in &exp.groups {
        let doms: Vec<Option<&TaskDraft>> = g.ranges.iter().map(|&r| dominant(tasks, r)).collect();
        let idents: Vec<_> = doms.iter().flatten().map(|t| ident(t)).collect();
        let grouped = idents.len() == g.ranges.len() && idents.windows(2).all(|w| w[0] == w[1]);
        check(
            format!("group:{}", g.name),
            grouped,
            if grouped {
                format!("all {} ranges on one task", g.ranges.len())
            } else {
                format!(
                    "ranges resolve to {:?}",
                    doms.iter()
                        .map(|d| d.map_or("<uncovered>", |t| t.label.as_str()))
                        .collect::<Vec<_>>()
                )
            },
        );
        if let Some(want) = &g.project {
            let projects: Vec<_> = doms
                .iter()
                .flatten()
                .map(|t| t.project.as_deref().unwrap_or("none"))
                .collect();
            let pass = !doms.is_empty()
                && doms.iter().all(|d| {
                    d.is_some_and(|t| {
                        t.project
                            .as_deref()
                            .is_some_and(|p| p.eq_ignore_ascii_case(want))
                    })
                });
            check(
                format!("project:{}", g.name),
                pass,
                format!("want {want}, got {projects:?}"),
            );
        }
        if !g.label_any.is_empty() {
            let pass = !doms.is_empty()
                && doms.iter().all(|d| {
                    d.is_some_and(|t| {
                        let label = t.label.to_lowercase();
                        g.label_any
                            .iter()
                            .any(|tok| label.contains(&tok.to_lowercase()))
                    })
                });
            check(
                format!("label:{}", g.name),
                pass,
                format!("want one of {:?} in every dominant label", g.label_any),
            );
        }
    }

    // Groups are distinct expected tasks: two cohesive groups landing on one
    // identity = over-linking (everything dumped onto a declared task).
    for (gi, a) in exp.groups.iter().enumerate() {
        let ident_of = |g: &GroupExpect| {
            let idents: Vec<_> = g
                .ranges
                .iter()
                .filter_map(|&r| dominant(tasks, r))
                .map(ident)
                .collect();
            (idents.len() == g.ranges.len() && idents.windows(2).all(|w| w[0] == w[1]))
                .then(|| idents.into_iter().next())
                .flatten()
        };
        let ia = ident_of(a);
        for b in &exp.groups[gi + 1..] {
            if let (Some(ia), Some(ib)) = (ia.clone(), ident_of(b)) {
                check(
                    format!("distinct:{}|{}", a.name, b.name),
                    ia != ib,
                    if ia == ib {
                        format!("both resolve to {:?}", ia.0)
                    } else {
                        "separate identities".into()
                    },
                );
            }
        }
    }

    if !exp.forbid_label_tokens.is_empty() {
        let offenders: Vec<String> = tasks
            .iter()
            .filter(|t| {
                let label = t.label.to_lowercase();
                exp.forbid_label_tokens
                    .iter()
                    .any(|tok| label.contains(&tok.to_lowercase()))
            })
            .map(|t| t.label.clone())
            .collect();
        check(
            "hygiene".into(),
            offenders.is_empty(),
            if offenders.is_empty() {
                "no forbidden tokens in labels".into()
            } else {
                format!("forbidden tokens in {offenders:?}")
            },
        );
    }

    // Distinct identities that are near-identical = an unmerged duplicate.
    let mut idents: Vec<(String, Option<String>)> = tasks.iter().map(ident).collect();
    idents.sort();
    idents.dedup();
    let dups: Vec<String> = idents
        .iter()
        .enumerate()
        .flat_map(|(i, a)| idents[i + 1..].iter().map(move |b| (a, b)))
        .filter(|(a, b)| near_identical(&a.0, &b.0))
        .map(|(a, b)| format!("{:?} ~ {:?}", a.0, b.0))
        .collect();
    check(
        "dups".into(),
        dups.is_empty(),
        if dups.is_empty() {
            "no near-identical identities".into()
        } else {
            dups.join("; ")
        },
    );

    if let Some(max) = exp.max_tasks {
        check(
            "fragmentation".into(),
            idents.len() <= max,
            format!("{} identities, cap {max}", idents.len()),
        );
    }
    if let Some(max) = exp.max_intervals {
        check(
            "intervals".into(),
            tasks.len() <= max,
            format!("{} intervals, cap {max}", tasks.len()),
        );
    }

    Report { checks }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(label: &str, project: Option<&str>, start: i64, end: i64) -> TaskDraft {
        TaskDraft {
            label: label.into(),
            project: project.map(str::to_owned),
            start_offset_min: start,
            end_offset_min: end,
            confidence: 0.9,
        }
    }

    fn sms_expectations() -> Expectations {
        serde_json::from_str(
            r#"{
              "groups": [{
                "name": "sms",
                "ranges": [[6, 13], [19, 23], [23, 26]],
                "project": "mailer",
                "label_any": ["sms"]
              }],
              "forbid_label_tokens": ["M7", "northwind"],
              "max_tasks": 4
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn morning_v2_output_fails() {
        // The real batch-8 derivation, verbatim: fragmented SMS identity,
        // Heroku title bleed, M7 token bleed.
        let tasks = [
            draft(
                "verifying mailer retry backoff in email system",
                Some("mailer"),
                0,
                5,
            ),
            draft(
                "investigating SMS campaign sending failures in email system",
                Some("mailer"),
                6,
                13,
            ),
            draft(
                "debugging and testing local SMS send script in email system",
                Some("mailer"),
                19,
                23,
            ),
            draft(
                "monitoring and adjusting Heroku app settings for northwind-memberships",
                Some("northwind-memberships"),
                23,
                26,
            ),
            draft(
                "reviewing and refining digest v2 chronological span for task grouping",
                None,
                26,
                30,
            ),
        ];
        let report = score(&tasks, &sms_expectations());
        let get = |name: &str| {
            report
                .checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("missing check {name}"))
        };
        assert!(!get("group:sms").pass);
        assert!(!get("project:sms").pass);
        assert!(!get("label:sms").pass);
        assert!(!get("hygiene").pass);
        assert!(!get("fragmentation").pass);
    }

    #[test]
    fn fixed_grouping_passes() {
        let tasks = [
            draft(
                "bringing up the email-server staging environment",
                Some("mailer"),
                0,
                5,
            ),
            draft(
                "fixing SMS campaign sending failures",
                Some("mailer"),
                6,
                13,
            ),
            draft(
                "fixing SMS campaign sending failures",
                Some("mailer"),
                19,
                26,
            ),
            draft(
                "reviewing chronicle task grouping",
                Some("chronicle"),
                26,
                30,
            ),
        ];
        let report = score(&tasks, &sms_expectations());
        assert!(report.passed(), "{}", report.summary());
    }

    #[test]
    fn uncovered_range_fails_grouping() {
        let tasks = [draft("fixing SMS sending", Some("mailer"), 6, 13)];
        let report = score(&tasks, &sms_expectations());
        assert!(
            !report
                .checks
                .iter()
                .find(|c| c.name == "group:sms")
                .unwrap()
                .pass
        );
    }
}
