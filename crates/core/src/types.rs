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

#[derive(Debug, Clone)]
pub struct Task {
    pub id: i64,
    pub batch_id: i64,
    pub label: String,
    pub project: Option<String>,
    pub start_ts: Timestamp,
    pub end_ts: Timestamp,
    pub confidence: f64,
}

/// A past user correction surfaced into the digest as few-shot guidance.
#[derive(Debug, Clone)]
pub struct Correction {
    pub old_label: String,
    pub new_label: String,
    pub old_project: Option<String>,
    pub new_project: Option<String>,
}

/// A task to insert; `Task` is the stored row.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub label: String,
    pub project: Option<String>,
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
