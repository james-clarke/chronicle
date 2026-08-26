use jiff::Timestamp;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusEvent {
    pub ts: Timestamp,
    pub app: String,
    pub title: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    Focus(FocusEvent),
    TitleChanged(FocusEvent),
    Afk { idle: bool, ts: Timestamp },
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

/// Storage convention: UTC unix milliseconds, INTEGER. No tz in storage, ever.
pub fn ts_to_ms(ts: Timestamp) -> i64 {
    ts.as_millisecond()
}

pub fn ms_to_ts(ms: i64) -> Timestamp {
    Timestamp::from_millisecond(ms).expect("stored timestamp out of jiff range")
}
