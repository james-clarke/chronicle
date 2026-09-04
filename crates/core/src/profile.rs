//! m30 chunk 2: task evidence profiles and the segment scorer.
//!
//! A profile is what a task has been seen with: the anchors and title terms
//! of the focus spans under its intervals, plus what corrections and the
//! task's own declaration say. Scoring a segment against the profiles is a
//! pure function; the winner takes the segment when its margin over the
//! runner-up clears [`Params::delta`], else the segment is "to confirm".
//! Nothing here touches the model or the database.

use std::collections::{BTreeMap, HashMap};

use regex::Regex;

use crate::extract::{Anchor, AnchorKind, Strength};
use crate::replay::{CorrectionRow, IntervalRow, TaskRow};

/// Numeric strength of an anchor kind in the score.
pub fn strength_weight(s: Strength) -> f64 {
    match s {
        Strength::Strong => 1.0,
        Strength::Medium => 0.5,
        Strength::Weak => 0.2,
    }
}

/// Weight of one matching title term.
pub const TERM_WEIGHT: f64 = 0.08;
/// Shortest title term kept.
const MIN_TERM_CHARS: usize = 4;
/// Title words that name browsers, apps and window furniture, not work.
const TERM_STOP: &[&str] = &[
    "mozilla",
    "firefox",
    "google",
    "chrome",
    "chromium",
    "https",
    "http",
    "window",
    "untitled",
    "private",
    "browsing",
    "tab",
    "tabs",
    "page",
    "pages",
    "home",
    "file",
    "edit",
    "view",
    "with",
    "from",
    "this",
    "that",
    "your",
    "into",
    "about",
    "slack",
    "notion",
    "figma",
    "zoom",
    "meet",
    "docs",
    "sheets",
    "slides",
    "gmail",
    "mail",
    "inbox",
    "channel",
    "code",
    "claude",
    "visual",
    "studio",
    "terminal",
    "alacritty",
    "kitty",
    "konsole",
    "gnome",
];

/// How long an eject may precede the assign that recovers its range.
const EJECT_PAIR_MS: i64 = 120_000;
/// Longest focus span the sessionizer emits, for the sorted-scan skip.
const MAX_SPAN_MS: i64 = 6 * 3_600_000;

/// One evidence key: a typed anchor or a title term.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Anchor(AnchorKind, String),
    Term(String),
}

impl Key {
    /// How much a never-seen value of this kind argues for a new task: a
    /// new item, change, branch or calendar entry does; a new repo or
    /// document somewhat (a page is often the task, for people whose work
    /// is pages); a new tool session much less (people start sessions
    /// inside one task all day); a new person or site not at all.
    pub fn novelty(&self) -> f64 {
        match self {
            Key::Anchor(k, _) => match k {
                AnchorKind::Item | AnchorKind::Change | AnchorKind::Branch | AnchorKind::Event => {
                    1.0
                }
                AnchorKind::Place | AnchorKind::Doc => 0.5,
                AnchorKind::Session => 0.25,
                AnchorKind::People | AnchorKind::Domain => 0.0,
            },
            Key::Term(_) => 0.0,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Key::Anchor(k, _) => k.as_str(),
            Key::Term(_) => "term",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            Key::Anchor(_, v) | Key::Term(v) => v,
        }
    }

    pub fn parse(kind: &str, value: &str) -> Option<Key> {
        if kind == "term" {
            return Some(Key::Term(value.to_owned()));
        }
        AnchorKind::parse(kind).map(|k| Key::Anchor(k, value.to_owned()))
    }

    /// Score weight of a full match on this key.
    pub fn weight(&self) -> f64 {
        match self {
            Key::Anchor(k, _) => strength_weight(k.strength()),
            Key::Term(_) => TERM_WEIGHT,
        }
    }

    pub fn is_term(&self) -> bool {
        matches!(self, Key::Term(_))
    }
}

/// A focus span with its stored anchors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredSpan {
    pub id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub app: String,
    pub title: String,
    pub anchors: Vec<Anchor>,
}

/// Lower-case title terms worth matching on: at least [`MIN_TERM_CHARS`],
/// containing a letter, not the app's own name, not window furniture.
pub fn terms(app: &str, title: &str) -> Vec<String> {
    let app = app.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for word in title.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.chars().count() < MIN_TERM_CHARS || !word.chars().any(char::is_alphabetic) {
            continue;
        }
        let word = word.to_lowercase();
        if word == app || TERM_STOP.contains(&word.as_str()) || out.contains(&word) {
            continue;
        }
        out.push(word);
    }
    out
}

/// Every key one span carries.
pub fn span_keys(span: &AnchoredSpan) -> Vec<Key> {
    let mut keys: Vec<Key> = span
        .anchors
        .iter()
        .map(|a| Key::Anchor(a.kind, a.value.clone()))
        .collect();
    keys.extend(terms(&span.app, &span.title).into_iter().map(Key::Term));
    keys
}

/// Tunables. Defaults are the plan's starting values; the replay and the
/// persona fixtures say whether they hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    /// Minutes on a value before it counts as a full match.
    pub saturate_min: f64,
    /// Interval evidence halves every this many days.
    pub half_life_days: f64,
    /// A correction on a range counts as this many minutes of evidence.
    pub correction_min: f64,
    /// A declaration (label key, project) counts as this many minutes.
    pub declared_min: f64,
    /// What "new task" scores.
    pub new_task: f64,
    /// Margin the winner needs over the runner-up to be confident.
    pub delta: f64,
    /// A task touched within this long before the segment gets the bonus.
    pub recency_ms: i64,
    pub recency_bonus: f64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            saturate_min: 10.0,
            half_life_days: 14.0,
            correction_min: 10.0,
            declared_min: 10.0,
            new_task: 0.35,
            delta: 0.25,
            recency_ms: 2 * 3_600_000,
            recency_bonus: 0.05,
        }
    }
}

/// Where an evidence row came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Interval,
    Correction,
    Declared,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Interval => "interval",
            Source::Correction => "correction",
            Source::Declared => "declared",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "interval" => Some(Source::Interval),
            "correction" => Some(Source::Correction),
            "declared" => Some(Source::Declared),
            _ => None,
        }
    }
}

/// One row of `task_evidence`: minutes of evidence (decayed, may be
/// negative after an eject or a reassign away) for a key under a task.
#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceRow {
    pub task_id: i64,
    pub key: Key,
    pub minutes: f64,
    pub first_ts: i64,
    pub last_ts: i64,
    pub source: Source,
}

/// A task's accumulated evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub task_id: i64,
    /// Decayed minutes per key across sources.
    pub minutes: HashMap<Key, f64>,
    /// End of the task's latest interval, if any.
    pub last_ts: Option<i64>,
}

impl Profile {
    /// Aggregate rows (any order, any task mix) into one profile per task.
    /// `last_ts` comes from `intervals`.
    pub fn from_rows(rows: &[EvidenceRow], intervals: &[IntervalRow]) -> Vec<Profile> {
        let mut by_task: BTreeMap<i64, Profile> = BTreeMap::new();
        for r in rows {
            let p = by_task.entry(r.task_id).or_insert_with(|| Profile {
                task_id: r.task_id,
                minutes: HashMap::new(),
                last_ts: None,
            });
            *p.minutes.entry(r.key.clone()).or_insert(0.0) += r.minutes;
        }
        for iv in intervals {
            if let Some(p) = by_task.get_mut(&iv.task_id) {
                p.last_ts = Some(p.last_ts.map_or(iv.end_ts, |l| l.max(iv.end_ts)));
            }
        }
        by_task.into_values().collect()
    }

    /// Saturation of a key: 0 (none or negative) to 1 (a full match).
    pub fn sat(&self, key: &Key, p: &Params) -> f64 {
        let m = self.minutes.get(key).copied().unwrap_or(0.0);
        (m / p.saturate_min.max(f64::EPSILON)).clamp(0.0, 1.0)
    }
}

fn overlap_ms(a: (i64, i64), b: (i64, i64)) -> i64 {
    (a.1.min(b.1) - a.0.max(b.0)).max(0)
}

fn minutes(ms: i64) -> f64 {
    ms as f64 / 60_000.0
}

/// Minutes of each key across the spans overlapping `range`.
fn keys_in(spans: &[AnchoredSpan], range: (i64, i64)) -> HashMap<Key, (f64, i64, i64)> {
    let mut out: HashMap<Key, (f64, i64, i64)> = HashMap::new();
    // Spans are sorted by start; skip straight to the first that can
    // overlap. (A span longer than any before it could start earlier and
    // still overlap; focus spans are minutes long, so the miss is bounded
    // by that.)
    let first = spans.partition_point(|s| s.start_ts < range.0 - MAX_SPAN_MS);
    for s in &spans[first..] {
        if s.end_ts <= range.0 {
            continue;
        }
        if s.start_ts >= range.1 {
            break;
        }
        let ms = overlap_ms((s.start_ts, s.end_ts), range);
        if ms <= 0 {
            continue;
        }
        let lo = s.start_ts.max(range.0);
        let hi = s.end_ts.min(range.1);
        for k in span_keys(s) {
            let e = out.entry(k).or_insert((0.0, lo, hi));
            e.0 += minutes(ms);
            e.1 = e.1.min(lo);
            e.2 = e.2.max(hi);
        }
    }
    out
}

/// Evidence rows for every task as of `before_ts`: intervals that ended by
/// then (decayed by age), corrections made by then, and each task's own
/// label keys and project. `spans` must be sorted by `start_ts`. Tasks
/// closed by `before_ts` get no rows.
pub fn build_evidence(
    tasks: &[TaskRow],
    intervals: &[IntervalRow],
    spans: &[AnchoredSpan],
    corrections: &[CorrectionRow],
    ticket_re: &Regex,
    before_ts: i64,
    p: &Params,
) -> Vec<EvidenceRow> {
    let live: HashMap<i64, &TaskRow> = tasks
        .iter()
        .filter(|t| t.created_ts < before_ts && t.closed_ts.is_none_or(|c| c > before_ts))
        .map(|t| (t.id, t))
        .collect();
    let mut acc: HashMap<(i64, Key, Source), (f64, i64, i64)> = HashMap::new();
    let mut add = |task_id: i64, key: Key, source: Source, m: f64, lo: i64, hi: i64| {
        let e = acc.entry((task_id, key, source)).or_insert((0.0, lo, hi));
        e.0 += m;
        e.1 = e.1.min(lo);
        e.2 = e.2.max(hi);
    };
    let decay = |end_ts: i64| -> f64 {
        let age_days = (before_ts - end_ts).max(0) as f64 / 86_400_000.0;
        0.5f64.powf(age_days / p.half_life_days)
    };

    for iv in intervals.iter().filter(|iv| iv.end_ts <= before_ts) {
        if !live.contains_key(&iv.task_id) {
            continue;
        }
        let d = decay(iv.end_ts);
        for (k, (m, lo, hi)) in keys_in(spans, (iv.start_ts, iv.end_ts)) {
            add(iv.task_id, k, Source::Interval, m * d, lo, hi);
        }
    }

    let interval = |id: i64| intervals.iter().find(|iv| iv.id == id);
    // The task a reassign moved time away from, by its label then: the one
    // live task with that label, else the one closed task.
    let task_by_label = |label: &str| {
        let one = |closed: bool| {
            let mut hits = tasks
                .iter()
                .filter(|t| t.label == label && live.contains_key(&t.id) != closed);
            match (hits.next(), hits.next()) {
                (Some(t), None) => Some(t.id),
                _ => None,
            }
        };
        one(false).or_else(|| one(true))
    };
    // A correction counts as `correction_min` minutes spread over the
    // range's keys by their share of it.
    let mut bonus = |task_id: i64, range: (i64, i64), sign: f64, ts: i64| {
        let total = minutes(range.1 - range.0);
        if total <= 0.0 || !live.contains_key(&task_id) {
            return;
        }
        for (k, (m, _, _)) in keys_in(spans, range) {
            let share = (m / total).min(1.0);
            add(
                task_id,
                k,
                Source::Correction,
                sign * p.correction_min * share,
                ts,
                ts,
            );
        }
    };
    for c in corrections.iter().filter(|c| c.ts <= before_ts) {
        match c.kind.as_str() {
            "assign" | "reassign" => {
                let Some(iv) = c.interval_id.and_then(interval) else {
                    continue;
                };
                bonus(c.task_id, (iv.start_ts, iv.end_ts), 1.0, c.ts);
                if c.kind == "reassign"
                    && let Some(from) = task_by_label(&c.old_label)
                {
                    bonus(from, (iv.start_ts, iv.end_ts), -1.0, c.ts);
                }
            }
            "eject" => {
                let pair = corrections.iter().find(|a| {
                    a.kind == "assign"
                        && a.ts >= c.ts
                        && a.ts - c.ts <= EJECT_PAIR_MS
                        && a.interval_id.is_some()
                });
                let Some(iv) = pair.and_then(|a| a.interval_id).and_then(interval) else {
                    continue;
                };
                bonus(c.task_id, (iv.start_ts, iv.end_ts), -1.0, c.ts);
            }
            _ => {}
        }
    }

    // Item values seen anywhere, for labels that carry only the number
    // ("11342: storing images" names ACME-11342).
    let items: Vec<&str> = {
        let mut v: Vec<&str> = spans
            .iter()
            .filter(|s| s.end_ts <= before_ts)
            .flat_map(|s| s.anchors.iter())
            .filter(|a| a.kind == AnchorKind::Item)
            .map(|a| a.value.as_str())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    for t in live.values() {
        let (label, project) = crate::replay::label_at(t, before_ts, corrections);
        let mut declared: Vec<String> = ticket_re
            .find_iter(&label)
            .map(|m| m.as_str().to_owned())
            .collect();
        for num in label
            .split(|c: char| !c.is_ascii_digit())
            .filter(|n| n.len() >= 4)
        {
            declared.extend(
                items
                    .iter()
                    .filter(|it| it.rsplit_once('-').is_some_and(|(_, n)| n == num))
                    .map(|it| (*it).to_owned()),
            );
        }
        declared.sort_unstable();
        declared.dedup();
        for item in declared {
            add(
                t.id,
                Key::Anchor(AnchorKind::Item, item),
                Source::Declared,
                p.declared_min,
                t.created_ts,
                t.created_ts,
            );
        }
        if let Some(pr) = project.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            add(
                t.id,
                Key::Anchor(AnchorKind::Place, pr.to_owned()),
                Source::Declared,
                p.declared_min,
                t.created_ts,
                t.created_ts,
            );
        }
    }

    let mut rows: Vec<EvidenceRow> = acc
        .into_iter()
        .map(
            |((task_id, key, source), (minutes, first_ts, last_ts))| EvidenceRow {
                task_id,
                key,
                minutes,
                first_ts,
                last_ts,
                source,
            },
        )
        .collect();
    rows.sort_by(|a, b| (a.task_id, &a.key, a.source).cmp(&(b.task_id, &b.key, b.source)));
    rows
}

/// Profiles as of `before_ts`; see [`build_evidence`].
pub fn build_profiles(
    tasks: &[TaskRow],
    intervals: &[IntervalRow],
    spans: &[AnchoredSpan],
    corrections: &[CorrectionRow],
    ticket_re: &Regex,
    before_ts: i64,
    p: &Params,
) -> Vec<Profile> {
    let rows = build_evidence(
        tasks,
        intervals,
        spans,
        corrections,
        ticket_re,
        before_ts,
        p,
    );
    let before: Vec<IntervalRow> = intervals
        .iter()
        .filter(|iv| iv.end_ts <= before_ts)
        .cloned()
        .collect();
    let mut profiles = Profile::from_rows(&rows, &before);
    // Live tasks with no evidence still compete (at zero) and carry recency.
    for t in tasks
        .iter()
        .filter(|t| t.created_ts < before_ts && t.closed_ts.is_none_or(|c| c > before_ts))
    {
        if profiles.iter().all(|p| p.task_id != t.id) {
            let last_ts = before
                .iter()
                .filter(|iv| iv.task_id == t.id)
                .map(|iv| iv.end_ts)
                .max();
            profiles.push(Profile {
                task_id: t.id,
                minutes: HashMap::new(),
                last_ts,
            });
        }
    }
    profiles.sort_by_key(|p| p.task_id);
    profiles
}

/// The evidence of one stretch of focus: minutes per key.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start_ts: i64,
    pub end_ts: i64,
    /// Focus minutes inside the range.
    pub minutes: f64,
    pub keys: HashMap<Key, f64>,
}

impl Segment {
    /// Keys of the spans overlapping `[lo, hi)`. `spans` sorted by start.
    pub fn from_spans(spans: &[AnchoredSpan], lo: i64, hi: i64) -> Segment {
        Self::from_spans_skipping(spans, lo, hi, &[])
    }

    /// [`Segment::from_spans`] with spans matching a distraction pattern
    /// left out: a video inside a work block is not evidence of anything.
    /// Their minutes still count toward the segment's length.
    pub fn from_spans_skipping(
        spans: &[AnchoredSpan],
        lo: i64,
        hi: i64,
        distractions: &[Regex],
    ) -> Segment {
        let focus: i64 = spans
            .iter()
            .map(|s| overlap_ms((s.start_ts, s.end_ts), (lo, hi)))
            .sum();
        let kept: Vec<AnchoredSpan> = spans
            .iter()
            .filter(|s| s.end_ts > lo && s.start_ts < hi)
            .filter(|s| !crate::evidence::is_distraction(&s.app, &s.title, distractions))
            .cloned()
            .collect();
        let keys = keys_in(&kept, (lo, hi))
            .into_iter()
            .map(|(k, (m, _, _))| (k, m))
            .collect();
        Segment {
            start_ts: lo,
            end_ts: hi,
            minutes: minutes(focus),
            keys,
        }
    }

    /// Strongest anchors by time, for naming a new cluster: up to `n`
    /// strong/medium values, strongest then longest first.
    pub fn describe(&self, n: usize) -> String {
        let mut v: Vec<(&Key, f64)> = self
            .keys
            .iter()
            .filter(
                |(k, _)| matches!(k, Key::Anchor(kind, _) if kind.strength() >= Strength::Medium),
            )
            .map(|(k, m)| (k, *m))
            .collect();
        v.sort_by(|a, b| {
            b.0.weight()
                .partial_cmp(&a.0.weight())
                .unwrap()
                .then(b.1.partial_cmp(&a.1).unwrap())
                .then(a.0.cmp(b.0))
        });
        v.iter()
            .take(n)
            .map(|(k, _)| k.value())
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub task_id: i64,
    pub score: f64,
}

/// The scorer's answer for one segment.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// Tasks by score, best first. Only tasks with a profile appear.
    pub ranked: Vec<Candidate>,
    /// What "new task" scored.
    pub new_task: f64,
    /// `Some(task)` or `None` for a new task.
    pub best: Option<i64>,
    /// Best minus runner-up (new task counts as a runner).
    pub margin: f64,
    /// `margin >= delta`.
    pub confident: bool,
}

impl Verdict {
    /// The runner-up as a one-click alternative: the second task, or
    /// "new" (`None`) when it outranks the second task.
    pub fn runner_up(&self) -> Option<Option<i64>> {
        let second = self.ranked.get(1).map(|c| (Some(c.task_id), c.score));
        match (self.best, second) {
            (Some(_), Some((id, s))) if s >= self.new_task => Some(id),
            (Some(_), _) => Some(None),
            (None, _) => self.ranked.first().map(|c| Some(c.task_id)),
        }
    }
}

/// Saturation at which a key counts as "in" a profile when measuring how
/// many tasks share it.
const SHARED_SAT: f64 = 0.5;
/// Terms may carry at most this share of the hard evidence mass.
const TERM_SHARE: f64 = 0.3;
/// Ceiling of a score built from terms alone (a segment with no anchors).
const TERM_ONLY_CEIL: f64 = 0.6;

/// Score `seg` against `profiles`. A task's score is the fraction of the
/// segment's *known* evidence it explains: per key, `weight × discount ×
/// share of the segment × saturation`, over the same sum at saturation 1
/// across the keys at least one live profile carries. The discount is
/// `1 / n` for a key `n` profiles carry, so a place or branch every task in
/// a repo shares cannot decide between them while a key only one task has
/// can. Keys no profile has ever seen are novelty, weighted by
/// [`Key::novelty`]: a task's score is scaled by the known share of the
/// (undiscounted) evidence and "new task" scores the larger of
/// [`Params::new_task`] and the novel share, so a stretch of mostly new
/// items, branches or places is a new task while new pages inside a known
/// place are not. Terms are scaled to at most
/// [`TERM_SHARE`] of the hard mass (or [`TERM_ONLY_CEIL`] alone). Plus the
/// recency bonus. The winner is confident when its margin over the
/// runner-up (new task included) clears [`Params::delta`]; a segment with
/// no evidence at all is never confident.
pub fn score(seg: &Segment, profiles: &[Profile], p: &Params) -> Verdict {
    // (key, share, discount, known)
    let keys: Vec<(&Key, f64, f64, bool)> = if seg.minutes > 0.0 {
        seg.keys
            .iter()
            .map(|(k, m)| {
                let n = profiles
                    .iter()
                    .filter(|pr| pr.sat(k, p) >= SHARED_SAT)
                    .count();
                let known = profiles.iter().any(|pr| pr.sat(k, p) > 0.0);
                (k, (m / seg.minutes).min(1.0), 1.0 / n.max(1) as f64, known)
            })
            .collect()
    } else {
        Vec::new()
    };
    // Discounted masses rank tasks against each other; raw masses say how
    // much of the segment is novel.
    let mass = |term: bool, known: bool, discounted: bool| -> f64 {
        keys.iter()
            .filter(|(k, _, _, kn)| k.is_term() == term && *kn == known)
            .map(|(k, share, disc, _)| k.weight() * share * if discounted { *disc } else { 1.0 })
            .sum()
    };
    let hard_known = mass(false, true, true);
    let term_known = mass(true, true, true);
    let term_scale = if term_known <= 0.0 {
        0.0
    } else if hard_known > 0.0 {
        (TERM_SHARE * hard_known / term_known).min(1.0)
    } else {
        1.0
    };
    let denom = hard_known + term_scale * term_known;
    let raw_known = mass(false, true, false);
    let raw_novel: f64 = keys
        .iter()
        .filter(|(k, _, _, known)| !k.is_term() && !known)
        .map(|(k, share, _, _)| k.weight() * k.novelty() * share)
        .sum();
    let known_share = if raw_known + raw_novel > 0.0 {
        raw_known / (raw_known + raw_novel)
    } else {
        1.0
    };
    let new_task = p.new_task.max(1.0 - known_share);
    let mut ranked: Vec<Candidate> = profiles
        .iter()
        .map(|pr| {
            let mut num = 0.0;
            for (k, share, disc, _) in &keys {
                let sat = pr.sat(k, p);
                if sat <= 0.0 {
                    continue;
                }
                let scale = if k.is_term() { term_scale } else { 1.0 };
                num += k.weight() * disc * share * sat * scale;
            }
            let mut score = if denom > 0.0 {
                num / denom * known_share
            } else {
                0.0
            };
            if hard_known <= 0.0 {
                score *= TERM_ONLY_CEIL;
            }
            let recent = pr
                .last_ts
                .is_some_and(|l| l <= seg.start_ts && seg.start_ts - l <= p.recency_ms);
            if recent {
                score += p.recency_bonus;
            }
            Candidate {
                task_id: pr.task_id,
                score,
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap()
            .then(a.task_id.cmp(&b.task_id))
    });
    let top = ranked.first().map(|c| c.score).unwrap_or(0.0);
    let (best, margin) = if top > new_task {
        let runner = ranked.get(1).map(|c| c.score).unwrap_or(0.0).max(new_task);
        (ranked.first().map(|c| c.task_id), top - runner)
    } else {
        (None, new_task - top)
    };
    let empty = keys.iter().all(|(k, ..)| k.is_term());
    Verdict {
        ranked,
        new_task,
        best,
        margin,
        confident: !empty && margin >= p.delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn re() -> Regex {
        Regex::new("[A-Z][A-Z0-9]+-[0-9]+").unwrap()
    }

    fn task(id: i64, label: &str, created_ts: i64) -> TaskRow {
        TaskRow {
            id,
            label: label.into(),
            project: None,
            declared: false,
            closed: false,
            created_ts,
            closed_ts: None,
        }
    }

    fn iv(id: i64, task_id: i64, lo: i64, hi: i64) -> IntervalRow {
        IntervalRow {
            id,
            task_id,
            batch_id: Some(1),
            start_ts: lo,
            end_ts: hi,
            origin_task_id: Some(task_id),
        }
    }

    fn span(
        id: i64,
        lo: i64,
        hi: i64,
        title: &str,
        anchors: &[(AnchorKind, &str)],
    ) -> AnchoredSpan {
        AnchoredSpan {
            id,
            start_ts: lo,
            end_ts: hi,
            app: "code".into(),
            title: title.into(),
            anchors: anchors
                .iter()
                .map(|(k, v)| Anchor {
                    kind: *k,
                    value: (*v).to_owned(),
                })
                .collect(),
        }
    }

    fn corr(
        id: i64,
        ts: i64,
        kind: &str,
        task_id: i64,
        old: &str,
        interval_id: Option<i64>,
    ) -> CorrectionRow {
        CorrectionRow {
            id,
            ts,
            kind: kind.into(),
            task_id,
            old_label: old.into(),
            new_label: String::new(),
            old_project: None,
            new_project: None,
            interval_id,
        }
    }

    #[test]
    fn terms_drop_furniture_and_short_words() {
        let t = terms(
            "code",
            "ACME-11382 fix billing — Visual Studio Code - Google Chrome",
        );
        assert_eq!(t, vec!["acme", "billing"]);
    }

    #[test]
    fn item_beats_place_and_place_alone_is_unsure() {
        let tasks = vec![task(1, "ACME-1 billing", 0), task(2, "ACME-2 emails", 0)];
        let spans = vec![
            span(
                1,
                0,
                20 * MIN,
                "billing.rs - app",
                &[(AnchorKind::Place, "app"), (AnchorKind::Item, "ACME-1")],
            ),
            span(
                2,
                20 * MIN,
                40 * MIN,
                "mail.rs - app",
                &[(AnchorKind::Place, "app"), (AnchorKind::Item, "ACME-2")],
            ),
            span(
                3,
                60 * MIN,
                70 * MIN,
                "billing.rs - app",
                &[(AnchorKind::Place, "app"), (AnchorKind::Item, "ACME-1")],
            ),
            span(
                4,
                70 * MIN,
                80 * MIN,
                "readme - app",
                &[(AnchorKind::Place, "app")],
            ),
        ];
        let ivs = vec![iv(1, 1, 0, 20 * MIN), iv(2, 2, 20 * MIN, 40 * MIN)];
        let p = Params::default();
        let profiles = build_profiles(&tasks, &ivs, &spans, &[], &re(), 60 * MIN, &p);
        assert_eq!(profiles.len(), 2);

        let seg = Segment::from_spans(&spans, 60 * MIN, 70 * MIN);
        let v = score(&seg, &profiles, &p);
        assert_eq!(v.best, Some(1), "{v:?}");
        assert!(v.confident, "{v:?}");

        let seg = Segment::from_spans(&spans, 70 * MIN, 80 * MIN);
        let v = score(&seg, &profiles, &p);
        // Both tasks share the place; neither clears the margin.
        assert!(!v.confident, "{v:?}");
        assert_eq!(v.ranked.len(), 2);
    }

    #[test]
    fn unknown_evidence_is_a_new_task() {
        let tasks = vec![task(1, "ACME-1 billing", 0)];
        let spans = vec![
            span(
                1,
                0,
                20 * MIN,
                "billing.rs - app",
                &[(AnchorKind::Place, "app"), (AnchorKind::Item, "ACME-1")],
            ),
            span(
                2,
                60 * MIN,
                80 * MIN,
                "Q4 pricing page - Notion",
                &[(AnchorKind::Doc, "Q4 pricing page")],
            ),
        ];
        let ivs = vec![iv(1, 1, 0, 20 * MIN)];
        let p = Params::default();
        let profiles = build_profiles(&tasks, &ivs, &spans, &[], &re(), 60 * MIN, &p);
        let seg = Segment::from_spans(&spans, 60 * MIN, 80 * MIN);
        let v = score(&seg, &profiles, &p);
        assert_eq!(v.best, None, "{v:?}");
        assert!(v.confident, "{v:?}");
        assert_eq!(seg.describe(3), "Q4 pricing page");
    }

    #[test]
    fn eject_and_reassign_write_negative_rows() {
        let tasks = vec![task(1, "billing", 0), task(2, "emails", 0)];
        let spans = vec![span(
            1,
            0,
            20 * MIN,
            "mail.rs - app",
            &[(AnchorKind::Doc, "mail.rs")],
        )];
        // The range sits under task 2 today; it was ejected from task 1 and
        // assigned to 2 seconds later.
        let ivs = vec![iv(7, 2, 0, 20 * MIN)];
        let corrections = vec![
            corr(1, 30 * MIN, "eject", 1, "billing", None),
            corr(2, 30 * MIN + 5_000, "assign", 2, "(unassigned)", Some(7)),
        ];
        let p = Params::default();
        let rows = build_evidence(&tasks, &ivs, &spans, &corrections, &re(), 60 * MIN, &p);
        let doc = Key::Anchor(AnchorKind::Doc, "mail.rs".into());
        let neg = rows
            .iter()
            .find(|r| r.task_id == 1 && r.key == doc && r.source == Source::Correction)
            .expect("negative row on the ejected task");
        assert!(neg.minutes < 0.0, "{neg:?}");
        let pos = rows
            .iter()
            .find(|r| r.task_id == 2 && r.key == doc && r.source == Source::Correction)
            .expect("bonus on the target");
        assert!(pos.minutes > 0.0, "{pos:?}");
        let profiles = Profile::from_rows(&rows, &ivs);
        assert_eq!(
            profiles
                .iter()
                .find(|p| p.task_id == 1)
                .unwrap()
                .sat(&doc, &p),
            0.0
        );
    }

    #[test]
    fn declared_keys_and_decay() {
        let mut t = task(1, "ACME-9 checkout", 0);
        t.project = Some("shop".into());
        let tasks = vec![t];
        let day = 86_400_000;
        let spans = vec![span(
            1,
            0,
            10 * MIN,
            "x - shop",
            &[(AnchorKind::Place, "shop")],
        )];
        let ivs = vec![iv(1, 1, 0, 10 * MIN)];
        let p = Params::default();
        let rows = build_evidence(&tasks, &ivs, &spans, &[], &re(), 14 * day, &p);
        let item = rows
            .iter()
            .find(|r| r.key == Key::Anchor(AnchorKind::Item, "ACME-9".into()))
            .unwrap();
        assert_eq!(item.source, Source::Declared);
        assert_eq!(item.minutes, p.declared_min);
        let place = rows
            .iter()
            .find(|r| {
                r.key == Key::Anchor(AnchorKind::Place, "shop".into())
                    && r.source == Source::Interval
            })
            .unwrap();
        // 10 minutes, one half-life old.
        assert!((place.minutes - 5.0).abs() < 0.01, "{place:?}");
    }

    #[test]
    fn closed_tasks_do_not_compete() {
        let mut t = task(1, "old", 0);
        t.closed_ts = Some(30 * MIN);
        let tasks = vec![t, task(2, "new", 0)];
        let spans = vec![span(1, 0, 10 * MIN, "x", &[(AnchorKind::Doc, "x")])];
        let ivs = vec![iv(1, 1, 0, 10 * MIN)];
        let p = Params::default();
        let profiles = build_profiles(&tasks, &ivs, &spans, &[], &re(), 60 * MIN, &p);
        assert_eq!(
            profiles.iter().map(|p| p.task_id).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn runner_up_prefers_new_when_second_is_weak() {
        let v = Verdict {
            ranked: vec![
                Candidate {
                    task_id: 1,
                    score: 0.9,
                },
                Candidate {
                    task_id: 2,
                    score: 0.1,
                },
            ],
            new_task: 0.35,
            best: Some(1),
            margin: 0.55,
            confident: true,
        };
        assert_eq!(v.runner_up(), Some(None));
    }
}
