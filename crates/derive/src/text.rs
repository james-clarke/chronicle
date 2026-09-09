//! The one-request text completion surface a job kind is routed through
//! (m31): the local llama.cpp describer and the cloud backends both stand
//! behind [`TextBackend`], and a job asks for its output shape once via
//! [`Request::schema`] (GBNF locally, JSON schema on the wire).

use serde_json::Value;

/// A job kind as stored in `ai_jobs.kind`, plus chat and the derive tiers
/// for the bench and the routing table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobKind {
    TaskDescription,
    SuggestTask,
    NameTask,
    Journal,
    Checkpoint,
    Narrative,
    Standup,
    Chat,
    Derive,
    Live,
    Consolidate,
}

impl JobKind {
    /// Every kind a route can name, in Settings order.
    pub const ALL: [JobKind; 11] = [
        JobKind::Chat,
        JobKind::Narrative,
        JobKind::Standup,
        JobKind::Journal,
        JobKind::TaskDescription,
        JobKind::Checkpoint,
        JobKind::SuggestTask,
        JobKind::NameTask,
        JobKind::Consolidate,
        JobKind::Derive,
        JobKind::Live,
    ];

    /// The `ai_jobs.kind` / `[routes]` key.
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::TaskDescription => "task_description",
            JobKind::SuggestTask => "suggest_task",
            JobKind::NameTask => "name_task",
            JobKind::Journal => "journal",
            JobKind::Checkpoint => "checkpoint",
            JobKind::Narrative => "narrative",
            JobKind::Standup => "standup",
            JobKind::Chat => "chat",
            JobKind::Derive => "derive",
            JobKind::Live => "live",
            JobKind::Consolidate => "consolidate",
        }
    }

    pub fn parse(s: &str) -> Option<JobKind> {
        JobKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The jobs the "writing and chat" preset routes to a cloud backend:
    /// free text and naming, where the 4B invents or truncates.
    pub fn is_writing(self) -> bool {
        !matches!(self, JobKind::Derive | JobKind::Live | JobKind::Consolidate)
    }

    /// Output is a JSON object (schema-constrained on every backend).
    pub fn is_json(self) -> bool {
        matches!(
            self,
            JobKind::SuggestTask
                | JobKind::NameTask
                | JobKind::Checkpoint
                | JobKind::Derive
                | JobKind::Live
                | JobKind::Consolidate
        )
    }
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One completion request. `system` is the frozen prefix of the prompt
/// (instruction, schema rules, the open-task list; chat's template), the
/// part the provider caches; `user` is the volatile digest
/// (`prompts::split_prefix`); `history` is chat's prior (question, answer)
/// turns.
pub struct Request<'a> {
    pub job: JobKind,
    pub system: Option<&'a str>,
    pub user: &'a str,
    pub history: &'a [(String, String)],
    /// JSON schema the output must satisfy. The local engine maps the job
    /// to its GBNF grammar instead and ignores the schema body.
    pub schema: Option<&'a Value>,
    pub max_output: u32,
}

/// What a backend returned and what it cost.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Prompt tokens served from the provider's cache (billed at 0.1×).
    pub cache_read_tokens: u32,
    pub wall_ms: u64,
    /// The provider's own cost figure when it reports one (Claude Code's
    /// `total_cost_usd`); `cloud::cost_usd` prefers it to the price table.
    pub cost_usd: Option<f64>,
    /// Redaction classes that fired on the request (m36 chunk 1); set by
    /// the `cloud::Redacting` wrapper, empty on the local path.
    pub redactions: Vec<crate::redact::Class>,
}

pub trait TextBackend: Send + Sync {
    /// The `[backends.<name>]` key, for `ai_jobs.backend` and the UI.
    fn name(&self) -> &str;
    /// Provider model id, for the egress line and the price table.
    fn model(&self) -> &str;
    /// Prompt budget in tokens (context window minus output headroom).
    fn context_tokens(&self) -> usize;
    fn complete(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<Completion>;
}

#[cfg(test)]
mod tests {
    use super::JobKind;

    #[test]
    fn kinds_round_trip_their_db_strings() {
        for k in JobKind::ALL {
            assert_eq!(JobKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(JobKind::parse("naming"), None);
        assert!(JobKind::NameTask.is_writing() && JobKind::NameTask.is_json());
        assert!(!JobKind::Derive.is_writing());
        assert!(!JobKind::Journal.is_json());
    }
}
