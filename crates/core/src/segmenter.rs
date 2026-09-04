//! Deterministic segmentation over anchored spans (m30 chunk 3): a segment
//! runs while its strong/medium anchors — or its distinctive title words —
//! keep recurring, with hysteresis so a chat glance folds into the
//! surrounding work; each segment is scored against the live task profiles
//! and written as a `segment` interval, no model in the loop. Stretches the
//! scorer calls new are clustered by what they share and become one task
//! per cluster once there is enough of them; the model only names them.
//!
//! Behind `Config::derive_mode = "segmenter"`; the default stays the m27
//! model path until the replay gate says otherwise.

use std::collections::{BTreeSet, HashMap};

use jiff::Timestamp;
use regex::Regex;
use rusqlite::Connection;

use crate::config::Config;
use crate::extract::{AnchorKind, Family, Strength};
use crate::profile::{self, AnchoredSpan, Key, Params, Profile, Segment, Verdict};
use crate::storage::{self, StorageError};
use crate::types::ts_to_ms;

/// Never look further back than this when no batch has derived yet.
const MAX_WINDOW_MS: i64 = 12 * 3_600_000;
/// A pause between focus spans at least this long ends a segment (the M5
/// time-honesty rule: AFK is never inside a task block).
pub const AFK_GAP_MS: i64 = 5 * 60_000;
/// Shortest title word that can carry a segment from one document to the
/// next; four-letter words tie too much.
const MIN_TIE_CHARS: usize = 5;
/// Title words that name tools and furniture, never the work, on top of
/// [`profile::terms`]'s own list.
const TIE_STOP: &[&str] = &[
    "docs",
    "sheets",
    "slides",
    "drive",
    "notion",
    "figma",
    "slack",
    "linear",
    "jira",
    "github",
    "gitlab",
    "issue",
    "issues",
    "request",
    "pages",
    "dashboard",
    "untitled",
    "channel",
    "inbox",
    "search",
    "results",
    "account",
    "settings",
    "general",
    "meeting",
    "document",
    "spreadsheet",
    "presentation",
    "workspace",
];

/// Segmentation tunables, from [`Config`].
#[derive(Debug, Clone, PartialEq)]
pub struct SegParams {
    /// A run of spans sharing nothing with the segment must reach this many
    /// minutes before it becomes a segment of its own; shorter excursions
    /// fold back in.
    pub switch_min: f64,
    /// A cluster of "new" stretches needs this many focus minutes before
    /// a task is created for it; less stays unassigned.
    pub new_task_min: f64,
}

impl SegParams {
    pub fn from_config(c: &Config) -> Self {
        SegParams {
            switch_min: f64::from(c.segment_switch_min.max(1)),
            new_task_min: f64::from(c.segment_new_task_min),
        }
    }
}

impl Default for SegParams {
    fn default() -> Self {
        SegParams {
            switch_min: 3.0,
            new_task_min: 10.0,
        }
    }
}

/// What one span says about where it belongs.
#[derive(Debug, Clone, Default, PartialEq)]
struct Signature {
    /// Strong and medium anchors.
    keys: Vec<Key>,
    /// Distinctive title words.
    ties: Vec<String>,
}

impl Signature {
    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.ties.is_empty()
    }
}

/// One stretch of focus the segmenter cut, `[lo, hi)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub lo: i64,
    pub hi: i64,
    /// Focus minutes per strong/medium anchor key.
    pub keys: HashMap<Key, f64>,
    /// Focus minutes per distinctive title word.
    pub ties: HashMap<String, f64>,
    /// Focus minutes inside the segment (distractions included).
    pub minutes: f64,
    /// `false` for the last segment, still growing at the window's end.
    pub closed: bool,
}

impl Seg {
    fn empty(lo: i64) -> Seg {
        Seg {
            lo,
            hi: lo,
            keys: HashMap::new(),
            ties: HashMap::new(),
            minutes: 0.0,
            closed: true,
        }
    }

    fn add(&mut self, sig: &Signature, minutes: f64, end: i64) {
        for k in &sig.keys {
            *self.keys.entry(k.clone()).or_insert(0.0) += minutes;
        }
        for t in &sig.ties {
            *self.ties.entry(t.clone()).or_insert(0.0) += minutes;
        }
        self.minutes += minutes;
        self.hi = self.hi.max(end);
    }

    fn absorb(&mut self, other: &Seg) {
        for (k, m) in &other.keys {
            *self.keys.entry(k.clone()).or_insert(0.0) += m;
        }
        for (t, m) in &other.ties {
            *self.ties.entry(t.clone()).or_insert(0.0) += m;
        }
        self.minutes += other.minutes;
        self.lo = self.lo.min(other.lo);
        self.hi = self.hi.max(other.hi);
    }

    /// Whether a span continues this segment: a shared anchor, or a shared
    /// word among the segment's leading ones.
    fn shares(&self, sig: &Signature) -> bool {
        if sig.keys.iter().any(|k| self.keys.contains_key(k)) {
            return true;
        }
        let top = self.ties.values().cloned().fold(0.0, f64::max);
        sig.ties
            .iter()
            .any(|t| self.ties.get(t).is_some_and(|m| *m >= top * 0.25))
    }

    /// Anchor keys the segment has spent at least a quarter of its anchored
    /// time on: what it is "about", for the excursion rule.
    fn dominant(&self) -> BTreeSet<&Key> {
        let top = self.keys.values().cloned().fold(0.0, f64::max);
        self.keys
            .iter()
            .filter(|(_, m)| **m >= top * 0.25)
            .map(|(k, _)| k)
            .collect()
    }

    /// Whether two segments are about the same thing: a dominant anchor in
    /// common, or a leading word.
    fn akin(&self, other: &Seg) -> bool {
        if self
            .dominant()
            .intersection(&other.dominant())
            .next()
            .is_some()
        {
            return true;
        }
        let lead = |s: &Seg| -> BTreeSet<String> {
            let top = s.ties.values().cloned().fold(0.0, f64::max);
            s.ties
                .iter()
                .filter(|(_, m)| **m >= top * 0.25)
                .map(|(t, _)| t.clone())
                .collect()
        };
        lead(self).intersection(&lead(other)).next().is_some()
    }
}

fn minutes(ms: i64) -> f64 {
    ms as f64 / 60_000.0
}

fn signature(span: &AnchoredSpan) -> Signature {
    let mut keys: Vec<Key> = span
        .anchors
        .iter()
        .filter(|a| a.kind.strength() >= Strength::Medium)
        .map(|a| Key::Anchor(a.kind, a.value.clone()))
        .collect();
    keys.sort();
    keys.dedup();
    // Words tie one document to the next; a span with no anchor at all (a
    // chat window, a plain terminal) has nothing to tie and stretches
    // whatever is open instead.
    let ties = if keys.is_empty() {
        Vec::new()
    } else {
        profile::terms(&span.app, &span.title)
            .into_iter()
            .filter(|t| t.chars().count() >= MIN_TIE_CHARS && !TIE_STOP.contains(&t.as_str()))
            .collect()
    };
    Signature { keys, ties }
}

fn strong_kind(k: &Key) -> Option<AnchorKind> {
    match k {
        Key::Anchor(kind, _) if kind.strength() == Strength::Strong => Some(*kind),
        _ => None,
    }
}

/// Cut `spans` (focus, sorted by start) into segments.
///
/// A span joins the open segment when it shares an anchor or a leading
/// word with it. A span that swaps a strong anchor the segment holds for
/// another of the same kind (branch for branch, item for item, session for
/// session) cuts at once, once the segment is `switch_min` long. A span
/// sharing nothing starts a *foreign run*; the segment closes at the run's
/// start once the run holds `switch_min` focus minutes. Spans with nothing
/// to say (no anchor, no word) and distraction spans stretch whatever is
/// open. A gap of [`AFK_GAP_MS`] closes the segment. Afterwards a segment
/// shorter than `switch_min` between two about the same thing folds into
/// them (the glance-at-chat rule).
pub fn segment(spans: &[AnchoredSpan], distractions: &[Regex], p: &SegParams) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    let mut cur: Option<Seg> = None;
    // The foreign run since the last span that matched the open segment.
    let mut run: Option<Seg> = None;

    for s in spans {
        let dur = minutes(s.end_ts - s.start_ts);
        let sig = signature(s);
        let distraction = crate::evidence::is_distraction(&s.app, &s.title, distractions);

        if let Some(c) = cur.as_mut()
            && s.start_ts - c.hi >= AFK_GAP_MS
        {
            out.push(cur.take().expect("checked"));
            run = None;
        }
        let Some(c) = cur.as_mut() else {
            let mut c = Seg::empty(s.start_ts);
            if distraction {
                c.minutes += dur;
                c.hi = s.end_ts;
            } else {
                c.add(&sig, dur, s.end_ts);
            }
            cur = Some(c);
            continue;
        };

        if distraction || sig.is_empty() {
            c.minutes += dur;
            c.hi = c.hi.max(s.end_ts);
            continue;
        }
        // A strong anchor swapped for another of its kind is a switch even
        // when the place or the words stay the same.
        let swaps_strong = sig.keys.iter().any(|k| {
            strong_kind(k).is_some_and(|kind| {
                !c.keys.contains_key(k) && c.keys.keys().any(|held| strong_kind(held) == Some(kind))
            })
        });
        let shares_strong = sig
            .keys
            .iter()
            .any(|k| strong_kind(k).is_some() && c.keys.contains_key(k));
        if shares_strong || (!swaps_strong && c.shares(&sig)) {
            c.add(&sig, dur, s.end_ts);
            run = None;
            continue;
        }
        if swaps_strong && c.minutes >= p.switch_min && run.is_none() {
            let mut next = Seg::empty(s.start_ts);
            next.add(&sig, dur, s.end_ts);
            let mut done = std::mem::replace(c, next);
            done.hi = done.hi.min(s.start_ts);
            out.push(done);
            continue;
        }
        // Foreign to the segment: the run grows until it earns a cut.
        let r = run.get_or_insert_with(|| Seg::empty(s.start_ts));
        r.add(&sig, dur, s.end_ts);
        if r.minutes >= p.switch_min {
            let start = r.lo;
            let next = run.take().expect("just inserted");
            let mut done = std::mem::replace(c, next);
            done.hi = done.hi.min(start);
            // The run's own minutes were counted on the open segment while
            // it could still fold back; they belong to the new one now.
            done.minutes = (done.minutes - (c.minutes - dur)).max(0.0);
            out.push(done);
        } else {
            // Tentatively part of the open segment (time only, not what it
            // is about), in case it folds back.
            c.minutes += dur;
            c.hi = c.hi.max(s.end_ts);
        }
    }
    if let Some(mut c) = cur.take() {
        c.closed = false;
        out.push(c);
    }
    out.retain(|s| s.hi > s.lo);
    fold_excursions(out, p)
}

/// A short segment between two about the same thing joins them.
fn fold_excursions(segs: Vec<Seg>, p: &SegParams) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::with_capacity(segs.len());
    let mut i = 0;
    while i < segs.len() {
        let s = &segs[i];
        let short = s.minutes < p.switch_min && s.closed;
        if short && !out.is_empty() && i + 1 < segs.len() {
            let prev = out.last().expect("non-empty");
            let next = &segs[i + 1];
            if prev.akin(next) && next.lo - prev.hi < AFK_GAP_MS {
                let mut merged = out.pop().expect("non-empty");
                merged.absorb(s);
                merged.absorb(next);
                merged.closed = next.closed;
                out.push(merged);
                i += 2;
                continue;
            }
        }
        out.push(s.clone());
        i += 1;
    }
    out
}

/// Where a scored segment goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Existing(i64),
    /// A task to create: label, project, and the cluster it belongs to —
    /// every placement with the same cluster shares one new task.
    New {
        label: String,
        project: Option<String>,
        cluster: usize,
    },
}

/// One segment, scored: what the window write stores.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub lo: i64,
    pub hi: i64,
    pub target: Target,
    /// The winner's score (or "new task"'s).
    pub confidence: f64,
    pub confident: bool,
    pub reason: String,
    /// Winner minus runner-up, for the verdict log.
    pub margin: f64,
    /// The task that came second (for a new-task verdict, the best task).
    pub runner_up: Option<i64>,
    /// The kind of work: see [`kind_of`].
    pub kind: String,
}

/// The kinds of work a segment can be, general across roles.
pub const KINDS: [&str; 9] = [
    "author",
    "agent",
    "review",
    "communicate",
    "meet",
    "plan",
    "read",
    "admin",
    "break",
];

/// One span's kind from its app family and anchors: an editor, design or
/// office window is `author`; a terminal with an agent session `agent`,
/// without one `author`; a change page or a git GUI `review`; a tracker
/// page (item, no change) `plan`; a calendar entry `meet`, as is a meeting
/// app; chat and mail `communicate`; a document or site with nothing more
/// `read`; a distraction `break`.
fn span_kind(span: &AnchoredSpan, distractions: &[Regex]) -> &'static str {
    if crate::evidence::is_distraction(&span.app, &span.title, distractions) {
        return "break";
    }
    let has = |k: AnchorKind| span.anchors.iter().any(|a| a.kind == k);
    if has(AnchorKind::Event) {
        return "meet";
    }
    match crate::extract::family(&span.app) {
        Family::Editor | Family::Document => "author",
        Family::Terminal if has(AnchorKind::Session) => "agent",
        Family::Terminal => "author",
        Family::Vcs => "review",
        Family::Chat | Family::Mail => "communicate",
        Family::Meeting => "meet",
        Family::Browser | Family::Other => {
            if has(AnchorKind::Change) {
                "review"
            } else if has(AnchorKind::Item) {
                "plan"
            } else {
                "read"
            }
        }
    }
}

/// The kind of work over `[lo, hi)`: the kind its spans spent most time
/// on, `break` only when nothing else is there.
pub fn kind_of(spans: &[AnchoredSpan], lo: i64, hi: i64, distractions: &[Regex]) -> &'static str {
    let mut ms: HashMap<&'static str, i64> = HashMap::new();
    for s in spans.iter().filter(|s| s.end_ts > lo && s.start_ts < hi) {
        let ov = s.end_ts.min(hi) - s.start_ts.max(lo);
        if ov > 0 {
            *ms.entry(span_kind(s, distractions)).or_insert(0) += ov;
        }
    }
    let work = ms
        .iter()
        .filter(|(k, _)| **k != "break")
        .max_by_key(|(k, m)| (**m, std::cmp::Reverse(**k)))
        .map(|(k, _)| *k);
    work.unwrap_or(if ms.is_empty() { "read" } else { "break" })
}

fn label_of(labels: &HashMap<i64, String>, id: i64) -> String {
    labels
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("task {id}"))
}

/// One segment's verdict as a placement on an existing task, or `None`
/// when the scorer calls it new (clustering decides those).
fn place_existing(seg: &Segment, v: &Verdict, labels: &HashMap<i64, String>) -> Option<Placement> {
    let id = v.best?;
    let reason = if v.confident {
        let d = seg.describe(2);
        if d.is_empty() {
            "evidence".to_owned()
        } else {
            d
        }
    } else {
        match v.runner_up() {
            Some(Some(other)) => {
                format!(
                    "between {} and {}",
                    label_of(labels, id),
                    label_of(labels, other)
                )
            }
            _ => format!("between {} and a new task", label_of(labels, id)),
        }
    };
    Some(Placement {
        lo: seg.start_ts,
        hi: seg.end_ts,
        target: Target::Existing(id),
        confidence: v.ranked.first().map_or(0.0, |c| c.score),
        confident: v.confident,
        reason,
        margin: v.margin,
        runner_up: v.runner_up().flatten(),
        kind: String::new(),
    })
}

/// Decide every segment in `spans` over `[lo, hi)`: score each against the
/// profiles; the "new" stretches cluster by what they share, and a cluster
/// with `new_task_min` focus minutes becomes one new task; a short "new"
/// stretch left over between two placements on one task joins that task
/// (unsure). Contiguous placements on one target merge into one row.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    spans: &[AnchoredSpan],
    lo: i64,
    hi: i64,
    profiles: &[Profile],
    labels: &HashMap<i64, String>,
    distractions: &[Regex],
    params: &Params,
    sp: &SegParams,
) -> Vec<Placement> {
    // 1. Score.
    let mut scored: Vec<(Seg, Segment, Verdict)> = Vec::new();
    for mut seg in segment(spans, distractions, sp) {
        seg.lo = seg.lo.max(lo);
        seg.hi = seg.hi.min(hi);
        if seg.hi <= seg.lo {
            continue;
        }
        let evidence = Segment::from_spans_skipping(spans, seg.lo, seg.hi, distractions);
        if evidence.keys.is_empty() {
            continue;
        }
        let v = profile::score(&evidence, profiles, params);
        scored.push((seg, evidence, v));
    }
    // 2. Place on existing tasks.
    let mut placed: Vec<Option<Placement>> = scored
        .iter()
        .map(|(_, ev, v)| place_existing(ev, v, labels))
        .collect();
    // 3. Cluster what is new by what it shares; a cluster with enough
    //    minutes is one new task, however scattered its stretches.
    let mut clusters: Vec<(Seg, Vec<usize>)> = Vec::new();
    for (i, (seg, ..)) in scored.iter().enumerate() {
        if placed[i].is_some() {
            continue;
        }
        match clusters.iter_mut().find(|(c, _)| c.akin(seg)) {
            Some((c, members)) => {
                c.absorb(seg);
                members.push(i);
            }
            None => clusters.push((seg.clone(), vec![i])),
        }
    }
    for (ci, (c, members)) in clusters.iter().enumerate() {
        if c.minutes < sp.new_task_min {
            continue;
        }
        let evidence = Segment {
            start_ts: c.lo,
            end_ts: c.hi,
            minutes: c.minutes,
            keys: c.keys.clone(),
            vec: None,
        };
        let mut label = evidence.describe(2);
        if label.is_empty() {
            let mut words: Vec<(&String, f64)> = c.ties.iter().map(|(t, m)| (t, *m)).collect();
            words.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            label = words
                .iter()
                .take(3)
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join(" ");
        }
        if label.is_empty() {
            label = "new work".to_owned();
        }
        let project = c
            .keys
            .iter()
            .filter(|(k, _)| matches!(k, Key::Anchor(AnchorKind::Place, _)))
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.value().to_owned());
        for &i in members {
            let (seg, _, v) = &scored[i];
            placed[i] = Some(Placement {
                lo: seg.lo,
                hi: seg.hi,
                target: Target::New {
                    label: label.clone(),
                    project: project.clone(),
                    cluster: ci,
                },
                confidence: v.new_task,
                confident: v.confident,
                reason: "new".to_owned(),
                margin: v.margin,
                runner_up: v.ranked.first().map(|c| c.task_id),
                kind: String::new(),
            });
        }
    }
    // 3b. A short new stretch still unplaced, sandwiched by one task, goes
    //     to that task as an excursion (unsure).
    for i in 0..scored.len() {
        if placed[i].is_some() || scored[i].0.minutes >= sp.new_task_min {
            continue;
        }
        let (Some(prev), Some(next)) = (
            i.checked_sub(1).and_then(|j| placed[j].as_ref()),
            placed.get(i + 1).and_then(|p| p.as_ref()),
        ) else {
            continue;
        };
        let (Target::Existing(a), Target::Existing(b)) = (&prev.target, &next.target) else {
            continue;
        };
        if a != b {
            continue;
        }
        let seg = &scored[i].0;
        if seg.lo - prev.hi >= AFK_GAP_MS || next.lo - seg.hi >= AFK_GAP_MS {
            continue;
        }
        placed[i] = Some(Placement {
            lo: seg.lo,
            hi: seg.hi,
            target: Target::Existing(*a),
            confidence: scored[i].2.new_task,
            confident: false,
            reason: format!("between {} and a new task", label_of(labels, *a)),
            margin: 0.0,
            runner_up: None,
            kind: String::new(),
        });
    }
    // 4. Contiguous rows on one target become one.
    let mut out: Vec<Placement> = Vec::new();
    for p in placed.into_iter().flatten() {
        if let Some(last) = out.last_mut()
            && last.target == p.target
            && p.lo - last.hi < AFK_GAP_MS
        {
            last.hi = last.hi.max(p.hi);
            last.confident = last.confident && p.confident;
            last.confidence = last.confidence.min(p.confidence);
            last.margin = last.margin.min(p.margin);
            continue;
        }
        out.push(p);
    }
    for p in &mut out {
        p.kind = kind_of(spans, p.lo, p.hi, distractions).to_owned();
    }
    out
}

/// The live tick: cut and score `[end of the newest derived batch, now)`,
/// rewrite that window's `segment` rows, create tasks for new clusters and
/// queue their naming. Returns what was placed.
pub fn run(
    conn: &mut Connection,
    config: &Config,
    now: Timestamp,
    distractions: &[Regex],
) -> Result<Vec<Placement>, StorageError> {
    let hi = ts_to_ms(now);
    let lo = storage::latest_done_batch_end(conn)?
        .unwrap_or(0)
        .max(hi - MAX_WINDOW_MS);
    // A correction from the last little while has taught something: its
    // tasks' evidence is refreshed before the window is scored again.
    let corrected = storage::recently_corrected_tasks(conn, hi - CORRECTION_LOOKBACK_MS)?;
    if !corrected.is_empty() {
        storage::refresh_task_evidence(conn, &ticket_re(config), &params(config), hi, &corrected)?;
    }
    place_window(conn, config, lo, hi, None, now, distractions)
}

/// How far back the live tick looks for corrections to learn from: two
/// ticks, so none is missed and none is refreshed forever.
const CORRECTION_LOOKBACK_MS: i64 = 2 * 60_000;

fn ticket_re(config: &Config) -> Regex {
    Regex::new(&config.ticket_regex)
        .or_else(|_| Regex::new(&Config::default().ticket_regex))
        .expect("default ticket regex compiles")
}

/// Scorer tunables: the defaults, with delta from the config when set
/// (`chronicle bench --calibrate` says what the verdict log supports).
pub fn params(config: &Config) -> Params {
    let mut p = Params::default();
    if let Some(d) = config.scorer_delta {
        p.delta = d;
    }
    p
}

/// After a correction (m30 chunk 4): re-score the day's reconciled batches
/// and its live tail with the profiles as they stand now, so one keep or
/// move fixes every stretch it teaches about. Rows the user placed are
/// untouched. When rows moved, a `rescore` correction holds the snapshot
/// for undo; returns `(correction id, rows moved)`.
pub fn rescore_day(
    conn: &mut Connection,
    config: &Config,
    now: Timestamp,
    distractions: &[Regex],
    day: &str,
    day_lo: i64,
    day_hi: i64,
) -> Result<Option<(i64, usize)>, StorageError> {
    let corrected =
        storage::recently_corrected_tasks(conn, ts_to_ms(now) - CORRECTION_LOOKBACK_MS)?;
    if !corrected.is_empty() {
        storage::refresh_task_evidence(
            conn,
            &ticket_re(config),
            &params(config),
            ts_to_ms(now),
            &corrected,
        )?;
    }
    let before = storage::segment_rows(conn, day_lo, day_hi)?;
    for batch_id in storage::done_batches_in(conn, day_lo, day_hi)? {
        if let Some((lo, hi)) = storage::batch_range(conn, batch_id)? {
            place_window(conn, config, lo, hi, Some(batch_id), now, distractions)?;
        }
    }
    let hi = ts_to_ms(now).min(day_hi);
    let lo = storage::latest_done_batch_end(conn)?
        .unwrap_or(0)
        .max(hi - MAX_WINDOW_MS)
        .max(day_lo);
    if lo < hi {
        place_window(conn, config, lo, hi, None, now, distractions)?;
    }
    let after = storage::segment_rows(conn, day_lo, day_hi)?;
    let owner_before = |lo: i64, hi: i64| -> Option<i64> {
        before
            .iter()
            .map(|r| (r.task_id, r.end_ts.min(hi) - r.start_ts.max(lo)))
            .filter(|(_, ov)| *ov > 0)
            .max_by_key(|(_, ov)| *ov)
            .map(|(t, _)| t)
    };
    let moved = after
        .iter()
        .filter(|r| owner_before(r.start_ts, r.end_ts).is_some_and(|t| t != r.task_id))
        .count();
    if moved == 0 {
        return Ok(None);
    }
    let id = storage::record_rescore(conn, now, day, &before, moved)?;
    Ok(Some((id, moved)))
}

/// Once a day in segmenter mode: verdicts left alone become right, the
/// evidence cache is rebuilt in full so decay advances, and derived tasks
/// with no project take the place their evidence saturates.
pub fn daily(conn: &mut Connection, config: &Config, now: Timestamp) -> Result<(), StorageError> {
    let now_ms = ts_to_ms(now);
    let last = storage::get_meta(conn, "segmenter_daily_ts")?
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now_ms - last < 86_400_000 {
        return Ok(());
    }
    let p = params(config);
    storage::passive_accept(conn, now)?;
    storage::rebuild_task_evidence(conn, &ticket_re(config), &p, now_ms)?;
    storage::infer_projects(conn, p.saturate_min)?;
    storage::set_meta(conn, "segmenter_daily_ts", Some(&now_ms.to_string()))?;
    Ok(())
}

/// The batch tier under the segmenter: re-score the batch's window with the
/// profiles as they stand now, attach the rows to the batch, mark it done.
/// No model process.
pub fn reconcile(
    conn: &mut Connection,
    config: &Config,
    batch_id: i64,
    now: Timestamp,
    distractions: &[Regex],
) -> Result<usize, StorageError> {
    let started = std::time::Instant::now();
    let Some((lo, hi)) = storage::batch_range(conn, batch_id)? else {
        return Ok(0);
    };
    let placed = place_window(conn, config, lo, hi, Some(batch_id), now, distractions)?;
    storage::finish_batch_reconciled(conn, batch_id, now, started.elapsed().as_millis() as i64)?;
    Ok(placed.len())
}

fn place_window(
    conn: &mut Connection,
    config: &Config,
    lo: i64,
    hi: i64,
    batch_id: Option<i64>,
    now: Timestamp,
    distractions: &[Regex],
) -> Result<Vec<Placement>, StorageError> {
    let ticket_re = ticket_re(config);
    let params = params(config);
    let sp = SegParams::from_config(config);
    let spans = storage::anchored_spans(conn, lo, hi)?;
    if spans.is_empty() {
        storage::store_segments(conn, lo, hi, batch_id, &[])?;
        return Ok(Vec::new());
    }
    let (profiles, labels) = storage::live_profiles(conn)?;
    let placements = decide(
        &spans,
        lo,
        hi,
        &profiles,
        &labels,
        distractions,
        &params,
        &sp,
    );
    let (touched, new_tasks) = storage::store_segments(conn, lo, hi, batch_id, &placements)?;
    if !touched.is_empty() {
        storage::refresh_task_evidence(conn, &ticket_re, &params, ts_to_ms(now), &touched)?;
    }
    for (task_id, plo, phi, placeholder) in new_tasks {
        let payload = serde_json::json!({
            "task_id": task_id,
            "lo": plo,
            "hi": phi,
            "placeholder": placeholder,
        })
        .to_string();
        storage::enqueue_ai_job(conn, now, "name_task", 0, &payload)?;
    }
    Ok(placements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Anchor;

    const M: i64 = 60_000;

    fn span(
        id: i64,
        lo: i64,
        hi: i64,
        app: &str,
        title: &str,
        anchors: &[(AnchorKind, &str)],
    ) -> AnchoredSpan {
        AnchoredSpan {
            id,
            start_ts: lo * M,
            end_ts: hi * M,
            app: app.into(),
            title: title.into(),
            anchors: anchors
                .iter()
                .map(|(k, v)| Anchor {
                    kind: *k,
                    value: (*v).to_owned(),
                })
                .collect(),
            vec: None,
        }
    }

    fn ranges(segs: &[Seg]) -> Vec<(i64, i64)> {
        segs.iter().map(|s| (s.lo / M, s.hi / M)).collect()
    }

    fn code(id: i64, lo: i64, hi: i64, branch: &str) -> AnchoredSpan {
        span(
            id,
            lo,
            hi,
            "Code",
            "main.rs - chronicle",
            &[
                (AnchorKind::Place, "chronicle"),
                (AnchorKind::Branch, branch),
            ],
        )
    }

    fn chat(id: i64, lo: i64, hi: i64) -> AnchoredSpan {
        span(
            id,
            lo,
            hi,
            "Slack",
            "#ops - Acme",
            &[(AnchorKind::People, "#ops")],
        )
    }

    fn notion(id: i64, lo: i64, hi: i64) -> AnchoredSpan {
        span(
            id,
            lo,
            hi,
            "Firefox",
            "Roadmap - Notion",
            &[(AnchorKind::Doc, "Roadmap")],
        )
    }

    #[test]
    fn glance_at_chat_folds_in() {
        let spans = vec![
            code(1, 0, 10, "m30"),
            chat(2, 10, 12),
            code(3, 12, 20, "m30"),
        ];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 20)]);
        assert!(!segs[0].closed);
    }

    #[test]
    fn a_long_enough_foreign_run_becomes_its_own_segment() {
        let spans = vec![
            code(1, 0, 10, "m30"),
            notion(2, 10, 12),
            notion(3, 12, 15),
            code(4, 15, 20, "m30"),
        ];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 10), (10, 15), (15, 20)]);
        let roadmap = Key::Anchor(AnchorKind::Doc, "Roadmap".into());
        assert!(segs[1].keys.contains_key(&roadmap));
        assert!(!segs[0].keys.contains_key(&roadmap));
    }

    #[test]
    fn a_swapped_strong_anchor_cuts_at_once() {
        let spans = vec![
            code(1, 0, 10, "m30"),
            code(2, 10, 11, "ACME-7-x"),
            code(3, 11, 20, "ACME-7-x"),
        ];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 10), (10, 20)]);
        // A first strong anchor of a new kind enriches rather than cuts.
        let pr = span(
            2,
            10,
            12,
            "Firefox",
            "Pull request #5",
            &[
                (AnchorKind::Change, "acme/x#5"),
                (AnchorKind::Place, "chronicle"),
            ],
        );
        let spans = vec![code(1, 0, 10, "m30"), pr, code(3, 12, 20, "m30")];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 20)]);
    }

    #[test]
    fn afk_gap_closes_a_segment() {
        let spans = vec![code(1, 0, 10, "m30"), code(2, 16, 20, "m30")];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 10), (16, 20)]);
        assert!(segs[0].closed && !segs[1].closed);
    }

    #[test]
    fn distractions_and_bare_spans_stretch_without_cutting() {
        let video = span(
            2,
            10,
            14,
            "Firefox",
            "Cats - YouTube",
            &[(AnchorKind::Domain, "youtube.com")],
        );
        let bare = span(3, 14, 15, "Terminator", "bash", &[]);
        let spans = vec![code(1, 0, 10, "m30"), video, bare, code(4, 15, 20, "m30")];
        let d = crate::evidence::compile_patterns(&["YouTube".to_owned()]);
        let segs = segment(&spans, &d, &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 20)]);
        assert_eq!(segs[0].minutes, 20.0);
    }

    #[test]
    fn a_short_segment_between_two_about_the_same_thing_folds() {
        // Two minutes on another repo's PR page inside a branch's work: too
        // short to earn a cut, it folds into the branch's segment.
        let pr = span(
            2,
            10,
            12,
            "Firefox",
            "Pull request #5",
            &[
                (AnchorKind::Change, "acme/x#5"),
                (AnchorKind::Place, "acme"),
            ],
        );
        let spans = vec![code(1, 0, 10, "m30"), pr, code(3, 12, 20, "m30")];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 20)]);
        assert_eq!(segs[0].minutes, 20.0);
    }

    #[test]
    fn documents_sharing_a_word_carry_one_segment() {
        // A PM rotating through a page, a design file and a doc that all
        // say "pricing": one segment, not three.
        let spans = vec![
            span(
                1,
                0,
                5,
                "notion",
                "Q4 pricing page – Product – Notion",
                &[
                    (AnchorKind::Doc, "Q4 pricing page"),
                    (AnchorKind::Place, "Product"),
                ],
            ),
            span(
                2,
                5,
                8,
                "figma",
                "Pricing page v3 – Figma",
                &[(AnchorKind::Doc, "Pricing page v3")],
            ),
            span(
                3,
                8,
                14,
                "Google-chrome",
                "Pricing FAQ - Google Docs",
                &[
                    (AnchorKind::Doc, "Pricing FAQ"),
                    (AnchorKind::Domain, "docs.google.com"),
                ],
            ),
            span(
                4,
                14,
                20,
                "Google-chrome",
                "Onboarding funnel Aug - Google Sheets",
                &[
                    (AnchorKind::Doc, "Onboarding funnel Aug"),
                    (AnchorKind::Domain, "docs.google.com"),
                ],
            ),
        ];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 14), (14, 20)]);
    }

    fn m30_profile() -> (Vec<Profile>, HashMap<i64, String>) {
        let mut minutes = HashMap::new();
        minutes.insert(Key::Anchor(AnchorKind::Branch, "m30".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 30.0);
        let profiles = vec![Profile {
            task_id: 7,
            minutes,
            last_ts: Some(0),
            vec: None,
        }];
        (profiles, HashMap::from([(7, "m30 work".to_owned())]))
    }

    #[test]
    fn decide_places_known_work_and_clusters_the_new() {
        let spans = vec![
            code(1, 0, 20, "m30"),
            notion(2, 20, 24),
            notion(3, 24, 26),
            code(4, 26, 40, "m30"),
            notion(5, 40, 46),
        ];
        let (profiles, labels) = m30_profile();
        let out = decide(
            &spans,
            0,
            46 * M,
            &profiles,
            &labels,
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        // Code lands on task 7 twice; the roadmap stretches (6 + 6 min) share a
        // document, so together they clear new_task_min and become one task.
        let ranges: Vec<(i64, i64)> = out.iter().map(|p| (p.lo / M, p.hi / M)).collect();
        assert_eq!(ranges, [(0, 20), (20, 26), (26, 40), (40, 46)], "{out:?}");
        assert_eq!(out[0].target, Target::Existing(7));
        assert!(out[0].confident);
        assert!(
            matches!(&out[1].target, Target::New { cluster: 0, label, .. } if label == "Roadmap"),
            "{:?}",
            out[1].target
        );
        assert_eq!(out[1].target, out[3].target);
        // Alone, one six-minute roadmap stretch stays unassigned.
        let out = decide(
            &spans[..4],
            0,
            40 * M,
            &profiles,
            &labels,
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert!(
            out.iter().all(|p| p.target == Target::Existing(7)),
            "{out:?}"
        );
    }

    #[test]
    fn kinds_follow_family_and_anchors() {
        let d = crate::evidence::compile_patterns(&["YouTube".to_owned()]);
        let agent = span(
            1,
            0,
            10,
            "Terminator",
            "✳ fix tests",
            &[
                (AnchorKind::Session, "s1"),
                (AnchorKind::Place, "chronicle"),
            ],
        );
        let pr = span(
            2,
            10,
            12,
            "Firefox",
            "PR #5",
            &[(AnchorKind::Change, "acme/x#5")],
        );
        let video = span(3, 12, 20, "Firefox", "Cats - YouTube", &[]);
        let spans = vec![agent, pr, video];
        assert_eq!(kind_of(&spans, 0, 10 * M, &d), "agent");
        assert_eq!(kind_of(&spans, 10 * M, 12 * M, &d), "review");
        // Break time never wins while any work is inside the range …
        assert_eq!(kind_of(&spans, 0, 20 * M, &d), "agent");
        // … but a stretch of nothing else is a break.
        assert_eq!(kind_of(&spans, 12 * M, 20 * M, &d), "break");
        assert_eq!(kind_of(&[code(4, 0, 5, "m30")], 0, 5 * M, &d), "author");
        assert_eq!(kind_of(&[chat(5, 0, 5)], 0, 5 * M, &d), "communicate");
        assert_eq!(kind_of(&[notion(6, 0, 5)], 0, 5 * M, &d), "read");
    }

    #[test]
    fn a_short_new_stretch_between_one_task_joins_it_unsure() {
        let meet = span(
            2,
            20,
            25,
            "Google-chrome",
            "Meet - abc-defg",
            &[(AnchorKind::Doc, "abc-defg")],
        );
        let spans = vec![code(1, 0, 20, "m30"), meet, code(3, 25, 40, "m30")];
        let (profiles, labels) = m30_profile();
        let out = decide(
            &spans,
            0,
            40 * M,
            &profiles,
            &labels,
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0].lo, out[0].hi), (0, 40 * M));
        assert!(!out[0].confident, "the folded stretch makes the row unsure");
    }
}
