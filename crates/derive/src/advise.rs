//! The pairwise advisor (m36 chunk 3): a low-margin verdict between two
//! open tasks goes to a model with the segment's evidence rows (by id),
//! both tasks' profiles and the nearest corrections; the answer is A, B,
//! new or unsure with the ids that decided it. Nothing here touches the
//! database: the app renders the rows and applies the decision, the bench
//! replays it.

use chronicle_core::profile::{AnchoredSpan, Profile};

use crate::prompts::ADVISE_PROMPT;

/// One evidence row the model can cite: a focus span under the segment.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceRow {
    pub id: i64,
    pub app: String,
    pub title: String,
    pub minutes: f64,
    /// `kind:value` anchors the span carries.
    pub anchors: Vec<String>,
}

/// A candidate task as the prompt shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub task_id: i64,
    pub label: String,
    pub project: Option<String>,
    /// The profile's strongest keys, "kind value (Nm)".
    pub profile: Vec<String>,
}

/// The rows of a segment: focus spans overlapping `lo..hi`, in time order,
/// clipped to the overlap. Rows under a minute are dropped unless the
/// segment has nothing longer.
pub fn evidence_rows(spans: &[AnchoredSpan], lo: i64, hi: i64) -> Vec<EvidenceRow> {
    let mut rows: Vec<EvidenceRow> = spans
        .iter()
        .filter(|s| s.end_ts > lo && s.start_ts < hi && !s.title.is_empty())
        .map(|s| EvidenceRow {
            id: s.id,
            app: s.app.clone(),
            title: s.title.chars().take(120).collect(),
            minutes: (s.end_ts.min(hi) - s.start_ts.max(lo)) as f64 / 60_000.0,
            anchors: s
                .anchors
                .iter()
                .map(|a| format!("{}:{}", a.kind.as_str(), a.value))
                .collect(),
        })
        .collect();
    if rows.iter().any(|r| r.minutes >= 1.0) {
        rows.retain(|r| r.minutes >= 1.0);
    }
    rows.truncate(40);
    rows
}

/// A task's profile as candidate lines: up to `n` keys by decayed minutes.
pub fn candidate(
    task_id: i64,
    label: &str,
    project: Option<&str>,
    profile: Option<&Profile>,
    n: usize,
) -> Candidate {
    let mut keys: Vec<(String, f64)> = profile
        .map(|p| {
            p.minutes
                .iter()
                .map(|(k, m)| (format!("{} {}", k.kind_str(), k.value()), *m))
                .collect()
        })
        .unwrap_or_default();
    keys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    Candidate {
        task_id,
        label: label.to_owned(),
        project: project.map(str::to_owned),
        profile: keys
            .into_iter()
            .take(n)
            .map(|(k, m)| format!("{k} ({m:.0}m)"))
            .collect(),
    }
}

fn render_candidate(c: &Candidate) -> String {
    let mut out = format!("\"{}\"", c.label);
    if let Some(p) = &c.project {
        out.push_str(&format!(" [{p}]"));
    }
    out.push('\n');
    if c.profile.is_empty() {
        out.push_str("(no evidence profile yet)\n");
    }
    for line in &c.profile {
        out.push_str("- ");
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// The rendered prompt. `examples` is `examples::render_section` output
/// (empty when there is none).
pub fn render(evidence: &[EvidenceRow], a: &Candidate, b: &Candidate, examples: &str) -> String {
    let rows: String = evidence
        .iter()
        .map(|r| {
            let mut line = format!(
                "- id {}: {}m {}: {}",
                r.id,
                r.minutes.round() as i64,
                r.app,
                r.title
            );
            if !r.anchors.is_empty() {
                line.push_str(&format!(" [{}]", r.anchors.join(", ")));
            }
            line.push('\n');
            line
        })
        .collect();
    ADVISE_PROMPT
        .replace("{evidence}", rows.trim_end())
        .replace("{a}", &render_candidate(a))
        .replace("{b}", &render_candidate(b))
        .replace("{examples}", examples)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    A,
    B,
    New,
    Unsure,
}

impl Answer {
    pub fn as_str(self) -> &'static str {
        match self {
            Answer::A => "A",
            Answer::B => "B",
            Answer::New => "new",
            Answer::Unsure => "unsure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    pub answer: Answer,
    pub evidence_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
struct AdviceOut {
    answer: String,
    evidence_ids: Vec<i64>,
}

pub fn parse(out: &str) -> anyhow::Result<Advice> {
    let parsed: AdviceOut = serde_json::from_str(out)
        .map_err(|e| anyhow::anyhow!("model output is not valid advice JSON: {e}: {out}"))?;
    let answer = match parsed.answer.as_str() {
        "A" => Answer::A,
        "B" => Answer::B,
        "new" => Answer::New,
        "unsure" => Answer::Unsure,
        other => anyhow::bail!("advice answer {other:?} is not A, B, new or unsure"),
    };
    Ok(Advice {
        answer,
        evidence_ids: parsed.evidence_ids,
    })
}

/// What the pipeline does with an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// A: the placement stands, now backed by the advisor.
    Keep,
    /// B: the interval moves to the runner-up.
    Move,
    /// new: mint a task as the scorer would have.
    Mint,
    /// unsure: leave it "to confirm".
    Unsure,
    /// A, B or new with no cited row, or a cited id outside the segment.
    Invalid,
}

/// Re-rank only when every cited id is a row of the segment and at least
/// one was cited; "unsure" needs no citation.
pub fn decide(advice: &Advice, segment_ids: &[i64]) -> Decision {
    if advice.answer == Answer::Unsure {
        return Decision::Unsure;
    }
    if advice.evidence_ids.is_empty()
        || advice
            .evidence_ids
            .iter()
            .any(|id| !segment_ids.contains(id))
    {
        return Decision::Invalid;
    }
    match advice.answer {
        Answer::A => Decision::Keep,
        Answer::B => Decision::Move,
        Answer::New => Decision::Mint,
        Answer::Unsure => Decision::Unsure,
    }
}

/// The reason string an advised interval carries.
pub fn reason(advice: &Advice) -> String {
    format!("advisor: {}", advice.answer.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chronicle_core::extract::{Anchor, AnchorKind};

    fn span(id: i64, lo_min: i64, hi_min: i64, app: &str, title: &str) -> AnchoredSpan {
        AnchoredSpan {
            id,
            start_ts: lo_min * 60_000,
            end_ts: hi_min * 60_000,
            app: app.into(),
            title: title.into(),
            anchors: vec![Anchor {
                kind: AnchorKind::Place,
                value: "shop".into(),
            }],
            vec: None,
            quiet_ms: 0,
            wrote: false,
            project: None,
        }
    }

    #[test]
    fn rows_clip_to_the_segment_and_render_with_ids() {
        let spans = vec![
            span(1, 0, 5, "Code", "cart.py — shop"),
            span(2, 5, 30, "Firefox", "Stripe API reference"),
            span(3, 29, 60, "Terminal", "pytest"),
        ];
        let rows = evidence_rows(&spans, 4 * 60_000, 30 * 60_000);
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(rows[0].minutes, 1.0);
        assert_eq!(rows[1].minutes, 25.0);
        assert_eq!(rows[2].minutes, 1.0);
        let a = Candidate {
            task_id: 7,
            label: "fixing checkout".into(),
            project: Some("shop".into()),
            profile: vec!["place shop (40m)".into()],
        };
        let b = Candidate {
            task_id: 8,
            label: "blog redesign".into(),
            project: None,
            profile: vec![],
        };
        let p = render(&rows, &a, &b, "");
        assert!(p.contains("- id 2: 25m Firefox: Stripe API reference [place:shop]"));
        assert!(p.contains("## Task A: \"fixing checkout\" [shop]\n- place shop (40m)"));
        assert!(p.contains("## Task B: \"blog redesign\"\n(no evidence profile yet)"));
        assert!(p.starts_with("/no_think\n"));
    }

    #[test]
    fn answers_parse_and_decide() {
        let adv = parse(r#"{"answer":"B","evidence_ids":[2,3]}"#).unwrap();
        assert_eq!(decide(&adv, &[1, 2, 3]), Decision::Move);
        assert_eq!(decide(&adv, &[1, 2]), Decision::Invalid);
        assert_eq!(reason(&adv), "advisor: B");
        let adv = parse(r#"{"answer":"A","evidence_ids":[]}"#).unwrap();
        assert_eq!(decide(&adv, &[1]), Decision::Invalid);
        let adv = parse(r#"{"answer":"new","evidence_ids":[1]}"#).unwrap();
        assert_eq!(decide(&adv, &[1]), Decision::Mint);
        let adv = parse(r#"{"answer":"unsure","evidence_ids":[]}"#).unwrap();
        assert_eq!(decide(&adv, &[1]), Decision::Unsure);
        assert!(parse(r#"{"answer":"C","evidence_ids":[]}"#).is_err());
        assert!(parse("nope").is_err());
    }
}
