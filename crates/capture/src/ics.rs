//! ICS calendar collector (m37 chunk 4): a plain `.ics` file or URL folded
//! into `meeting` events, the same shape `gcal.rs` produces. No auth, no
//! account — just VEVENT blocks (RFC 5545), including DAILY/WEEKLY RRULE
//! expansion, read on a schedule.

use std::collections::HashSet;
use std::time::Duration;

use chronicle_core::config::expand_home;
use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Span, Timestamp, Zoned};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(15 * 60);
/// Polled window: a week either side of now, every tick.
const WINDOW: SignedDuration = SignedDuration::from_hours(24 * 7);
const MAX_ATTENDEES: usize = 20;
/// Safety valve against a rule with neither COUNT nor UNTIL: never walk
/// past this many candidate weeks/days, window or not.
const MAX_STEPS: usize = 2000;

/// One calendar property line, already unfolded: `NAME;PARAM=VAL;…:VALUE`.
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// RFC 5545 §3.1 line unfolding: a line break followed by a single space or
/// tab is not a line break — it glues the continuation back onto the
/// previous line. Handles both CRLF and bare-LF input.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if (raw.starts_with(' ') || raw.starts_with('\t')) && !lines.is_empty() {
            lines
                .last_mut()
                .expect("checked not empty")
                .push_str(&raw[1..]);
        } else {
            lines.push(raw.to_owned());
        }
    }
    lines
}

fn parse_line(line: &str) -> Option<Prop> {
    let (head, value) = line.split_once(':')?;
    let mut parts = head.split(';');
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.to_owned()))
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_owned(),
    })
}

/// `text` split into unfolded lines, VEVENT blocks parsed into `Meeting`
/// events (recurrences expanded) that fall inside `[lo_ms, hi_ms)`.
pub fn parse_ics(text: &str, lo_ms: i64, hi_ms: i64, now_tz: &TimeZone) -> Vec<ActivityEvent> {
    let lines = unfold(text);
    let mut out = Vec::new();
    let mut block: Option<Vec<Prop>> = None;
    for line in &lines {
        let t = line.trim();
        if t.eq_ignore_ascii_case("BEGIN:VEVENT") {
            block = Some(Vec::new());
            continue;
        }
        if t.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(props) = block.take() {
                out.extend(event_occurrences(&props, lo_ms, hi_ms, now_tz));
            }
            continue;
        }
        if let Some(props) = &mut block
            && let Some(p) = parse_line(line)
        {
            props.push(p);
        }
    }
    out
}

fn get<'a>(props: &'a [Prop], name: &str) -> Option<&'a Prop> {
    props.iter().find(|p| p.name == name)
}

fn event_occurrences(
    props: &[Prop],
    lo_ms: i64,
    hi_ms: i64,
    now_tz: &TimeZone,
) -> Vec<ActivityEvent> {
    if get(props, "STATUS").is_some_and(|p| p.value.trim().eq_ignore_ascii_case("CANCELLED")) {
        return Vec::new();
    }
    let Some(uid) = get(props, "UID").map(|p| p.value.trim().to_owned()) else {
        return Vec::new();
    };
    let Some(dtstart) = get(props, "DTSTART") else {
        return Vec::new();
    };
    let Some((start, all_day)) = parse_dt_prop(dtstart, now_tz) else {
        return Vec::new();
    };
    if all_day {
        return Vec::new();
    }
    let end = compute_end(&start, get(props, "DTEND"), get(props, "DURATION"), now_tz);
    let duration = end.timestamp() - start.timestamp();
    let summary = get(props, "SUMMARY")
        .map(|p| unescape_text(&p.value))
        .filter(|s| !s.trim().is_empty());
    let attendees = parse_attendees(props);

    let Some(rrule_prop) = get(props, "RRULE") else {
        let ms = start.timestamp().as_millisecond();
        return if ms >= lo_ms && ms < hi_ms {
            vec![meeting(
                start.timestamp(),
                end.timestamp(),
                &uid,
                None,
                summary,
                attendees,
            )]
        } else {
            Vec::new()
        };
    };

    let rrule = parse_rrule(&rrule_prop.value, now_tz);
    let exdates = parse_exdates(props, now_tz);
    let occurrences = match rrule.freq {
        Freq::Daily => expand_daily(&rrule, &start, lo_ms, hi_ms),
        Freq::Weekly => expand_weekly(&rrule, &start, lo_ms, hi_ms),
        Freq::Other => {
            let ms = start.timestamp().as_millisecond();
            if ms >= lo_ms && ms < hi_ms {
                vec![start.clone()]
            } else {
                Vec::new()
            }
        }
    };
    occurrences
        .into_iter()
        .filter(|z| !exdates.contains(&z.timestamp().as_millisecond()))
        .map(|z| {
            let occ_start = z.timestamp();
            let occ_end = occ_start + duration;
            meeting(
                occ_start,
                occ_end,
                &uid,
                Some(occ_start.as_millisecond()),
                summary.clone(),
                attendees.clone(),
            )
        })
        .collect()
}

fn meeting(
    start: Timestamp,
    end: Timestamp,
    uid: &str,
    occurrence_ms: Option<i64>,
    summary: Option<String>,
    attendees: Vec<String>,
) -> ActivityEvent {
    let ext_id = match occurrence_ms {
        Some(ms) => format!("ics:{uid}@{ms}"),
        None => format!("ics:{uid}"),
    };
    ActivityEvent {
        ts: start,
        end_ts: Some(end),
        repo: String::new(),
        branch: String::new(),
        kind: ActivityKind::Meeting,
        ext_id: Some(ext_id),
        summary,
        detail: (!attendees.is_empty())
            .then(|| serde_json::json!({ "attendees": attendees }).to_string()),
    }
}

fn compute_end(
    start: &Zoned,
    dtend: Option<&Prop>,
    duration: Option<&Prop>,
    now_tz: &TimeZone,
) -> Zoned {
    if let Some(p) = dtend
        && let Some((z, _)) = parse_dt_prop(p, now_tz)
    {
        return z;
    }
    if let Some(p) = duration
        && let Some(span) = parse_duration(&p.value)
        && let Ok(z) = start.checked_add(span)
    {
        return z;
    }
    start.clone()
}

/// `ATTENDEE;CN=Name;…:mailto:x@y` → `Name`, else the mailto local part.
fn parse_attendees(props: &[Prop]) -> Vec<String> {
    props
        .iter()
        .filter(|p| p.name == "ATTENDEE")
        .filter_map(|p| {
            if let Some(cn) = p.param("CN") {
                let cn = cn.trim();
                if !cn.is_empty() {
                    return Some(cn.to_owned());
                }
            }
            let val = p.value.trim();
            let email = if val.len() >= 7 && val[..7].eq_ignore_ascii_case("mailto:") {
                &val[7..]
            } else {
                val
            };
            let local = email.split('@').next().unwrap_or(email).trim();
            (!local.is_empty()).then(|| local.to_owned())
        })
        .take(MAX_ATTENDEES)
        .collect()
}

/// RFC 5545 TEXT escaping: `\n`/`\N` → newline, `\,` `\;` `\\` → the literal
/// character.
fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// A DATE-TIME or DATE property value, parsed into a zoned instant and
/// whether it was an all-day (`VALUE=DATE`) value.
fn parse_dt_prop(p: &Prop, now_tz: &TimeZone) -> Option<(Zoned, bool)> {
    let is_date = p
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    if is_date {
        let date = parse_date_only(p.value.trim())?;
        let z = date.at(0, 0, 0, 0).to_zoned(TimeZone::UTC).ok()?;
        return Some((z, true));
    }
    let raw = p.value.trim();
    let (body, is_utc) = match raw.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (raw, false),
    };
    let naive = parse_local_datetime(body)?;
    let tz = if is_utc {
        TimeZone::UTC
    } else if let Some(id) = p.param("TZID") {
        TimeZone::get(id).unwrap_or_else(|_| now_tz.clone())
    } else {
        now_tz.clone()
    };
    let z = naive.to_zoned(tz).ok()?;
    Some((z, false))
}

fn parse_date_only(s: &str) -> Option<Date> {
    if s.len() != 8 {
        return None;
    }
    let y: i16 = s.get(0..4)?.parse().ok()?;
    let m: i8 = s.get(4..6)?.parse().ok()?;
    let d: i8 = s.get(6..8)?.parse().ok()?;
    Date::new(y, m, d).ok()
}

fn parse_local_datetime(s: &str) -> Option<jiff::civil::DateTime> {
    if s.len() < 15 || s.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let date = parse_date_only(&s[..8])?;
    let h: i8 = s.get(9..11)?.parse().ok()?;
    let mi: i8 = s.get(11..13)?.parse().ok()?;
    let se: i8 = s.get(13..15)?.parse().ok()?;
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..61).contains(&se) {
        return None;
    }
    Some(date.at(h, mi, se, 0))
}

/// `PT1H`, `PT30M`, `P1D`, `P1DT2H30M` — RFC 5545 DURATION.
fn parse_duration(s: &str) -> Option<Span> {
    let s = s.trim().strip_prefix('P')?;
    let (date_part, time_part) = s.split_once('T').unwrap_or((s, ""));
    let mut span = Span::new();
    let mut num = String::new();
    for c in date_part.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num.parse().ok()?;
        num.clear();
        span = match c {
            'W' => span.weeks(n),
            'D' => span.days(n),
            _ => return None,
        };
    }
    for c in time_part.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: i64 = num.parse().ok()?;
        num.clear();
        span = match c {
            'H' => span.hours(n),
            'M' => span.minutes(n),
            'S' => span.seconds(n),
            _ => return None,
        };
    }
    Some(span)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Freq {
    Daily,
    Weekly,
    Other,
}

struct Rrule {
    freq: Freq,
    interval: i64,
    byday: Vec<Weekday>,
    until: Option<Timestamp>,
    count: Option<i64>,
}

fn parse_rrule(value: &str, now_tz: &TimeZone) -> Rrule {
    let mut freq = Freq::Other;
    let mut interval: i64 = 1;
    let mut byday = Vec::new();
    let mut until = None;
    let mut count = None;
    for part in value.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => {
                freq = match v.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Freq::Daily,
                    "WEEKLY" => Freq::Weekly,
                    _ => Freq::Other,
                }
            }
            "INTERVAL" => interval = v.trim().parse::<i64>().unwrap_or(1).max(1),
            "BYDAY" => byday = v.split(',').filter_map(parse_weekday).collect(),
            "UNTIL" => until = parse_until(v.trim(), now_tz),
            "COUNT" => count = v.trim().parse().ok(),
            _ => {}
        }
    }
    Rrule {
        freq,
        interval: interval.max(1),
        byday,
        until,
        count,
    }
}

fn parse_weekday(code: &str) -> Option<Weekday> {
    // A leading ordinal (`2MO`) is not supported; only the plain weekday
    // codes DAILY/WEEKLY expansion needs.
    Some(match code.trim() {
        "MO" => Weekday::Monday,
        "TU" => Weekday::Tuesday,
        "WE" => Weekday::Wednesday,
        "TH" => Weekday::Thursday,
        "FR" => Weekday::Friday,
        "SA" => Weekday::Saturday,
        "SU" => Weekday::Sunday,
        _ => return None,
    })
}

fn parse_until(v: &str, now_tz: &TimeZone) -> Option<Timestamp> {
    let p = Prop {
        name: "UNTIL".to_owned(),
        params: Vec::new(),
        value: v.to_owned(),
    };
    parse_dt_prop(&p, now_tz).map(|(z, _)| z.timestamp())
}

fn parse_exdates(props: &[Prop], now_tz: &TimeZone) -> HashSet<i64> {
    let mut out = HashSet::new();
    for p in props.iter().filter(|p| p.name == "EXDATE") {
        for v in p.value.split(',') {
            let single = Prop {
                name: "EXDATE".to_owned(),
                params: p.params.clone(),
                value: v.to_owned(),
            };
            if let Some((z, _)) = parse_dt_prop(&single, now_tz) {
                out.insert(z.timestamp().as_millisecond());
            }
        }
    }
    out
}

enum Step {
    Emit(Zoned),
    Skip,
    Stop,
}

/// One candidate occurrence date: applies DTSTART's time-of-day and zone,
/// then UNTIL/COUNT/window. `n` is the running occurrence count (mutated).
fn check_occurrence(
    date: Date,
    start: &Zoned,
    tz: &TimeZone,
    rrule: &Rrule,
    n: &mut i64,
    lo_ms: i64,
    hi_ms: i64,
) -> Step {
    if date < start.date() {
        return Step::Skip;
    }
    let t = start.time();
    let dt = date.at(t.hour(), t.minute(), t.second(), t.subsec_nanosecond());
    let Ok(z) = dt.to_zoned(tz.clone()) else {
        return Step::Skip;
    };
    if let Some(u) = rrule.until
        && z.timestamp() > u
    {
        return Step::Stop;
    }
    if let Some(c) = rrule.count
        && *n >= c
    {
        return Step::Stop;
    }
    *n += 1;
    let ms = z.timestamp().as_millisecond();
    if ms >= hi_ms {
        return Step::Stop;
    }
    if ms < lo_ms {
        return Step::Skip;
    }
    Step::Emit(z)
}

fn expand_daily(rrule: &Rrule, start: &Zoned, lo_ms: i64, hi_ms: i64) -> Vec<Zoned> {
    let tz = start.time_zone().clone();
    let mut out = Vec::new();
    let mut n: i64 = 0;
    let mut date = start.date();
    let step = Span::new().days(rrule.interval);
    for _ in 0..MAX_STEPS {
        match check_occurrence(date, start, &tz, rrule, &mut n, lo_ms, hi_ms) {
            Step::Emit(z) => out.push(z),
            Step::Skip => {}
            Step::Stop => break,
        }
        date += step;
    }
    out
}

/// Weekly, honouring BYDAY: each qualifying week emits its weekdays in
/// Monday-first order (matching RFC 5545 generation order), so the date
/// sequence stays non-decreasing and `check_occurrence`'s window/COUNT/
/// UNTIL early exit stays valid.
fn expand_weekly(rrule: &Rrule, start: &Zoned, lo_ms: i64, hi_ms: i64) -> Vec<Zoned> {
    let tz = start.time_zone().clone();
    let mut out = Vec::new();
    let mut n: i64 = 0;
    let mut weekdays: Vec<Weekday> = if rrule.byday.is_empty() {
        vec![start.date().weekday()]
    } else {
        rrule.byday.clone()
    };
    weekdays.sort_by_key(|w| w.to_monday_zero_offset());
    weekdays.dedup();
    let monday_offset = start.date().weekday().to_monday_zero_offset() as i64;
    let mut week_monday = start.date() - Span::new().days(monday_offset);
    let week_step = Span::new().weeks(rrule.interval);
    'weeks: for _ in 0..MAX_STEPS {
        for wd in &weekdays {
            let offset = i64::from(wd.to_monday_zero_offset());
            let date = week_monday + Span::new().days(offset);
            match check_occurrence(date, start, &tz, rrule, &mut n, lo_ms, hi_ms) {
                Step::Emit(z) => out.push(z),
                Step::Skip => {}
                Step::Stop => break 'weeks,
            }
        }
        week_monday += week_step;
    }
    out
}

fn fetch(src: &str) -> Result<String, BoxError> {
    if src.starts_with("http://") || src.starts_with("https://") {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .new_agent();
        let mut resp = agent.get(src).call()?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_string()?;
        if status != 200 {
            let head: String = body.trim().chars().take(200).collect();
            return Err(format!("ics fetch: HTTP {status}: {head}").into());
        }
        Ok(body)
    } else {
        Ok(std::fs::read_to_string(src)?)
    }
}

pub struct IcsProvider {
    sources: Vec<String>,
    tz: TimeZone,
    last_err: Option<String>,
}

impl IcsProvider {
    /// `sources` are URLs or paths (`~` expanded).
    pub fn new(sources: Vec<String>, tz: TimeZone) -> Self {
        let sources = sources
            .into_iter()
            .map(|s| {
                if s.starts_with("http://") || s.starts_with("https://") {
                    s
                } else {
                    expand_home(&s).to_string_lossy().into_owned()
                }
            })
            .collect();
        Self {
            sources,
            tz,
            last_err: None,
        }
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        let now = Timestamp::now();
        let lo_ms = (now - WINDOW).as_millisecond();
        let hi_ms = (now + WINDOW).as_millisecond();
        let mut out = Vec::new();
        let mut err: Option<String> = None;
        for src in &self.sources {
            match fetch(src) {
                Ok(text) => out.extend(parse_ics(&text, lo_ms, hi_ms, &self.tz)),
                Err(e) => {
                    if err.is_none() {
                        err = Some(format!("{src}: {e}"));
                    }
                }
            }
        }
        match &err {
            None => self.last_err = None,
            Some(e) => {
                if self.last_err.as_deref() != Some(e.as_str()) {
                    tracing::warn!("ics poll: {e}");
                    self.last_err = Some(e.clone());
                }
            }
        }
        out
    }
}

impl FocusProvider for IcsProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn unfold_glues_continuation_lines() {
        let text = "BEGIN:VEVENT\r\nSUMMARY:Long summary contin\r\n ued here\r\nEND:VEVENT";
        let lines = unfold(text);
        assert_eq!(
            lines,
            vec![
                "BEGIN:VEVENT".to_owned(),
                "SUMMARY:Long summary continued here".to_owned(),
                "END:VEVENT".to_owned(),
            ]
        );
    }

    const FIXTURE: &str = "BEGIN:VCALENDAR\r\n\
        VERSION:2.0\r\n\
        BEGIN:VEVENT\r\n\
        UID:utc-1\r\n\
        SUMMARY:Standup\r\n\
        DTSTART:20260909T140000Z\r\n\
        DTEND:20260909T143000Z\r\n\
        ATTENDEE;CN=Alice:mailto:alice@example.com\r\n\
        ATTENDEE:mailto:bob@example.com\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:tz-1\r\n\
        SUMMARY:1:1 with manager\r\n\
        DTSTART;TZID=America/New_York:20260909T100000\r\n\
        DURATION:PT45M\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:allday-1\r\n\
        SUMMARY:Company holiday\r\n\
        DTSTART;VALUE=DATE:20260910\r\n\
        DTEND;VALUE=DATE:20260911\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:cancelled-1\r\n\
        SUMMARY:Old sync\r\n\
        STATUS:CANCELLED\r\n\
        DTSTART:20260909T160000Z\r\n\
        DTEND:20260909T163000Z\r\n\
        END:VEVENT\r\n\
        BEGIN:VEVENT\r\n\
        UID:weekly-1\r\n\
        SUMMARY:Team sync\r\n\
        DTSTART:20260907T090000Z\r\n\
        DTEND:20260907T093000Z\r\n\
        RRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR;COUNT=6\r\n\
        EXDATE:20260911T090000Z\r\n\
        END:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn utc_event_with_attendees() {
        let got = parse_ics(
            FIXTURE,
            ts("2026-09-01T00:00:00Z").as_millisecond(),
            ts("2026-09-30T00:00:00Z").as_millisecond(),
            &TimeZone::UTC,
        );
        let ev = got
            .iter()
            .find(|e| e.ext_id.as_deref() == Some("ics:utc-1"))
            .expect("utc event");
        assert_eq!(ev.kind, ActivityKind::Meeting);
        assert_eq!(ev.ts, ts("2026-09-09T14:00:00Z"));
        assert_eq!(ev.end_ts, Some(ts("2026-09-09T14:30:00Z")));
        assert_eq!(ev.summary.as_deref(), Some("Standup"));
        let d: serde_json::Value = serde_json::from_str(ev.detail.as_deref().unwrap()).unwrap();
        assert_eq!(d["attendees"], serde_json::json!(["Alice", "bob"]));
    }

    #[test]
    fn tzid_event_with_duration() {
        let got = parse_ics(
            FIXTURE,
            ts("2026-09-01T00:00:00Z").as_millisecond(),
            ts("2026-09-30T00:00:00Z").as_millisecond(),
            &TimeZone::UTC,
        );
        let ev = got
            .iter()
            .find(|e| e.ext_id.as_deref() == Some("ics:tz-1"))
            .expect("tz event");
        // America/New_York is UTC-4 (EDT) in September.
        assert_eq!(ev.ts, ts("2026-09-09T14:00:00Z"));
        assert_eq!(ev.end_ts, Some(ts("2026-09-09T14:45:00Z")));
    }

    #[test]
    fn all_day_and_cancelled_events_are_skipped() {
        let got = parse_ics(
            FIXTURE,
            ts("2026-09-01T00:00:00Z").as_millisecond(),
            ts("2026-09-30T00:00:00Z").as_millisecond(),
            &TimeZone::UTC,
        );
        assert!(
            !got.iter()
                .any(|e| e.ext_id.as_deref() == Some("ics:allday-1"))
        );
        assert!(!got.iter().any(|e| {
            e.ext_id
                .as_deref()
                .is_some_and(|id| id.starts_with("ics:cancelled-1"))
        }));
    }

    #[test]
    fn weekly_rrule_expands_with_count_and_exdate() {
        let got = parse_ics(
            FIXTURE,
            ts("2026-09-01T00:00:00Z").as_millisecond(),
            ts("2026-09-30T00:00:00Z").as_millisecond(),
            &TimeZone::UTC,
        );
        let weekly: Vec<&ActivityEvent> = got
            .iter()
            .filter(|e| {
                e.ext_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("ics:weekly-1@"))
            })
            .collect();
        // COUNT=6 raw occurrences (Mon/Wed/Fri from 9/7), minus the 9/11 EXDATE.
        assert_eq!(weekly.len(), 5, "{weekly:?}");
        let starts: Vec<Timestamp> = weekly.iter().map(|e| e.ts).collect();
        assert!(starts.contains(&ts("2026-09-07T09:00:00Z")));
        assert!(starts.contains(&ts("2026-09-09T09:00:00Z")));
        assert!(
            !starts.contains(&ts("2026-09-11T09:00:00Z")),
            "excluded by EXDATE"
        );
        assert!(starts.contains(&ts("2026-09-14T09:00:00Z")));
        assert!(starts.contains(&ts("2026-09-16T09:00:00Z")));
        assert!(starts.contains(&ts("2026-09-18T09:00:00Z")));
        assert!(
            !starts.contains(&ts("2026-09-21T09:00:00Z")),
            "beyond COUNT=6"
        );
        for e in &weekly {
            let expected_id = format!("ics:weekly-1@{}", e.ts.as_millisecond());
            assert_eq!(e.ext_id.as_deref(), Some(expected_id.as_str()));
            assert_eq!(e.summary.as_deref(), Some("Team sync"));
        }
    }

    #[test]
    fn weekly_window_narrows_which_occurrences_come_back() {
        let got = parse_ics(
            FIXTURE,
            ts("2026-09-13T00:00:00Z").as_millisecond(),
            ts("2026-09-17T00:00:00Z").as_millisecond(),
            &TimeZone::UTC,
        );
        let starts: Vec<Timestamp> = got
            .iter()
            .filter(|e| {
                e.ext_id
                    .as_deref()
                    .is_some_and(|id| id.starts_with("ics:weekly-1@"))
            })
            .map(|e| e.ts)
            .collect();
        assert_eq!(
            starts,
            vec![ts("2026-09-14T09:00:00Z"), ts("2026-09-16T09:00:00Z")]
        );
    }

    #[test]
    fn parse_duration_handles_hours_minutes_and_days() {
        assert_eq!(
            parse_duration("PT1H").unwrap().fieldwise(),
            Span::new().hours(1).fieldwise()
        );
        assert_eq!(
            parse_duration("PT30M").unwrap().fieldwise(),
            Span::new().minutes(30).fieldwise()
        );
        assert_eq!(
            parse_duration("P1D").unwrap().fieldwise(),
            Span::new().days(1).fieldwise()
        );
        assert!(parse_duration("nope").is_none());
    }
}
