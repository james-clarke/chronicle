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
use crate::extract::{Anchor, AnchorKind, Family, Strength};
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
    /// Count foreign minutes per strand across the whole segment, not
    /// only in one unbroken run (m33 chunk B, `segment_switch_mode =
    /// accumulated`): a strand reaching `switch_min` in all becomes its
    /// own row over the segment's range. Off by default: the cut rule
    /// alone, as before.
    pub accumulate: bool,
}

impl SegParams {
    pub fn from_config(c: &Config) -> Self {
        SegParams {
            switch_min: f64::from(c.segment_switch_min.max(1)),
            new_task_min: f64::from(c.segment_new_task_min),
            accumulate: c.segment_switch_mode != "contiguous",
        }
    }
}

impl Default for SegParams {
    fn default() -> Self {
        SegParams {
            switch_min: 3.0,
            new_task_min: 10.0,
            accumulate: false,
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
    /// The spans counted on this segment, by id: what its evidence is
    /// built from when it shares its range (m33 chunk B).
    pub ids: Vec<i64>,
    /// This segment's share of its range: 1 for a whole segment, less for
    /// a strand or the incumbent it was unravelled from; the rows over
    /// one range sum to 1.
    pub share: f64,
    /// A strand unravelled from a segment (m33 chunk B), as opposed to
    /// the incumbent that kept the rest of the range.
    pub strand: bool,
    /// Foreign work that folded back in, by what it shares (m33 chunk B):
    /// each strand's own keys, ties, minutes and spans. Unravelled into
    /// rows of their own once the segment closes.
    strands: Vec<Seg>,
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
            ids: Vec::new(),
            share: 1.0,
            strand: false,
            strands: Vec::new(),
        }
    }

    fn add(&mut self, id: i64, sig: &Signature, minutes: f64, end: i64) {
        for k in &sig.keys {
            *self.keys.entry(k.clone()).or_insert(0.0) += minutes;
        }
        for t in &sig.ties {
            *self.ties.entry(t.clone()).or_insert(0.0) += minutes;
        }
        self.minutes += minutes;
        self.hi = self.hi.max(end);
        self.ids.push(id);
    }

    /// Time only, not what it is about: a distraction, a bare span, or a
    /// foreign span that may yet fold back.
    fn stretch(&mut self, id: i64, minutes: f64, end: i64) {
        self.minutes += minutes;
        self.hi = self.hi.max(end);
        self.ids.push(id);
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
        self.ids.extend(other.ids.iter().copied());
        self.strands.extend(other.strands.iter().cloned());
    }

    /// A foreign run that folded back (m33 chunk B): its spans were
    /// counted on the segment as time; what each is about goes to the
    /// strand it shares an anchor or a leading word with, else a strand
    /// of its own. The run carries its spans one by one in `strands`, so
    /// a run that mixed two repos parts again here.
    fn fold_run(&mut self, run: Seg) {
        for piece in run.strands {
            let hit = self
                .strands
                .iter_mut()
                .find(|st| st.akin(&piece) || piece.keys.keys().any(|k| st.keys.contains_key(k)));
            match hit {
                Some(st) => st.absorb(&piece),
                None => self.strands.push(piece),
            }
        }
    }

    /// The segment as rows over its range (m33 chunk B): every strand
    /// that reached `switch_min` becomes one, with its share of the time,
    /// and the incumbent keeps the rest — unless the incumbent itself is
    /// under `switch_min`, when the largest strand takes it. A segment
    /// with no strand over the bar stays whole, its strands' minutes still
    /// counted on it.
    fn unravel(mut self, p: &SegParams) -> Vec<Seg> {
        let strands = std::mem::take(&mut self.strands);
        if !p.accumulate || strands.is_empty() {
            return vec![self];
        }
        // Strands from absorbed segments may be about the same thing.
        let mut merged: Vec<Seg> = Vec::new();
        for st in strands {
            match merged.iter_mut().find(|m| m.akin(&st)) {
                Some(m) => m.absorb(&st),
                None => merged.push(st),
            }
        }
        let mut rows: Vec<Seg> = merged
            .into_iter()
            .filter(|st| st.minutes >= p.switch_min)
            .collect();
        if rows.is_empty() {
            return vec![self];
        }
        rows.sort_by(|a, b| {
            b.minutes
                .partial_cmp(&a.minutes)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let total = self.minutes;
        let taken: f64 = rows.iter().map(|r| r.minutes).sum();
        let (lo, hi, closed) = (self.lo, self.hi, self.closed);
        let mut incumbent = self;
        incumbent.minutes = (incumbent.minutes - taken).max(0.0);
        let strand_ids: BTreeSet<i64> = rows.iter().flat_map(|r| r.ids.iter().copied()).collect();
        incumbent.ids.retain(|id| !strand_ids.contains(id));
        if incumbent.minutes < p.switch_min {
            rows[0].absorb(&incumbent);
        } else {
            rows.insert(0, incumbent);
        }
        for r in &mut rows {
            r.lo = lo;
            r.hi = hi;
            r.closed = closed;
            r.strand = true;
            r.share = if total > 0.0 {
                (r.minutes / total).clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
        rows[0].strand = false;
        rows
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
        let distraction = crate::evidence::is_furniture(&s.app, &s.title, distractions);

        if let Some(c) = cur.as_mut()
            && s.start_ts - c.hi >= AFK_GAP_MS
        {
            if let Some(r) = run.take()
                && p.accumulate
            {
                c.fold_run(r);
            }
            out.push(cur.take().expect("checked"));
        }
        let Some(c) = cur.as_mut() else {
            let mut c = Seg::empty(s.start_ts);
            if distraction {
                c.stretch(s.id, dur, s.end_ts);
            } else {
                c.add(s.id, &sig, dur, s.end_ts);
            }
            cur = Some(c);
            continue;
        };

        if distraction || sig.is_empty() {
            c.stretch(s.id, dur, s.end_ts);
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
            c.add(s.id, &sig, dur, s.end_ts);
            // The excursion folded back: its time stays on the segment;
            // what it was about becomes a strand (accumulated mode).
            if let Some(r) = run.take()
                && p.accumulate
            {
                c.fold_run(r);
            }
            continue;
        }
        if swaps_strong && c.minutes >= p.switch_min && run.is_none() {
            let mut next = Seg::empty(s.start_ts);
            next.add(s.id, &sig, dur, s.end_ts);
            let mut done = std::mem::replace(c, next);
            done.hi = done.hi.min(s.start_ts);
            out.push(done);
            continue;
        }
        // Foreign to the segment: the run grows until it earns a cut. It
        // keeps each span apart too, for the strands it may fold into.
        let r = run.get_or_insert_with(|| Seg::empty(s.start_ts));
        r.add(s.id, &sig, dur, s.end_ts);
        if p.accumulate {
            let mut piece = Seg::empty(s.start_ts);
            piece.add(s.id, &sig, dur, s.end_ts);
            r.strands.push(piece);
        }
        if r.minutes >= p.switch_min {
            let start = r.lo;
            let mut next = run.take().expect("just inserted");
            next.strands.clear();
            let mut done = std::mem::replace(c, next);
            done.hi = done.hi.min(start);
            // The run's own minutes were counted on the open segment while
            // it could still fold back; they belong to the new one now.
            done.minutes = (done.minutes - (c.minutes - dur)).max(0.0);
            let run_ids: BTreeSet<i64> = c.ids.iter().copied().collect();
            done.ids.retain(|id| !run_ids.contains(id));
            out.push(done);
        } else {
            // Tentatively part of the open segment (time only, not what it
            // is about), in case it folds back.
            c.stretch(s.id, dur, s.end_ts);
        }
    }
    if let Some(mut c) = cur.take() {
        if let Some(r) = run.take()
            && p.accumulate
        {
            c.fold_run(r);
        }
        c.closed = false;
        out.push(c);
    }
    out.retain(|s| s.hi > s.lo);
    fold_excursions(out, p)
        .into_iter()
        .flat_map(|s| s.unravel(p))
        .collect()
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
                if p.accumulate {
                    // What the excursion was about is also a strand, so a
                    // repo that keeps cutting in and folding back adds up
                    // to a row of its own; the cut itself is unchanged.
                    let mut piece = s.clone();
                    piece.strands.clear();
                    merged.fold_run(Seg {
                        strands: vec![piece],
                        ..Seg::empty(s.lo)
                    });
                }
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
    /// The fraction of `[lo, hi)` that is this target's: 1 for a whole
    /// segment, less when concurrent AI sessions split it (m32 chunk 3);
    /// the rows over one range sum to 1.
    pub share: f64,
    /// A strand the segmenter unravelled (m33 chunk B): one repo's own
    /// spans, not split again by the sessions live around it; the
    /// incumbent it left is split as any whole row, its shares scaled.
    pub strand: bool,
}

/// The kinds of work a segment can be, general across roles.
pub const KINDS: [&str; 10] = [
    "author",
    "agent",
    "supervise",
    "review",
    "communicate",
    "meet",
    "plan",
    "read",
    "admin",
    "break",
];

/// One span's kind from its app family and anchors: an editor, design or
/// office window is `author`; a terminal with an agent session `agent`, or
/// `supervise` when the span was mostly quiet while the session wrote
/// (m32 chunk 1: hands off, watching the agent), without one `author`; a
/// change page or a git GUI `review`; a tracker page (item, no change)
/// `plan`; a calendar entry or a call `meet`, as is a meeting app; chat and
/// mail `communicate`; a document or site with nothing more `read`; a
/// distraction `break`; Chronicle's own window `admin` (m33).
fn span_kind(span: &AnchoredSpan, distractions: &[Regex]) -> &'static str {
    if crate::evidence::is_self_window(&span.app) {
        return "admin";
    }
    if crate::evidence::is_distraction(&span.app, &span.title, distractions) {
        return "break";
    }
    let has = |k: AnchorKind| span.anchors.iter().any(|a| a.kind == k);
    if has(AnchorKind::Event) {
        return "meet";
    }
    match crate::extract::family(&span.app) {
        Family::Editor | Family::Document => "author",
        Family::Terminal if has(AnchorKind::Session) => {
            if span.wrote && span.quiet_ms * 2 >= span.end_ts - span.start_ts {
                "supervise"
            } else {
                "agent"
            }
        }
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
/// on; `admin` only when no work is there, `break` only when nothing else.
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
        .filter(|(k, _)| **k != "break" && **k != "admin")
        .max_by_key(|(k, m)| (**m, std::cmp::Reverse(**k)))
        .map(|(k, _)| *k);
    work.unwrap_or(if ms.is_empty() {
        "read"
    } else if ms.contains_key("admin") {
        "admin"
    } else {
        "break"
    })
}

fn label_of(labels: &HashMap<i64, String>, id: i64) -> String {
    labels
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("task {id}"))
}

/// One segment's verdict as a placement on an existing task, or `None`
/// when the scorer calls it new (clustering decides those).
fn place_existing(
    seg: &Segment,
    share: f64,
    strand: bool,
    v: &Verdict,
    labels: &HashMap<i64, String>,
) -> Option<Placement> {
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
        share,
        strand,
    })
}

/// The place veto (m33 chunk C): a task declared in one project cannot
/// take a segment whose dominant place is another repo, whatever its
/// profile has learned — unless the segment carries a ticket the task's
/// own label names (a ticket spanning repos is one task). A task with no
/// project, a segment with no place, or a task whose project ties for
/// the segment's top place are left alone. With the winner vetoed the
/// verdict settles again on what remains, so the segment falls to the
/// next task in its own repo or to a new one, not to the incumbent.
fn place_veto(
    v: &mut Verdict,
    seg: &Segment,
    profiles: &[Profile],
    projects: &HashMap<i64, Option<String>>,
    params: &Params,
) {
    let places = || {
        seg.keys.iter().filter_map(|(k, m)| match k {
            Key::Anchor(AnchorKind::Place, p) => Some((p.as_str(), *m)),
            _ => None,
        })
    };
    let Some((place, top)) = places().max_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.0.cmp(a.0))
    }) else {
        return;
    };
    v.retain(seg, params, |t| {
        let Some(Some(project)) = projects.get(&t) else {
            return true;
        };
        if project.eq_ignore_ascii_case(place) {
            return true;
        }
        let own = places()
            .filter(|(p, _)| p.eq_ignore_ascii_case(project))
            .map(|(_, m)| m)
            .fold(0.0, f64::max);
        if own >= top {
            return true;
        }
        profiles.iter().any(|pr| {
            pr.task_id == t
                && pr.declared.iter().any(|k| {
                    matches!(k, Key::Anchor(AnchorKind::Item, _)) && seg.keys.contains_key(k)
                })
        })
    });
}

/// The declared sink (m35 fix 3): a segment whose dominant place has a
/// task the person declared goes to the newest such task, whatever the
/// scorer preferred, unless the segment carries a ticket the sink does
/// not hold and another task does. The scorer's ranking stays underneath
/// (confidence and margin still describe its view, so the row reads as
/// uncertain when it disagreed and the runner-up stays one click away).
/// Returns the place the rule fired on.
fn declared_sink(
    v: &mut Verdict,
    seg: &Segment,
    profiles: &[Profile],
    sinks: &HashMap<String, i64>,
) -> Option<String> {
    let (place, _) = seg
        .keys
        .iter()
        .filter_map(|(k, m)| match k {
            Key::Anchor(AnchorKind::Place, p) => Some((p.as_str(), *m)),
            _ => None,
        })
        .max_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.0.cmp(a.0))
        })?;
    let &sink = sinks.get(&place.to_ascii_lowercase())?;
    let items: Vec<&Key> = seg
        .keys
        .keys()
        .filter(|k| matches!(k, Key::Anchor(AnchorKind::Item, _)))
        .collect();
    let holds = |pr: &Profile| items.iter().any(|k| pr.minutes.contains_key(*k));
    let sink_holds = profiles.iter().any(|pr| pr.task_id == sink && holds(pr));
    if !sink_holds && profiles.iter().any(|pr| pr.task_id != sink && holds(pr)) {
        return None;
    }
    let score = v
        .ranked
        .iter()
        .find(|c| c.task_id == sink)
        .map_or(0.0, |c| c.score);
    v.ranked.retain(|c| c.task_id != sink);
    v.ranked.insert(
        0,
        profile::Candidate {
            task_id: sink,
            score,
        },
    );
    let runner = v
        .ranked
        .get(1)
        .map_or(0.0, |c| c.score)
        .max(v.new_task);
    v.confident = v.best == Some(sink) && v.confident;
    v.best = Some(sink);
    v.margin = score - runner;
    Some(place.to_owned())
}

/// Decide every segment in `spans` over `[lo, hi)`: score each against the
/// profiles under the place veto; the "new" stretches cluster by what
/// they share, and a cluster with `new_task_min` focus minutes becomes one
/// new task; a short "new" stretch left over between two placements on
/// one task joins that task (unsure). Contiguous placements on one target
/// merge into one row.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    spans: &[AnchoredSpan],
    lo: i64,
    hi: i64,
    profiles: &[Profile],
    labels: &HashMap<i64, String>,
    projects: &HashMap<i64, Option<String>>,
    sinks: &HashMap<String, i64>,
    distractions: &[Regex],
    params: &Params,
    sp: &SegParams,
) -> Vec<Placement> {
    // 1. Score.
    let mut scored: Vec<(Seg, Segment, Verdict)> = Vec::new();
    let mut sunk: Vec<Option<String>> = Vec::new();
    for mut seg in segment(spans, distractions, sp) {
        seg.lo = seg.lo.max(lo);
        seg.hi = seg.hi.min(hi);
        if seg.hi <= seg.lo {
            continue;
        }
        // A row sharing its range (m33 chunk B) is scored on its own
        // spans, not everything in the range.
        let evidence = if seg.share < 1.0 {
            let own: Vec<AnchoredSpan> = spans
                .iter()
                .filter(|s| seg.ids.contains(&s.id))
                .cloned()
                .collect();
            Segment::from_spans_skipping(&own, seg.lo, seg.hi, distractions)
        } else {
            Segment::from_spans_skipping(spans, seg.lo, seg.hi, distractions)
        };
        if evidence.keys.is_empty() {
            continue;
        }
        let mut v = profile::score(&evidence, profiles, params);
        place_veto(&mut v, &evidence, profiles, projects, params);
        sunk.push(declared_sink(&mut v, &evidence, profiles, sinks));
        scored.push((seg, evidence, v));
    }
    // 2. Place on existing tasks.
    let mut placed: Vec<Option<Placement>> = scored
        .iter()
        .zip(&sunk)
        .map(|((seg, ev, v), sunk)| {
            let mut p = place_existing(ev, seg.share, seg.strand, v, labels)?;
            if let Some(place) = sunk {
                p.reason = format!("declared in {place}");
            }
            Some(p)
        })
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
        let mut label = crate::evidence::strip_glyphs(&evidence.describe(2)).to_owned();
        if label.is_empty() {
            let mut words: Vec<(&String, f64)> = c.ties.iter().map(|(t, m)| (t, *m)).collect();
            words.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            label = words
                .iter()
                .map(|(t, _)| crate::evidence::strip_glyphs(t.as_str()))
                .filter(|t| !t.is_empty())
                .take(3)
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
                share: seg.share,
                strand: seg.strand,
            });
        }
    }
    // 3b. A short new stretch still unplaced, sandwiched by one task, goes
    //     to that task as an excursion (unsure). A strand cleared its own
    //     bar to stand apart; it does not fold into a neighbour.
    for i in 0..scored.len() {
        if placed[i].is_some() || scored[i].0.minutes >= sp.new_task_min || scored[i].0.share < 1.0
        {
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
            share: 1.0,
            strand: false,
        });
    }
    // A row sharing its range has the kind of its own spans.
    for (i, p) in placed.iter_mut().enumerate() {
        if let Some(p) = p
            && p.share < 1.0
        {
            let own: Vec<AnchoredSpan> = spans
                .iter()
                .filter(|s| scored[i].0.ids.contains(&s.id))
                .cloned()
                .collect();
            p.kind = kind_of(&own, p.lo, p.hi, distractions).to_owned();
        }
    }
    // 4. Contiguous whole rows on one target become one; rows sharing one
    //    range on one target add their shares.
    let mut out: Vec<Placement> = Vec::new();
    for p in placed.into_iter().flatten() {
        if let Some(last) = out.last_mut()
            && last.target == p.target
        {
            let same_range = last.lo == p.lo && last.hi == p.hi;
            let whole = last.share >= 1.0 && p.share >= 1.0;
            if same_range && !whole {
                if p.share > last.share {
                    last.kind = p.kind.clone();
                    last.reason = p.reason.clone();
                }
                last.share = (last.share + p.share).min(1.0);
                last.confident = last.confident && p.confident;
                last.confidence = last.confidence.min(p.confidence);
                last.margin = last.margin.min(p.margin);
                continue;
            }
            if whole && p.lo - last.hi < AFK_GAP_MS {
                last.hi = last.hi.max(p.hi);
                last.confident = last.confident && p.confident;
                last.confidence = last.confidence.min(p.confidence);
                last.margin = last.margin.min(p.margin);
                continue;
            }
        }
        out.push(p);
    }
    for p in &mut out {
        if p.share >= 1.0 {
            p.kind = kind_of(spans, p.lo, p.hi, distractions).to_owned();
        }
    }
    out
}

/// An AI session live around the window (m32 chunk 3): its transcript
/// write and prompt minutes, and the anchors its own row carries.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveSession {
    pub id: String,
    /// Its terminal title, else its first prompt.
    pub title: Option<String>,
    pub writes: Vec<i64>,
    pub prompts: Vec<i64>,
    pub anchors: Vec<Anchor>,
}

/// A session that wrote within this of a segment's edges was live in it.
pub const CONCURRENT_MS: i64 = 2 * 60_000;
/// Cluster ids for the tasks the split creates, past any of [`decide`]'s.
const SPLIT_CLUSTER_BASE: usize = 1 << 20;

/// Concurrency = supervision (m32 chunk 3): an `agent` or `supervise`
/// placement with two or more sessions writing within [`CONCURRENT_MS`]
/// of it becomes one row per task, each over the whole range with a
/// `share` by the prompts typed into that task's sessions (their focus
/// time when no prompt was kept). A session's task is what its own spans
/// in the range score to — or, with none on screen, its own scope (repo,
/// branch, item). A session in a repo that is not its verdict's project
/// (and whose item that task does not hold) is foreign to it: it goes to
/// the best-ranked task in its own repo, else becomes a new task there
/// (one per repo across the window, given `new_task_min` of shared time;
/// less stays with the segment's target), because a catch-all that once
/// absorbed a repo must not keep every session from it. A session the
/// scorer calls new with no repo stays with the segment's target. The
/// row's kind is its own sessions' (`supervise` for one never on screen).
/// Every other placement passes through whole.
#[allow(clippy::too_many_arguments)]
pub fn split_concurrent(
    placements: Vec<Placement>,
    spans: &[AnchoredSpan],
    sessions: &[LiveSession],
    profiles: &[Profile],
    projects: &HashMap<i64, Option<String>>,
    sinks: &HashMap<String, i64>,
    params: &Params,
    sp: &SegParams,
    distractions: &[Regex],
) -> Vec<Placement> {
    let mut out = Vec::with_capacity(placements.len());
    // Repos the split has opened a new task for, with its label, in order:
    // the cluster id.
    let mut opened: Vec<(String, String)> = Vec::new();
    for p in placements {
        // A row the segmenter already unravelled by strand (m33 chunk B)
        // is one repo's work; the sessions of the others are not its.
        if sessions.len() < 2 || p.strand || !matches!(p.kind.as_str(), "agent" | "supervise") {
            out.push(p);
            continue;
        }
        let (lo, hi) = (p.lo - CONCURRENT_MS, p.hi + CONCURRENT_MS);
        let within = |t: &i64| (lo..=hi).contains(t);
        let live: Vec<&LiveSession> = sessions
            .iter()
            .filter(|s| s.writes.iter().any(within))
            .collect();
        if live.len() < 2 {
            out.push(p);
            continue;
        }
        let owned: Vec<Vec<AnchoredSpan>> = live
            .iter()
            .map(|s| {
                spans
                    .iter()
                    .filter(|sp| sp.end_ts > p.lo && sp.start_ts < p.hi)
                    .filter(|sp| {
                        sp.anchors
                            .iter()
                            .any(|a| a.kind == AnchorKind::Session && a.value == s.id)
                    })
                    .cloned()
                    .collect()
            })
            .collect();
        let mut weights: Vec<f64> = live
            .iter()
            .map(|s| s.prompts.iter().filter(|t| within(t)).count() as f64)
            .collect();
        let by_prompts = weights.iter().sum::<f64>() > 0.0;
        if !by_prompts {
            weights = owned
                .iter()
                .map(|ss| {
                    ss.iter()
                        .map(|sp| sp.end_ts.min(p.hi) - sp.start_ts.max(p.lo))
                        .sum::<i64>() as f64
                })
                .collect();
        }
        let total: f64 = weights.iter().sum();
        if total <= 0.0 {
            out.push(p);
            continue;
        }
        // (target, weight, sessions, their spans)
        let mut groups: Vec<(Target, f64, usize, Vec<AnchoredSpan>)> = Vec::new();
        for (i, s) in live.iter().enumerate() {
            if weights[i] <= 0.0 {
                continue;
            }
            let verdict = session_verdict(s, &owned[i], p.lo, p.hi, profiles, params, distractions);
            let target = session_target(
                s,
                &owned[i],
                verdict.as_ref(),
                &p.target,
                profiles,
                projects,
                sinks,
                &mut opened,
            );
            match groups.iter_mut().find(|g| g.0 == target) {
                Some(g) => {
                    g.1 += weights[i];
                    g.2 += 1;
                    g.3.extend(owned[i].iter().cloned());
                }
                None => groups.push((target, weights[i], 1, owned[i].clone())),
            }
        }
        // A repo's new task needs its share of the range to be worth one;
        // a sliver stays with the segment's target.
        let minutes = (p.hi - p.lo) as f64 / 60_000.0;
        let mut slivers: Vec<(f64, usize, Vec<AnchoredSpan>)> = Vec::new();
        groups.retain_mut(|g| {
            let sliver = matches!(g.0, Target::New { .. })
                && g.0 != p.target
                && g.1 / total * minutes < sp.new_task_min;
            if sliver {
                slivers.push((g.1, g.2, std::mem::take(&mut g.3)));
            }
            !sliver
        });
        for (w, n, ss) in slivers {
            match groups.iter_mut().find(|g| g.0 == p.target) {
                Some(g) => {
                    g.1 += w;
                    g.2 += n;
                    g.3.extend(ss);
                }
                None => groups.push((p.target.clone(), w, n, ss)),
            }
        }
        if groups.len() < 2 {
            out.push(p);
            continue;
        }
        // The segment's own target first, then by weight.
        groups.sort_by(|a, b| {
            (b.0 == p.target)
                .cmp(&(a.0 == p.target))
                .then(b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal))
        });
        for (target, w, n, mut ss) in groups {
            ss.sort_by_key(|s| (s.start_ts, s.id));
            let kind = if ss.is_empty() {
                "supervise"
            } else {
                kind_of(&ss, p.lo, p.hi, distractions)
            };
            let basis = if by_prompts {
                format!("{w} of {total} prompts")
            } else {
                format!("{:.0}% of focus", w / total * 100.0)
            };
            let reason = format!("{n} of {} concurrent sessions, {basis}", live.len());
            out.push(Placement {
                target,
                kind: kind.to_owned(),
                share: p.share * w / total,
                reason,
                ..p.clone()
            });
        }
    }
    out
}

/// Where one live session goes given its verdict: the newest task the
/// person declared in the session's repo, unless another task holds the
/// session's item; else its best task when that task is in the session's
/// repo, has no project, or holds the session's item; else any task
/// holding the item (a ticket spanning repos is one task); else the
/// best-ranked task in the session's repo; else a new task in that repo,
/// one per repo over the window; with no repo at all, the segment's own
/// target.
#[allow(clippy::too_many_arguments)]
fn session_target(
    s: &LiveSession,
    owned: &[AnchoredSpan],
    verdict: Option<&Verdict>,
    fallback: &Target,
    profiles: &[Profile],
    projects: &HashMap<i64, Option<String>>,
    sinks: &HashMap<String, i64>,
    opened: &mut Vec<(String, String)>,
) -> Target {
    let anchor = |kind: AnchorKind| -> Vec<&str> {
        s.anchors
            .iter()
            .chain(owned.iter().flat_map(|sp| sp.anchors.iter()))
            .filter(|a| a.kind == kind)
            .map(|a| a.value.as_str())
            .collect()
    };
    let place = anchor(AnchorKind::Place).first().copied();
    let items = anchor(AnchorKind::Item);
    let consistent = |t: i64| -> bool {
        let Some(place) = place else {
            return true;
        };
        match projects.get(&t) {
            None | Some(None) => true,
            Some(Some(project)) if project.eq_ignore_ascii_case(place) => true,
            Some(Some(_)) => profiles.iter().any(|pr| {
                pr.task_id == t
                    && items.iter().any(|i| {
                        pr.minutes
                            .contains_key(&Key::Anchor(AnchorKind::Item, (*i).to_owned()))
                    })
            }),
        }
    };
    // A task that holds the session's ticket owns it whatever the repo.
    let holds = |pr: &Profile| {
        items.iter().any(|i| {
            pr.minutes
                .contains_key(&Key::Anchor(AnchorKind::Item, (*i).to_owned()))
        })
    };
    // The task the person declared in this repo is its sink (m35 fix 3):
    // a derived task with more evidence never outranks it. Only a ticket
    // the sink does not hold and another task does overrides it.
    if let Some(place) = place
        && let Some(&sink) = sinks.get(&place.to_ascii_lowercase())
    {
        let sink_holds = profiles.iter().any(|pr| pr.task_id == sink && holds(pr));
        if sink_holds || !profiles.iter().any(|pr| pr.task_id != sink && holds(pr)) {
            return Target::Existing(sink);
        }
    }
    if let Some(t) = verdict.and_then(|v| v.best)
        && consistent(t)
    {
        return Target::Existing(t);
    }
    if let Some(v) = verdict
        && let Some(c) = v.ranked.iter().find(|c| {
            profiles
                .iter()
                .any(|pr| pr.task_id == c.task_id && holds(pr))
        })
    {
        return Target::Existing(c.task_id);
    }
    if let Some(pr) = profiles.iter().find(|pr| holds(pr)) {
        return Target::Existing(pr.task_id);
    }
    let Some(place) = place else {
        return fallback.clone();
    };
    if let Some(v) = verdict
        && let Some(c) = v.ranked.iter().find(|c| {
            c.score > 0.0
                && projects.get(&c.task_id).is_some_and(|pr| {
                    pr.as_deref()
                        .is_some_and(|pr| pr.eq_ignore_ascii_case(place))
                })
        })
    {
        return Target::Existing(c.task_id);
    }
    let cluster = match opened.iter().position(|(o, _)| o == place) {
        Some(i) => i,
        None => {
            let label = s
                .title
                .as_deref()
                .map(crate::evidence::strip_glyphs)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map_or_else(|| format!("{place} session"), str::to_owned);
            opened.push((place.to_owned(), label));
            opened.len() - 1
        }
    };
    Target::New {
        label: opened[cluster].1.clone(),
        project: Some(place.to_owned()),
        cluster: SPLIT_CLUSTER_BASE + cluster,
    }
}

/// The scorer's verdict on one live session's evidence over `[lo, hi)`:
/// its spans on screen, else its own scope anchors spread over the range.
/// `None` when it has no evidence at all.
pub fn session_verdict(
    s: &LiveSession,
    owned: &[AnchoredSpan],
    lo: i64,
    hi: i64,
    profiles: &[Profile],
    params: &Params,
    distractions: &[Regex],
) -> Option<Verdict> {
    let evidence = if owned.is_empty() {
        let minutes = (hi - lo) as f64 / 60_000.0;
        Segment {
            start_ts: lo,
            end_ts: hi,
            minutes,
            keys: s
                .anchors
                .iter()
                .map(|a| (Key::Anchor(a.kind, a.value.clone()), minutes))
                .collect(),
            vec: None,
        }
    } else {
        Segment::from_spans_skipping(owned, lo, hi, distractions)
    };
    if evidence.keys.is_empty() {
        return None;
    }
    Some(profile::score(&evidence, profiles, params))
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

/// A freshly declared task's profile: its declared place and ticket rows
/// land in the evidence cache now, so placement can score it before
/// anything has been placed into it (m35 fix 1; the cache otherwise fills
/// only for tasks a placement or correction touched).
pub fn seed_task_evidence(
    conn: &mut Connection,
    config: &Config,
    now: Timestamp,
    task_id: i64,
) -> Result<usize, StorageError> {
    storage::refresh_task_evidence(
        conn,
        &ticket_re(config),
        &params(config),
        ts_to_ms(now),
        &[task_id],
    )
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
            .map(|r| (r.task_id, r.end_ts.min(hi) - r.start_ts.max(lo), r.share))
            .filter(|(_, ov, _)| *ov > 0)
            .map(|(t, ov, share)| (t, ov as f64 * share))
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
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

/// What [`reconcile`] would write for `[lo, hi)` against `profiles`,
/// without writing it (`chronicle bench --window`, which builds them as of
/// the window's start the way the replay does).
#[allow(clippy::too_many_arguments)]
pub fn place_dry(
    conn: &Connection,
    config: &Config,
    lo: i64,
    hi: i64,
    profiles: &[Profile],
    labels: &HashMap<i64, String>,
    projects: &HashMap<i64, Option<String>>,
    distractions: &[Regex],
) -> Result<Vec<Placement>, StorageError> {
    let ticket_re = ticket_re(config);
    let params = params(config);
    let sp = SegParams::from_config(config);
    let spans = storage::anchored_spans(conn, lo, hi)?;
    if spans.is_empty() {
        return Ok(Vec::new());
    }
    let sinks = storage::declared_sinks(conn)?;
    let placements = decide(
        &spans,
        lo,
        hi,
        profiles,
        labels,
        projects,
        &sinks,
        distractions,
        &params,
        &sp,
    );
    let sessions = storage::live_sessions(conn, lo, hi, &ticket_re)?;
    Ok(split_concurrent(
        placements,
        &spans,
        &sessions,
        profiles,
        projects,
        &sinks,
        &params,
        &sp,
        distractions,
    ))
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
    // A declared task the cache never saw (declared before seeding
    // existed, or through a path that does not seed) gets its rows now.
    let unseeded = storage::unseeded_user_tasks(conn)?;
    if !unseeded.is_empty() {
        storage::refresh_task_evidence(conn, &ticket_re, &params, ts_to_ms(now), &unseeded)?;
    }
    let (profiles, labels) = storage::live_profiles(conn)?;
    let projects = storage::task_projects(conn)?;
    let sinks = storage::declared_sinks(conn)?;
    let placements = decide(
        &spans,
        lo,
        hi,
        &profiles,
        &labels,
        &projects,
        &sinks,
        distractions,
        &params,
        &sp,
    );
    let sessions = storage::live_sessions(conn, lo, hi, &ticket_re)?;
    let placements = split_concurrent(
        placements,
        &spans,
        &sessions,
        &profiles,
        &projects,
        &sinks,
        &params,
        &sp,
        distractions,
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
            quiet_ms: 0,
            wrote: false,
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

    /// Interleaved foreign work never runs three minutes unbroken, but
    /// adds up (m33 chunk B): the strand that clears the bar becomes its
    /// own row over the range with its share, the one under it stays with
    /// the incumbent, and the contiguous rule alone keeps it all whole.
    #[test]
    fn interleaved_foreign_work_unravels_into_strands() {
        let contoso = |id: i64, lo: i64, hi: i64| {
            span(
                id,
                lo,
                hi,
                "Code",
                "tasks.py - contoso",
                &[(AnchorKind::Place, "contoso"), (AnchorKind::Item, "ACME-1")],
            )
        };
        let fabrikam = |id: i64, lo: i64, hi: i64| {
            span(
                id,
                lo,
                hi,
                "Firefox",
                "fabrikam-web - localhost",
                &[(AnchorKind::Place, "fabrikam-web")],
            )
        };
        // chronicle 12 min, contoso 4 × 1 min, fabrikam-web 2 × 1 min.
        let spans = vec![
            code(1, 0, 3, "m33"),
            contoso(2, 3, 4),
            code(3, 4, 7, "m33"),
            fabrikam(4, 7, 8),
            contoso(5, 8, 9),
            code(6, 9, 12, "m33"),
            contoso(7, 12, 13),
            fabrikam(8, 13, 14),
            code(9, 14, 17, "m33"),
            contoso(10, 17, 18),
        ];
        let accumulated = SegParams {
            accumulate: true,
            ..SegParams::default()
        };
        let segs = segment(&spans, &[], &accumulated);
        assert_eq!(ranges(&segs), [(0, 18), (0, 18)], "{segs:?}");
        let inc = &segs[0];
        let strand = &segs[1];
        let nyc = Key::Anchor(AnchorKind::Place, "contoso".into());
        assert!(
            inc.keys
                .contains_key(&Key::Anchor(AnchorKind::Branch, "m33".into()))
        );
        assert!(!inc.keys.contains_key(&nyc));
        assert!(strand.keys.contains_key(&nyc));
        assert_eq!(strand.ids, [2, 5, 7, 10]);
        assert!((strand.minutes - 4.0).abs() < 1e-9);
        assert!((strand.share - 4.0 / 18.0).abs() < 1e-9, "{}", strand.share);
        // The incumbent keeps the fabrikam-web minutes (under the bar) as time.
        assert!((inc.minutes - 14.0).abs() < 1e-9, "{}", inc.minutes);
        assert!((inc.share + strand.share - 1.0).abs() < 1e-9);
        assert!(inc.ids.contains(&4) && inc.ids.contains(&8));
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 18)]);
        assert_eq!(segs[0].share, 1.0);
        // Scored: the strand is its own row with its own evidence; the
        // incumbent lands on the m33 task without the contoso anchors.
        let mut minutes = HashMap::new();
        minutes.insert(Key::Anchor(AnchorKind::Branch, "m33".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 30.0);
        let profiles = vec![Profile {
            task_id: 7,
            minutes,
            declared: Default::default(),
            last_ts: Some(0),
            vec: None,
        }];
        let out = decide(
            &spans,
            0,
            18 * M,
            &profiles,
            &HashMap::from([(7, "m33 work".to_owned())]),
            &HashMap::from([(7, Some("chronicle".to_owned()))]),
            &HashMap::new(),
            &[],
            &Params::default(),
            &SegParams {
                new_task_min: 4.0,
                ..accumulated
            },
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(7));
        assert!((out[0].share - 14.0 / 18.0).abs() < 1e-9);
        assert_eq!(out[0].kind, "author");
        assert!(
            matches!(&out[1].target, Target::New { project: Some(p), .. } if p == "contoso"),
            "{:?}",
            out[1].target
        );
        assert!((out[1].share - 4.0 / 18.0).abs() < 1e-9);
        assert_eq!((out[1].lo, out[1].hi), (0, 18 * M));
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

    // m33: Chronicle's own window is furniture. Its title's leading word
    // matches the chronicle repo, so without the skip it would cut m30's
    // segment and carry the word as evidence; it stretches instead.
    #[test]
    fn the_self_window_stretches_and_carries_no_evidence() {
        let own = span(2, 10, 15, "chronicle", "Chronicle", &[]);
        let spans = vec![code(1, 0, 10, "m30"), own, code(3, 15, 20, "m30")];
        let segs = segment(&spans, &[], &SegParams::default());
        assert_eq!(ranges(&segs), [(0, 20)]);
        assert_eq!(segs[0].minutes, 20.0);
        let seg = crate::profile::Segment::from_spans_skipping(&spans, 0, 20 * M, &[]);
        // The word is there from m30's own titles (15 minutes of them), not
        // the 5 the window would have added.
        assert_eq!(
            seg.keys[&Key::Term("chronicle".into())],
            15.0,
            "{:?}",
            seg.keys
        );
        assert_eq!(seg.minutes, 20.0);
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
            declared: Default::default(),
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
            &HashMap::new(),
            &HashMap::new(),
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
            &HashMap::new(),
            &HashMap::new(),
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
        // m32 chunk 1: mostly quiet while the session wrote is supervising;
        // the same span with the user typing, or a session that never
        // wrote, stays `agent`. A call is a meeting.
        let mut watch = span(
            7,
            0,
            10,
            "Terminator",
            "✳ fix tests",
            &[(AnchorKind::Session, "s1")],
        );
        watch.quiet_ms = 5 * M;
        watch.wrote = true;
        assert_eq!(
            kind_of(std::slice::from_ref(&watch), 0, 10 * M, &d),
            "supervise"
        );
        watch.quiet_ms = 4 * M;
        assert_eq!(
            kind_of(std::slice::from_ref(&watch), 0, 10 * M, &d),
            "agent"
        );
        watch.quiet_ms = 10 * M;
        watch.wrote = false;
        assert_eq!(kind_of(&[watch], 0, 10 * M, &d), "agent");
        let call = span(8, 0, 5, "Firefox", "Meet", &[(AnchorKind::Event, "call:1")]);
        assert_eq!(kind_of(&[call], 0, 5 * M, &d), "meet");
        // m33: the app's own window is `admin`; it never outweighs work,
        // and outranks a break when neither has any.
        let own = span(9, 0, 8, "chronicle", "Chronicle", &[]);
        assert_eq!(
            kind_of(&[own.clone(), code(10, 8, 10, "m30")], 0, 10 * M, &d),
            "author"
        );
        assert_eq!(
            kind_of(&[own.clone(), spans[2].clone()], 0, 20 * M, &d),
            "admin"
        );
        assert_eq!(kind_of(&[own], 0, 8 * M, &d), "admin");
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
            &HashMap::new(),
            &HashMap::new(),
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0].lo, out[0].hi), (0, 40 * M));
        assert!(!out[0].confident, "the folded stretch makes the row unsure");
    }

    /// A chronicle task that absorbed a morning of contoso work cannot take
    /// the next contoso stretch (m33 chunk C), unless its own label names
    /// the ticket on screen or it has no project at all.
    #[test]
    fn decide_vetoes_a_task_outside_the_segment_place() {
        let contoso = |id: i64, lo: i64, hi: i64| {
            span(
                id,
                lo,
                hi,
                "Code",
                "tasks.py - contoso",
                &[
                    (AnchorKind::Place, "contoso"),
                    (AnchorKind::Branch, "ACME-1"),
                    (AnchorKind::Item, "ACME-1"),
                ],
            )
        };
        let spans = vec![contoso(1, 0, 12), contoso(2, 12, 24)];
        let mut minutes = HashMap::new();
        minutes.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Branch, "m30".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Branch, "ACME-1".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()), 30.0);
        let profile = Profile {
            task_id: 7,
            minutes,
            declared: Default::default(),
            last_ts: Some(0),
            vec: None,
        };
        let labels = HashMap::from([(7, "m30 work".to_owned())]);
        let run = |profiles: &[Profile], projects: &HashMap<i64, Option<String>>| {
            decide(
                &spans,
                0,
                24 * M,
                profiles,
                &labels,
                projects,
                &HashMap::new(),
                &[],
                &Params::default(),
                &SegParams::default(),
            )
        };
        let chronicle = HashMap::from([(7, Some("chronicle".to_owned()))]);
        let out = run(std::slice::from_ref(&profile), &chronicle);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            matches!(&out[0].target, Target::New { project: Some(p), .. } if p == "contoso"),
            "{:?}",
            out[0].target
        );
        assert_eq!(out[0].runner_up, None, "the vetoed task is no runner-up");
        // The label names the ticket: one task across repos.
        let mut declared = profile.clone();
        declared
            .declared
            .insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()));
        let out = run(std::slice::from_ref(&declared), &chronicle);
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
        // No project: nothing to veto.
        let out = run(std::slice::from_ref(&profile), &HashMap::from([(7, None)]));
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
    }

    /// Task 7 is chronicle work on `m30`, task 9 is contoso work on
    /// ACME-1; both profiles carry their place and branch.
    fn two_profiles() -> Vec<Profile> {
        let mut a = HashMap::new();
        a.insert(Key::Anchor(AnchorKind::Branch, "m30".into()), 30.0);
        a.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 30.0);
        let mut b = HashMap::new();
        b.insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()), 30.0);
        b.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 30.0);
        vec![
            Profile {
                task_id: 7,
                minutes: a,
                declared: Default::default(),
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 9,
                minutes: b,
                declared: Default::default(),
                last_ts: Some(0),
                vec: None,
            },
        ]
    }

    fn agent_span(id: i64, lo: i64, hi: i64, session: &str, place: &str) -> AnchoredSpan {
        let mut s = span(
            id,
            lo,
            hi,
            "Terminator",
            "✳ work",
            &[(AnchorKind::Session, session), (AnchorKind::Place, place)],
        );
        s.wrote = true;
        s
    }

    fn live(
        id: &str,
        writes: &[i64],
        prompts: &[i64],
        anchors: &[(AnchorKind, &str)],
    ) -> LiveSession {
        LiveSession {
            id: id.into(),
            title: None,
            writes: writes.iter().map(|m| m * M).collect(),
            prompts: prompts.iter().map(|m| m * M).collect(),
            anchors: anchors
                .iter()
                .map(|(k, v)| Anchor {
                    kind: *k,
                    value: (*v).to_owned(),
                })
                .collect(),
        }
    }

    fn whole(lo: i64, hi: i64, task: i64, kind: &str) -> Placement {
        Placement {
            lo: lo * M,
            hi: hi * M,
            target: Target::Existing(task),
            confidence: 0.8,
            confident: true,
            reason: "evidence".into(),
            margin: 0.3,
            runner_up: None,
            kind: kind.into(),
            share: 1.0,
            strand: false,
        }
    }

    #[test]
    fn concurrent_sessions_split_a_segment_by_prompts() {
        let profiles = two_profiles();
        let spans = vec![
            agent_span(1, 0, 30, "s1", "chronicle"),
            agent_span(2, 30, 40, "s2", "contoso"),
        ];
        let sessions = vec![
            live("s1", &[5, 15, 25], &[1, 10, 20], &[]),
            live("s2", &[32, 38], &[31], &[]),
        ];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "agent")],
            &spans,
            &sessions,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(7));
        assert_eq!(out[0].share, 0.75);
        assert_eq!(out[1].target, Target::Existing(9));
        assert_eq!(out[1].share, 0.25);
        assert!(out.iter().all(|p| (p.lo, p.hi) == (0, 40 * M)));
        assert!(out.iter().all(|p| p.kind == "agent"), "{out:?}");
        assert!(
            out[1].reason.contains("1 of 4 prompts"),
            "{}",
            out[1].reason
        );
    }

    #[test]
    fn a_session_never_on_screen_is_supervised_from_its_own_scope() {
        let profiles = two_profiles();
        let spans = vec![agent_span(1, 0, 40, "s1", "chronicle")];
        let sessions = vec![
            live("s1", &[5, 25], &[1], &[]),
            live(
                "s2",
                &[10, 30],
                &[2],
                &[
                    (AnchorKind::Session, "s2"),
                    (AnchorKind::Place, "contoso"),
                    (AnchorKind::Item, "ACME-1"),
                ],
            ),
        ];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "agent")],
            &spans,
            &sessions,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(
            (out[1].target.clone(), out[1].kind.as_str()),
            (Target::Existing(9), "supervise")
        );
        assert_eq!(out[0].share + out[1].share, 1.0);
    }

    #[test]
    fn focus_shares_stand_in_when_no_prompt_was_kept() {
        let profiles = two_profiles();
        let spans = vec![
            agent_span(1, 0, 10, "s1", "chronicle"),
            agent_span(2, 10, 40, "s2", "contoso"),
        ];
        let sessions = vec![live("s1", &[5], &[], &[]), live("s2", &[20], &[], &[])];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "supervise")],
            &spans,
            &sessions,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].share, 0.25);
        assert_eq!(out[1].share, 0.75);
        assert!(out[0].reason.contains("25% of focus"), "{}", out[0].reason);
    }

    #[test]
    fn segments_with_one_live_session_or_no_agent_work_pass_whole() {
        let profiles = two_profiles();
        let spans = vec![
            agent_span(1, 0, 30, "s1", "chronicle"),
            agent_span(2, 30, 40, "s2", "contoso"),
        ];
        // s2 last wrote long before the segment.
        let far = vec![
            live("s1", &[5, 25], &[1], &[]),
            live("s2", &[-10], &[-11], &[]),
        ];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "agent")],
            &spans,
            &far,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].share, 1.0);
        // Both live, but the segment is not agent work.
        let both = vec![
            live("s1", &[5, 25], &[1], &[]),
            live("s2", &[35], &[31], &[]),
        ];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "author")],
            &spans,
            &both,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 1);
        // Both live and both scoring to one task: one whole row.
        let same = vec![
            agent_span(1, 0, 30, "s1", "chronicle"),
            agent_span(2, 30, 40, "s2", "chronicle"),
        ];
        let out = split_concurrent(
            vec![whole(0, 40, 7, "agent")],
            &same,
            &both,
            &profiles,
            &HashMap::new(),
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].share, 1.0);
    }

    /// A catch-all that absorbed a repo does not keep that repo's sessions:
    /// they go to the best task in their own repo, else a new one there
    /// (one per repo across the window); a ticket the task holds keeps a
    /// session from another repo; a sliver of a repo folds back.
    #[test]
    fn a_session_foreign_to_its_verdicts_project_goes_to_its_own_repo() {
        // Task 85 is a mailer task that has learned chronicle too.
        let mut a = HashMap::new();
        a.insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()), 60.0);
        a.insert(Key::Anchor(AnchorKind::Place, "mailer".into()), 60.0);
        a.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 60.0);
        a.insert(
            Key::Anchor(AnchorKind::Branch, "chronicle@main".into()),
            60.0,
        );
        let profiles = vec![Profile {
            task_id: 85,
            minutes: a,
            declared: Default::default(),
            last_ts: Some(0),
            vec: None,
        }];
        let projects = HashMap::from([(85, Some("mailer".to_owned()))]);
        let spans = vec![
            agent_span(1, 0, 30, "s1", "chronicle"),
            agent_span(2, 30, 40, "s2", "contoso"),
            agent_span(3, 40, 80, "s3", "chronicle"),
        ];
        let mut nyc = live("s2", &[32, 38, 42], &[31, 35, 41], &[]);
        nyc.anchors.push(Anchor {
            kind: AnchorKind::Item,
            value: "ACME-1".into(),
        });
        let mut s1 = live("s1", &[5, 15, 25], &[1, 10], &[]);
        s1.title = Some("✳ fix the segmenter".into());
        let sessions = vec![s1, nyc, live("s3", &[45, 70], &[41, 60], &[])];
        let out = split_concurrent(
            vec![whole(0, 40, 85, "agent"), whole(40, 80, 85, "agent")],
            &spans,
            &sessions,
            &profiles,
            &projects,
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 4, "{out:?}");
        // contoso session: task 85 holds its ticket, so it stays.
        assert_eq!(out[0].target, Target::Existing(85));
        assert_eq!(out[0].share, 0.6);
        let Target::New {
            label,
            project,
            cluster,
        } = &out[1].target
        else {
            panic!("{out:?}")
        };
        assert_eq!(
            (label.as_str(), project.as_deref()),
            ("fix the segmenter", Some("chronicle"))
        );
        assert_eq!(out[1].share, 0.4);
        // The second segment's chronicle session joins the same new task.
        assert_eq!(out[2].target, Target::Existing(85));
        assert_eq!(
            out[3].target,
            Target::New {
                label: label.clone(),
                project: project.clone(),
                cluster: *cluster
            }
        );
        assert!((out[3].share - 2.0 / 3.0).abs() < 1e-9, "{out:?}");
        // A sliver of a repo (2 of 20 prompts over 40 min = 4 min) folds back.
        let sliver = vec![
            live(
                "s1",
                &[5, 15, 25],
                &[
                    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
                ],
                &[],
            ),
            {
                let mut s = live("s2", &[32, 38], &[31, 35], &[]);
                s.anchors.push(Anchor {
                    kind: AnchorKind::Item,
                    value: "ACME-1".into(),
                });
                s
            },
        ];
        let flipped = HashMap::from([(85, Some("chronicle".to_owned()))]);
        let mut a = HashMap::new();
        a.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 60.0);
        a.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 60.0);
        let profiles = vec![Profile {
            task_id: 85,
            minutes: a,
            declared: Default::default(),
            last_ts: Some(0),
            vec: None,
        }];
        let out = split_concurrent(
            vec![whole(0, 40, 85, "agent")],
            &spans[..2],
            &sliver,
            &profiles,
            &flipped,
            &HashMap::new(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].share, 1.0);
    }
}
