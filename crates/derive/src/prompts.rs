//! Prompt rendering and output parsing for the describer family, shared by
//! the local runner and the cloud backends (m31): both engines see the same
//! text, byte for byte, except the `/no_think` head the cloud path strips.

use anyhow::Context;

use chronicle_core::types::SuggestedTask;

pub const DESCRIPTION_PROMPT: &str = include_str!("../../../prompts/task_description_v1.txt");
pub const SUGGEST_PROMPT: &str = include_str!("../../../prompts/suggest_task_v1.txt");
pub const NARRATIVE_PROMPT: &str = include_str!("../../../prompts/narrative_v1.txt");
pub const JOURNAL_PROMPT: &str = include_str!("../../../prompts/journal_v1.txt");
pub const CHECKPOINT_PROMPT: &str = include_str!("../../../prompts/checkpoint_v1.txt");
pub const STANDUP_PROMPT: &str = include_str!("../../../prompts/standup_v1.txt");
pub const CHECKPOINT_GRAMMAR: &str = include_str!("../../../grammars/checkpoint_v1.gbnf");
pub const SUGGEST_GRAMMAR: &str = include_str!("../../../grammars/suggest_task_v1.gbnf");
pub const CHECKPOINT_SCHEMA: &str = include_str!("../../../grammars/checkpoint_v1.json");
pub const SUGGEST_SCHEMA: &str = include_str!("../../../grammars/suggest_task_v1.json");

/// Qwen3's thinking switch heads every local template; a cloud model has
/// no such switch and would read it as content.
pub const NO_THINK: &str = "/no_think";

/// The template without its `/no_think` first line, for cloud backends.
pub fn strip_no_think(prompt: &str) -> &str {
    match prompt.strip_prefix(NO_THINK) {
        Some(rest) => rest.strip_prefix('\n').unwrap_or(rest),
        None => prompt,
    }
}

fn project_line(project: Option<&str>) -> String {
    project.map(|p| format!(" [{p}]")).unwrap_or_default()
}

fn context_section(context: &str) -> String {
    if context.trim().is_empty() {
        String::new()
    } else {
        format!("Ticket context:\n{}\n", context.trim())
    }
}

pub fn render_description(label: &str, project: Option<&str>, evidence: &str) -> String {
    DESCRIPTION_PROMPT
        .replace("{label}", label)
        .replace("{project_line}", &project_line(project))
        .replace("{evidence}", evidence)
}

pub fn render_journal(
    label: &str,
    project: Option<&str>,
    context: &str,
    truth: &str,
    evidence: &str,
) -> String {
    JOURNAL_PROMPT
        .replace("{label}", label)
        .replace("{project_line}", &project_line(project))
        .replace("{context_section}", &context_section(context))
        .replace(
            "{truth}",
            if truth.trim().is_empty() {
                "(none)"
            } else {
                truth
            },
        )
        .replace(
            "{evidence}",
            if evidence.trim().is_empty() {
                "(none)"
            } else {
                evidence
            },
        )
}

pub fn render_narrative(digest: &str) -> String {
    NARRATIVE_PROMPT.replace("{digest}", digest)
}

pub fn render_standup(digest: &str) -> String {
    STANDUP_PROMPT.replace("{digest}", digest)
}

pub fn render_checkpoint(
    label: &str,
    project: Option<&str>,
    context: &str,
    journal: &str,
) -> String {
    CHECKPOINT_PROMPT
        .replace("{label}", label)
        .replace("{project_line}", &project_line(project))
        .replace("{context_section}", &context_section(context))
        .replace("{journal}", journal)
}

pub fn render_suggest(digest: &str) -> String {
    SUGGEST_PROMPT.replace("{digest}", digest)
}

#[derive(serde::Deserialize)]
struct CheckpointOut {
    state: String,
    next_steps: String,
}

/// `(state, next_steps)` from the checkpoint JSON.
pub fn parse_checkpoint(out: &str) -> anyhow::Result<(String, String)> {
    let parsed: CheckpointOut = serde_json::from_str(out)
        .with_context(|| format!("model output is not valid checkpoint JSON: {out}"))?;
    Ok((parsed.state, parsed.next_steps))
}

pub fn parse_suggest(out: &str) -> anyhow::Result<SuggestedTask> {
    serde_json::from_str(out)
        .with_context(|| format!("model output is not valid suggestion JSON: {out}"))
}

/// A standup claim must name its source (m32 chunk 5): every bullet and
/// every line after a block's first ends in a `[...]` tag, or it goes. A
/// block's first line (the task's label) stays while a claim under it
/// stays; a first line that carries a tag is itself a claim. `None` when
/// nothing sourced survives.
pub fn keep_sourced(text: &str) -> Option<String> {
    let sourced = |l: &str| {
        let l = l.trim_end();
        l.ends_with(']') && l.contains('[')
    };
    let mut out: Vec<String> = Vec::new();
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some((first, rest)) = lines.split_first() else {
            continue;
        };
        let kept: Vec<&str> = rest.iter().copied().filter(|l| sourced(l)).collect();
        if sourced(first) || !kept.is_empty() {
            let mut b = vec![first.trim_end()];
            b.extend(kept);
            out.push(b.join("\n"));
        }
    }
    (!out.is_empty()).then(|| out.join("\n\n"))
}

/// A prose completion that must not be empty.
pub fn non_empty(out: String, what: &str) -> anyhow::Result<String> {
    if out.trim().is_empty() {
        anyhow::bail!("model produced an empty {what}");
    }
    Ok(out.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_templates_keep_the_no_think_head() {
        for (name, t) in [
            ("description", DESCRIPTION_PROMPT),
            ("suggest", SUGGEST_PROMPT),
            ("narrative", NARRATIVE_PROMPT),
            ("journal", JOURNAL_PROMPT),
            ("checkpoint", CHECKPOINT_PROMPT),
            ("standup", STANDUP_PROMPT),
        ] {
            assert!(t.starts_with("/no_think\n"), "{name}");
            let stripped = strip_no_think(t);
            assert!(!stripped.contains("/no_think"), "{name}");
            assert_eq!(stripped.len(), t.len() - "/no_think\n".len(), "{name}");
        }
        assert_eq!(strip_no_think("plain"), "plain");
    }

    #[test]
    fn journal_renders_optional_sections() {
        let p = render_journal("Fix login", Some("web"), "", "", "ev");
        assert!(p.contains("Fix login [web]"));
        assert!(!p.contains("Ticket context"));
        assert!(p.contains("Ground truth this session:\n(none)\n"));
        let p = render_journal("Fix login", None, " ctx ", "g1", "");
        assert!(p.contains("Ticket context:\nctx\n"));
        assert!(p.contains("g1"));
        assert!(p.contains("Screen evidence this session:\n(none)\n"));
    }

    #[test]
    fn unsourced_claims_are_dropped() {
        let text = "Fix login\n- Fixed the redirect [commit abc1234]\n- Also tidied things up\n- Next: ship it [checkpoint]\n\nDocs\n- Wrote a lot\n\nPlanned X, spent most of the day on Y. [plan]\n";
        assert_eq!(
            keep_sourced(text).as_deref(),
            Some(
                "Fix login\n- Fixed the redirect [commit abc1234]\n- Next: ship it [checkpoint]\n\nPlanned X, spent most of the day on Y. [plan]"
            )
        );
        assert_eq!(keep_sourced("Docs\n- Wrote a lot\n"), None);
        assert_eq!(keep_sourced(""), None);
    }

    /// Structured outputs accept only a subset of JSON Schema: every object
    /// closed, no length or numeric bounds (those live in the GBNF and in
    /// post-parse clamps).
    #[test]
    fn schemas_stay_inside_the_structured_output_subset() {
        fn walk(v: &serde_json::Value) {
            if let Some(o) = v.as_object() {
                if o.get("type").map_or(false, |t| t == "object") {
                    assert_eq!(o["additionalProperties"], false, "{v}");
                    assert!(o.contains_key("required"), "{v}");
                }
                for k in ["minLength", "maxLength", "minimum", "maximum", "pattern"] {
                    assert!(!o.contains_key(k), "{k} in {v}");
                }
                o.values().for_each(walk);
            } else if let Some(a) = v.as_array() {
                a.iter().for_each(walk);
            }
        }
        for s in [
            CHECKPOINT_SCHEMA,
            SUGGEST_SCHEMA,
            crate::runner::Prompt::Batch.schema_json(),
            crate::runner::Prompt::Live.schema_json(),
            crate::runner::Prompt::Consolidate.schema_json(),
        ] {
            walk(&serde_json::from_str(s).unwrap());
        }
        // Samples shaped by the schemas parse into the runtime types.
        let s: SuggestedTask =
            serde_json::from_str(r#"{"label":"a","project":null,"description":null}"#).unwrap();
        assert_eq!(s.label, "a");
        let d: chronicle_core::types::DeriveOutput = serde_json::from_str(
            r#"{"intervals":[{"ref":null,"label":"x","project":null,"start":0,"end":5,"confidence":0.8}]}"#,
        )
        .unwrap();
        assert_eq!(d.intervals.len(), 1);
    }

    #[test]
    fn checkpoint_and_suggest_parse() {
        let (s, n) = parse_checkpoint(r#"{"state":"a","next_steps":"b"}"#).unwrap();
        assert_eq!((s.as_str(), n.as_str()), ("a", "b"));
        assert!(parse_checkpoint("{}").is_err());
        assert!(non_empty("  ".into(), "narrative").is_err());
        assert_eq!(non_empty(" x ".into(), "narrative").unwrap(), "x");
    }
}
