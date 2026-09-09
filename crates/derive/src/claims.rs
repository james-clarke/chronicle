//! Claims carry evidence (m36 chunk 4). A prose job's evidence lines are
//! numbered (`E1: …`) before the prompt renders; the model answers with
//! claims that each cite the ids they rest on; every id is checked against
//! the lines that were sent, a claim with no surviving id is dropped, and
//! the text renders with one footnote marker per claim. The standup keeps
//! its `[source]` tags as the ids: a bullet's tags are matched to the DATA
//! lines that carried them.

use std::collections::BTreeSet;

/// Number every non-empty line: `E1: text`. Returns the numbered text and
/// the lines in id order (id `n` is `lines[n - 1]`).
pub fn number_lines(text: &str) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    let mut out = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        lines.push(line.trim_end().to_owned());
        out.push_str(&format!("E{}: {}\n", lines.len(), line.trim_end()));
    }
    (out, lines)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Claim {
    pub text: String,
    pub evidence_ids: Vec<i64>,
}

#[derive(serde::Deserialize)]
struct ClaimsOut {
    claims: Vec<Claim>,
}

pub fn parse(out: &str) -> anyhow::Result<Vec<Claim>> {
    let parsed: ClaimsOut = serde_json::from_str(out)
        .map_err(|e| anyhow::anyhow!("model output is not valid claims JSON: {e}: {out}"))?;
    Ok(parsed.claims)
}

/// One evidence line a claim cites, as stored and shown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cited {
    pub id: i64,
    pub text: String,
}

/// A claim whose ids all resolved, with the lines they name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resolved {
    pub text: String,
    pub evidence: Vec<Cited>,
}

/// The verified claims and the faithfulness numbers logged before the drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub claims: Vec<Resolved>,
    /// Claims the model made.
    pub total: usize,
    /// Claims kept: at least one cited id named a line that was sent.
    pub kept: usize,
}

impl Verified {
    /// More than a third dropped: the job is worth one more try.
    pub fn should_retry(&self) -> bool {
        self.total > 0 && self.kept * 3 < self.total * 2
    }

    pub fn json(&self) -> String {
        serde_json::to_string(&self.claims).unwrap_or_else(|_| "[]".into())
    }
}

/// Keep each claim's ids that name a sent line; drop a claim left with
/// none, and a claim with empty text.
pub fn verify(claims: &[Claim], lines: &[String]) -> Verified {
    let mut out = Vec::new();
    for c in claims {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        let ids: BTreeSet<i64> = c
            .evidence_ids
            .iter()
            .copied()
            .filter(|id| *id >= 1 && (*id as usize) <= lines.len())
            .collect();
        if ids.is_empty() {
            continue;
        }
        out.push(Resolved {
            text: text.to_owned(),
            evidence: ids
                .into_iter()
                .map(|id| Cited {
                    id,
                    text: lines[id as usize - 1].clone(),
                })
                .collect(),
        });
    }
    Verified {
        kept: out.len(),
        total: claims.iter().filter(|c| !c.text.trim().is_empty()).count(),
        claims: out,
    }
}

/// The marker a claim's text ends with, `[^n]`, 1-based in claim order.
pub fn marker(n: usize) -> String {
    format!("[^{n}]")
}

/// Prose: the claims one after another, each with its marker.
pub fn render_prose(v: &Verified) -> String {
    v.claims
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} {}", c.text.trim_end_matches(' '), marker(i + 1)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Text without its markers, for places that show the prose plain.
pub fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("[^") {
        let (head, tail) = rest.split_at(i);
        out.push_str(head.trim_end_matches(' '));
        match tail.find(']') {
            Some(j) => rest = &tail[j + 1..],
            None => {
                out.push_str(tail);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.replace("  ", " ").trim().to_owned()
}

/// The standup's claims: every sourced line of the draft, resolved to the
/// digest lines carrying the same `[tag]`. A tagged bullet whose tags name
/// no digest line is counted but not kept, like an uncited claim.
pub fn standup_claims(draft: &str, digest: &str) -> Verified {
    let digest_lines: Vec<&str> = digest.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut claims = Vec::new();
    let mut total = 0;
    for line in draft.lines() {
        let l = line.trim_end();
        let tags = tags_of(l);
        if tags.is_empty() {
            continue;
        }
        total += 1;
        let mut evidence = Vec::new();
        for (i, d) in digest_lines.iter().enumerate() {
            let dtags = tags_of(d);
            if tags.iter().any(|t| dtags.contains(t)) {
                evidence.push(Cited {
                    id: i as i64 + 1,
                    text: (*d).to_owned(),
                });
            }
        }
        if !evidence.is_empty() {
            claims.push(Resolved {
                text: l.to_owned(),
                evidence,
            });
        }
    }
    Verified {
        kept: claims.len(),
        total,
        claims,
    }
}

/// The `[tag]` groups of a line, in order.
fn tags_of(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(i) = rest.find('[') {
        let tail = &rest[i + 1..];
        match tail.find(']') {
            Some(j) => {
                let t = tail[..j].trim();
                if !t.is_empty() && !t.starts_with('^') {
                    out.push(t.to_owned());
                }
                rest = &tail[j + 1..];
            }
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_number_and_claims_verify() {
        let (numbered, lines) = number_lines("Code cart.py\n\nfix login [commit abc]\n");
        assert_eq!(numbered, "E1: Code cart.py\n\nE2: fix login [commit abc]\n");
        assert_eq!(lines.len(), 2);
        let claims = parse(
            r#"{"claims":[{"text":"Fixed login.","evidence_ids":[2,9]},{"text":"Made it up.","evidence_ids":[]},{"text":"  ","evidence_ids":[1]},{"text":"Edited the cart.","evidence_ids":[1,1]}]}"#,
        )
        .unwrap();
        let v = verify(&claims, &lines);
        assert_eq!((v.total, v.kept), (3, 2));
        assert!(!v.should_retry());
        assert_eq!(
            v.claims[0].evidence,
            [Cited {
                id: 2,
                text: "fix login [commit abc]".into()
            }]
        );
        assert_eq!(v.claims[1].evidence.len(), 1);
        assert_eq!(render_prose(&v), "Fixed login. [^1] Edited the cart. [^2]");
        assert_eq!(
            strip_markers("Fixed login. [^1] Edited the cart. [^2]"),
            "Fixed login. Edited the cart."
        );
        let v = verify(&claims[1..2], &lines);
        assert_eq!((v.total, v.kept), (1, 0));
        assert!(v.should_retry());
        assert!(parse("nope").is_err());
        let round: Vec<Resolved> = serde_json::from_str(&verify(&claims, &lines).json()).unwrap();
        assert_eq!(round.len(), 2);
    }

    #[test]
    fn standup_bullets_resolve_by_tag() {
        let digest = "Task: Fix login\n- [09:41] Fixed the redirect [journal 09:41]\nGround truth:\n- fix redirect loop [commit abc1234]\nCheckpoint: nearly done [checkpoint]\n";
        let draft = "Fix login\n- Fixed the redirect [journal 09:41] [commit abc1234]\n- Wrote a lot\n- Something else [note 15:00]\n- Next: ship it [checkpoint]\n";
        let v = standup_claims(draft, digest);
        assert_eq!((v.total, v.kept), (3, 2));
        assert_eq!(
            v.claims[0]
                .evidence
                .iter()
                .map(|c| c.id)
                .collect::<Vec<_>>(),
            [2, 4]
        );
        assert_eq!(v.claims[1].text, "- Next: ship it [checkpoint]");
        assert_eq!(tags_of("x [a] y [^1]"), ["a"]);
    }
}
