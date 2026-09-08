use jiff::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusEvent {
    pub ts: Timestamp,
    pub app: String,
    pub title: String,
    pub pid: Option<u32>,
}

/// A browser heartbeat mapped to a page-change event (AW endpoint, M6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlEvent {
    pub ts: Timestamp,
    /// `browser:<name>`, derived from the AW bucket id.
    pub app: String,
    /// Page title as reported by the extension.
    pub title: String,
    pub url: String,
}

/// A point or span marker from a local collector (m15 git, m22 the rest).
/// Stored in `activity_events`, never in `events` — evidence, not focus time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEvent {
    pub ts: Timestamp,
    /// Span-like kinds only (AI session, call): last seen / hung up.
    pub end_ts: Option<Timestamp>,
    /// Short scope name: repo directory basename, cwd basename, PR repo
    /// name; empty when the kind has none.
    pub repo: String,
    /// Empty when the kind has none.
    pub branch: String,
    pub kind: ActivityKind,
    /// Per-kind external identity and dedupe key: commit hash, session id,
    /// PR url, `call:<start ms>`.
    pub ext_id: Option<String>,
    /// Commit subject, first prompt, PR title, calling app.
    pub summary: Option<String>,
    /// Per-kind JSON the anchors read (m30): `{"path","language"}` for
    /// edits, `{"prompts":[..],"paths":[..]}` for AI sessions,
    /// `{"attendees":[..]}` for meetings. `None` for kinds without one.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Checkout,
    Commit,
    AiSession,
    PrAuthored,
    PrReviewed,
    Call,
    /// Calendar event (m26 chunk 2): `ts`/`end_ts` = scheduled start/end.
    Meeting,
    /// Editor heartbeats folded per (project, branch) (m26 chunk 3).
    Edit,
    /// Shell commands folded per cwd repo (m26 chunk 4); never the command line.
    Shell,
    /// Working directory of the focused terminal's shell, read from the
    /// process tree on focus (m30): one row per `(terminal pid, place)`,
    /// `ts` first seen, `end_ts` last seen. Anchors only; never rendered.
    Cwd,
}

/// How a repeated observation of the same `ext_id` is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dedupe {
    /// Checkout: dropped when it matches the repo's latest stored checkout.
    LatestCheckout,
    /// Every observation is a row.
    None,
    /// One row per `(kind, ext_id)`; a repeat rewrites `ts`/`end_ts` (and
    /// an empty summary).
    Upsert,
    /// One row per `(kind, ext_id, ts)`; repeats are ignored.
    Ignore,
}

impl ActivityKind {
    pub const ALL: [ActivityKind; 10] = [
        ActivityKind::Checkout,
        ActivityKind::Commit,
        ActivityKind::AiSession,
        ActivityKind::PrAuthored,
        ActivityKind::PrReviewed,
        ActivityKind::Call,
        ActivityKind::Meeting,
        ActivityKind::Edit,
        ActivityKind::Shell,
        ActivityKind::Cwd,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ActivityKind::Checkout => "checkout",
            ActivityKind::Commit => "commit",
            ActivityKind::AiSession => "ai_session",
            ActivityKind::PrAuthored => "pr_authored",
            ActivityKind::PrReviewed => "pr_reviewed",
            ActivityKind::Call => "call",
            ActivityKind::Meeting => "meeting",
            ActivityKind::Edit => "edit",
            ActivityKind::Shell => "shell",
            ActivityKind::Cwd => "cwd",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Git kinds: the only ones the repo rows and branch coverage look at.
    pub fn is_vcs(self) -> bool {
        matches!(self, ActivityKind::Checkout | ActivityKind::Commit)
    }

    /// PR markers: their title carries the ticket key, not a branch.
    pub fn is_pr(self) -> bool {
        matches!(self, ActivityKind::PrAuthored | ActivityKind::PrReviewed)
    }

    pub fn dedupe(self) -> Dedupe {
        match self {
            ActivityKind::Checkout => Dedupe::LatestCheckout,
            ActivityKind::Commit => Dedupe::None,
            ActivityKind::AiSession
            | ActivityKind::Call
            | ActivityKind::Meeting
            | ActivityKind::Edit
            | ActivityKind::Shell
            | ActivityKind::Cwd => Dedupe::Upsert,
            ActivityKind::PrAuthored | ActivityKind::PrReviewed => Dedupe::Ignore,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    Focus(FocusEvent),
    TitleChanged(FocusEvent),
    Url(UrlEvent),
    Activity(ActivityEvent),
    Afk {
        idle: bool,
        ts: Timestamp,
    },
    /// Screen lock edge, or capture itself gone (focus provider exit): the
    /// sessionizer closes at once, never folds it as quiet (m32 chunk 1).
    Lock {
        locked: bool,
        ts: Timestamp,
    },
    Presence(PresenceMinute),
}

/// One minute of input counts (m32 chunk 1); never what was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PresenceMinute {
    /// Minute-aligned ms since the epoch.
    pub minute_ts: i64,
    pub keys: u32,
    pub buttons: u32,
    pub motion: u32,
    pub scroll: u32,
}

/// A stored `events` row; also the fixture JSONL line format.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Event {
    pub ts: Timestamp,
    pub kind: String,
    #[serde(default)]
    pub app: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub idle: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct Span {
    pub id: i64,
    pub start_ts: Timestamp,
    pub end_ts: Timestamp,
    pub app: String,
    pub title: String,
    pub kind: String,
}

/// Read model: one stored interval joined with its task identity.
/// `id` is the identity (what corrections and reassignment target);
/// `interval_id` is the time block.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub interval_id: i64,
    pub label: String,
    pub project: Option<String>,
    pub start_ts: Timestamp,
    pub end_ts: Timestamp,
    pub confidence: f64,
    pub declared: bool,
    /// User-edited or AI-generated summary of the task (tasks.description).
    pub description: Option<String>,
    /// External anchor (ticket key), set deterministically from branch names.
    pub external_ref: Option<String>,
    /// The kind of work the interval was (m30 chunk 5; `segment` rows only).
    pub kind: Option<String>,
}

/// A past user correction surfaced into the digest as few-shot guidance.
#[derive(Debug, Clone)]
pub struct Correction {
    pub old_label: String,
    pub new_label: String,
    pub old_project: Option<String>,
    pub new_project: Option<String>,
    /// 'rename' | 'reassign' | 'assign' | 'merge' | 'eject' | …; an eject's
    /// old_* is the task the work was pulled from, new_label '(unassigned)'.
    pub kind: String,
    /// The app/title lines the correction was made over (FTS context); the
    /// digest quotes an eject's first line as the work that is *not* the task.
    pub ctx: String,
}

/// The model's interval JSON (derive v3), parsed. Lives in core (not the
/// llama crate) so linking, post-merge, and eval scoring stay testable
/// without llama.
#[derive(Debug, serde::Deserialize)]
pub struct DeriveOutput {
    pub intervals: Vec<IntervalDraft>,
}

/// One model-proposed interval. `task_ref` is a 1-based index into the
/// digest's numbered "## Open tasks" list (deterministic linking, no string
/// matching); None proposes a new task via `label`. Offsets are minutes from
/// the start of the digest window. The v4 grammar emits `start`/`end`; the
/// v3 names stay accepted for fixtures and stored outputs.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IntervalDraft {
    #[serde(rename = "ref")]
    pub task_ref: Option<i64>,
    pub label: Option<String>,
    pub project: Option<String>,
    #[serde(alias = "start")]
    pub start_offset_min: i64,
    #[serde(alias = "end")]
    pub end_offset_min: i64,
    pub confidence: f64,
}

/// The live tier's answer (m27 chunk 4): the one task of the current
/// stretch. Same fields as an `IntervalDraft` minus the offsets, which the
/// daemon owns.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LiveDraft {
    #[serde(rename = "ref")]
    pub task_ref: Option<i64>,
    pub label: Option<String>,
    pub project: Option<String>,
    pub confidence: f64,
}

/// A declare-suggestion from the model: what the user seems to be working on
/// right now, offered as a pre-fill for the declare-a-task row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SuggestedTask {
    pub label: String,
    pub project: Option<String>,
    pub description: Option<String>,
}

/// An open task offered to the model in the digest's numbered list.
#[derive(Debug, Clone)]
pub struct OpenTask {
    pub id: i64,
    pub label: String,
    pub project: Option<String>,
    /// true = user-declared (source 'user'), listed first and marked in the digest.
    pub declared: bool,
}

/// A resolved interval identity used for bench scoring: what a stored
/// interval's task would look like after linking.
#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub label: String,
    pub project: Option<String>,
    pub start_offset_min: i64,
    pub end_offset_min: i64,
    pub confidence: f64,
}

/// A task identity to write: an existing row or a new one to create.
#[derive(Debug, Clone)]
pub enum TaskSlot {
    Existing(i64),
    New {
        label: String,
        project: Option<String>,
    },
}

/// An interval to insert; `slot` indexes the `TaskSlot` list of the same
/// derivation.
#[derive(Debug, Clone)]
pub struct NewInterval {
    pub slot: usize,
    pub start_ts: Timestamp,
    pub end_ts: Timestamp,
    pub confidence: f64,
}

/// Storage convention: UTC unix milliseconds, INTEGER. No tz in storage, ever.
pub fn ts_to_ms(ts: Timestamp) -> i64 {
    ts.as_millisecond()
}

pub fn ms_to_ts(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).expect("stored timestamp out of jiff range")
}
