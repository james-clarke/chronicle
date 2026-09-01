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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    Focus(FocusEvent),
    TitleChanged(FocusEvent),
    Url(UrlEvent),
    Afk { idle: bool, ts: Timestamp },
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
}

/// A past user correction surfaced into the digest as few-shot guidance.
#[derive(Debug, Clone)]
pub struct Correction {
    pub old_label: String,
    pub new_label: String,
    pub old_project: Option<String>,
    pub new_project: Option<String>,
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
/// the start of the digest window.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct IntervalDraft {
    #[serde(rename = "ref")]
    pub task_ref: Option<i64>,
    pub label: Option<String>,
    pub project: Option<String>,
    pub start_offset_min: i64,
    pub end_offset_min: i64,
    pub confidence: f64,
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
