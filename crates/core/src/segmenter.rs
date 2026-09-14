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

use std::collections::{BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use regex::Regex;
use rusqlite::Connection;

use crate::config::Config;
use crate::extract::{Anchor, AnchorKind, Family, Strength};
use crate::profile::{self, AnchoredSpan, Key, Params, Profile, Segment, Verdict};
use crate::project::Matcher;
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
    /// The spans counted on this segment, by id.
    pub ids: Vec<i64>,
    /// This segment's share of its range (m35 chunk 2): 1 when its project
    /// alone was on screen, less when other projects' spans interleave
    /// with it — its focus minutes over every span's inside the range.
    pub share: f64,
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

/// Cut `spans` (focus, sorted by start; one project's, m35 chunk 2) into
/// segments.
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
            run = None;
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
            // The excursion folded back: its time stays on the segment.
            run = None;
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
        // Foreign to the segment: the run grows until it earns a cut.
        let r = run.get_or_insert_with(|| Seg::empty(s.start_ts));
        r.add(s.id, &sig, dur, s.end_ts);
        if r.minutes >= p.switch_min {
            let start = r.lo;
            let next = run.take().expect("just inserted");
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
    /// The project's general task (m35 chunk 1): its time no task of it
    /// claims. Created on first use by the window write.
    General(String),
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
    /// segment, less when other projects interleave with it (m35 chunk
    /// 2) or concurrent AI sessions split it (m32 chunk 3).
    pub share: f64,
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
    })
}

/// The project sinks (m35 chunk 1): per project, the declared task the
/// person marked current, else the newest open declared one; and the
/// projects that mint no derived tasks.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sinks {
    /// The sink per project, keyed by the project lowercased.
    pub current: HashMap<String, i64>,
    /// Lowercased projects configured `derive = false`.
    pub no_derive: HashSet<String>,
    /// Tasks the person closed inside the window: id to `closed_ts`. A
    /// close is scoped by time: the task is a candidate, and its
    /// project's sink, for the segments before it and out of the running
    /// after.
    pub closed: HashMap<i64, i64>,
    /// Per project, its declared tasks oldest first, so the sink is
    /// answered as of each segment; `current` is the answer when none
    /// of them was open at the segment.
    pub history: HashMap<String, Vec<storage::DeclaredTask>>,
}

impl Sinks {
    /// `declared` oldest first; `projects` is every task's project as
    /// [`crate::project::normalize_projects`] leaves it; `closed` the
    /// tasks the person closed inside the window.
    pub fn build(
        declared: &[storage::DeclaredTask],
        projects: &HashMap<i64, Option<String>>,
        matcher: &Matcher,
        closed: HashMap<i64, i64>,
    ) -> Sinks {
        let mut current = HashMap::new();
        let mut flagged: HashSet<String> = HashSet::new();
        let mut history: HashMap<String, Vec<storage::DeclaredTask>> = HashMap::new();
        for d in declared {
            let Some(Some(project)) = projects.get(&d.id) else {
                continue;
            };
            let key = project.to_ascii_lowercase();
            history.entry(key.clone()).or_default().push(d.clone());
            if d.closed_ts.is_some() || flagged.contains(&key) {
                continue;
            }
            current.insert(key.clone(), d.id);
            if d.current {
                flagged.insert(key);
            }
        }
        let no_derive = matcher
            .projects
            .iter()
            .filter(|p| !p.derive)
            .map(|p| p.name.to_ascii_lowercase())
            .collect();
        Sinks {
            current,
            no_derive,
            closed,
            history,
        }
    }

    /// The project's sink for a segment over `[lo, hi)`: among its
    /// declared tasks open at the segment's midpoint (declared by then,
    /// not closed before it), the flagged one, else the newest; with
    /// none, the project's current task, so a task declared late still
    /// takes the work before it when no other declared task was open.
    pub fn current_in(&self, project: &str, lo: i64, hi: i64) -> Option<i64> {
        let key = project.to_ascii_lowercase();
        let mid = midpoint(lo, hi);
        let open: Vec<&storage::DeclaredTask> = self
            .history
            .get(&key)
            .map(|h| {
                h.iter()
                    .filter(|d| d.created_ts <= mid && d.closed_ts.is_none_or(|c| c >= mid))
                    .collect()
            })
            .unwrap_or_default();
        open.iter()
            .find(|d| d.current)
            .or(open.last())
            .map(|d| d.id)
            .or_else(|| self.current.get(&key).copied())
    }

    /// Whether `task` may take a segment over `[lo, hi)`: always, unless
    /// the person closed it before the segment's midpoint. A segment is
    /// one content-continuous stretch, so one mostly after the close is
    /// work that went on past it.
    pub fn admits(&self, task: i64, lo: i64, hi: i64) -> bool {
        self.closed
            .get(&task)
            .is_none_or(|&c| midpoint(lo, hi) <= c)
    }

    /// `candidates` less the tasks closed before `[lo, hi)`; borrowed
    /// when no close is in the window.
    fn admitted<'a>(
        &self,
        candidates: &'a [Profile],
        lo: i64,
        hi: i64,
    ) -> std::borrow::Cow<'a, [Profile]> {
        if self.closed.is_empty() {
            return std::borrow::Cow::Borrowed(candidates);
        }
        std::borrow::Cow::Owned(
            candidates
                .iter()
                .filter(|pr| self.admits(pr.task_id, lo, hi))
                .cloned()
                .collect(),
        )
    }

    pub fn derives(&self, project: &str) -> bool {
        !self.no_derive.contains(&project.to_ascii_lowercase())
    }
}

/// The candidates per project (m35 chunk 1): a segment scores against the
/// tasks of its own project only, an unfiled one against the tasks with
/// none. The scorer's shared-key discount is then per project, so a place
/// every task in a repo carries decides nothing while a ticket one holds
/// does.
fn by_project(
    profiles: &[Profile],
    projects: &HashMap<i64, Option<String>>,
) -> HashMap<Option<String>, Vec<Profile>> {
    let mut out: HashMap<Option<String>, Vec<Profile>> = HashMap::new();
    for pr in profiles {
        let project = projects.get(&pr.task_id).cloned().flatten();
        out.entry(project).or_default().push(pr.clone());
    }
    out
}

/// Put `task` at the top of the verdict, whatever the scorer preferred.
/// The scorer's ranking stays underneath (confidence and margin still
/// describe its view, so the row reads as uncertain when it disagreed and
/// the runner-up stays one click away).
fn midpoint(lo: i64, hi: i64) -> i64 {
    lo + (hi - lo) / 2
}

fn crown(v: &mut Verdict, task: i64) {
    let score = v
        .ranked
        .iter()
        .find(|c| c.task_id == task)
        .map_or(0.0, |c| c.score);
    v.ranked.retain(|c| c.task_id != task);
    v.ranked.insert(
        0,
        profile::Candidate {
            task_id: task,
            score,
        },
    );
    let runner = v.ranked.get(1).map_or(0.0, |c| c.score).max(v.new_task);
    v.confident = v.best == Some(task) && v.confident;
    v.best = Some(task);
    v.margin = score - runner;
}

/// The task of `candidates` holding one of `items`: the best-ranked such
/// task, else any. `(task, the key it holds)`.
fn holder(v: &Verdict, candidates: &[Profile], items: &[Key]) -> Option<(i64, String)> {
    let held = |pr: &Profile| {
        items
            .iter()
            .find(|k| pr.minutes.contains_key(*k))
            .map(|k| k.value().to_owned())
    };
    v.ranked
        .iter()
        .map(|c| c.task_id)
        .chain(candidates.iter().map(|pr| pr.task_id))
        .find_map(|t| {
            candidates
                .iter()
                .find(|pr| pr.task_id == t)
                .and_then(held)
                .map(|k| (t, k))
        })
}

/// The sink order inside a project (m35 chunk 1): the current declared
/// task, unless the segment carries a ticket it does not hold and another
/// task of the project does; else the task holding that ticket; else
/// whatever the scorer ranked first among the project's tasks — and when
/// it ranked none, a new task, then the project's other work (the caller).
/// Returns the reason when a rule overrode the scorer.
fn sink_order(
    v: &mut Verdict,
    seg: &Segment,
    candidates: &[Profile],
    project: Option<&str>,
    sinks: &Sinks,
) -> Option<String> {
    let project = project?;
    let items: Vec<Key> = seg
        .keys
        .keys()
        .filter(|k| matches!(k, Key::Anchor(AnchorKind::Item, _)))
        .cloned()
        .collect();
    let holder = holder(v, candidates, &items);
    if let Some(cur) = sinks.current_in(project, seg.start_ts, seg.end_ts) {
        let cur_holds = candidates
            .iter()
            .any(|pr| pr.task_id == cur && items.iter().any(|k| pr.minutes.contains_key(k)));
        if cur_holds || holder.is_none() {
            crown(v, cur);
            return Some(format!("declared in {project}"));
        }
    }
    let (task, key) = holder?;
    crown(v, task);
    Some(format!("holds {key}"))
}

/// Spans grouped by project (m35 chunk 2), each group in time order, so
/// every project is cut and scored on its own spans. Chronicle's own
/// window and a distraction belong to the project of the span before
/// them (else after): time on whatever they interrupted, as a glance
/// always was. Groups come in order of first span.
fn partition(
    spans: &[AnchoredSpan],
    distractions: &[Regex],
) -> Vec<(Option<String>, Vec<AnchoredSpan>)> {
    let mut owner: Vec<Option<String>> = Vec::with_capacity(spans.len());
    let mut last: Option<Option<String>> = None;
    let mut leading: Vec<usize> = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        if crate::evidence::is_furniture(&s.app, &s.title, distractions) {
            owner.push(last.clone().unwrap_or_default());
            if last.is_none() {
                leading.push(i);
            }
        } else {
            last = Some(s.project.clone());
            owner.push(s.project.clone());
            for j in leading.drain(..) {
                owner[j] = s.project.clone();
            }
        }
    }
    let mut out: Vec<(Option<String>, Vec<AnchoredSpan>)> = Vec::new();
    for (s, p) in spans.iter().zip(owner) {
        match out.iter_mut().find(|(q, _)| *q == p) {
            Some((_, v)) => v.push(s.clone()),
            None => out.push((p, vec![s.clone()])),
        }
    }
    out
}

/// Decide every segment in `spans` over `[lo, hi)`. Each project's spans
/// are cut and scored on their own (m35 chunk 2), against the tasks of
/// that project under the sink order; a segment's `share` is its focus
/// minutes over every span's inside its range, so projects interleaved
/// minute by minute are each their own rows over the same stretch. The
/// "new" stretches cluster by what they share inside one project, and a
/// cluster with `new_task_min` focus minutes becomes one new task there;
/// a short "new" stretch left over between two placements on one task
/// joins that task (unsure); what is still new inside a project is the
/// project's other work. Contiguous whole placements on one target merge
/// into one row. Rows come out in time order.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    spans: &[AnchoredSpan],
    lo: i64,
    hi: i64,
    profiles: &[Profile],
    labels: &HashMap<i64, String>,
    projects: &HashMap<i64, Option<String>>,
    sinks: &Sinks,
    distractions: &[Regex],
    params: &Params,
    sp: &SegParams,
) -> Vec<Placement> {
    let buckets = by_project(profiles, projects);
    let none: Vec<Profile> = Vec::new();
    let focus = |ss: &[AnchoredSpan], a: i64, b: i64| -> f64 {
        ss.iter()
            .filter(|s| s.end_ts > a && s.start_ts < b)
            .map(|s| minutes(s.end_ts.min(b) - s.start_ts.max(a)))
            .sum()
    };
    let parts = partition(spans, distractions);
    // 1. Cut and score each project's spans on their own.
    let mut scored: Vec<(Seg, Segment, Verdict)> = Vec::new();
    // Per scored segment: its project and the reason a sink rule gave,
    // and its partition, so the rules that look at neighbours stay
    // inside one project.
    let mut about: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut part: Vec<usize> = Vec::new();
    for (pi, (project, own)) in parts.iter().enumerate() {
        let candidates = buckets.get(project).unwrap_or(&none);
        for mut seg in segment(own, distractions, sp) {
            seg.lo = seg.lo.max(lo);
            seg.hi = seg.hi.min(hi);
            if seg.hi <= seg.lo {
                continue;
            }
            let evidence = Segment::from_spans_skipping(own, seg.lo, seg.hi, distractions);
            if evidence.keys.is_empty() {
                continue;
            }
            let (mine, all) = (focus(own, seg.lo, seg.hi), focus(spans, seg.lo, seg.hi));
            seg.share = if all <= 0.0 || all - mine < 1e-9 {
                1.0
            } else {
                (mine / all).clamp(0.0, 1.0)
            };
            let candidates = sinks.admitted(candidates, seg.lo, seg.hi);
            let mut v = profile::score(&evidence, &candidates, params);
            let reason = sink_order(&mut v, &evidence, &candidates, project.as_deref(), sinks);
            about.push((project.clone(), reason));
            part.push(pi);
            scored.push((seg, evidence, v));
        }
    }
    // 2. Place on existing tasks.
    let mut placed: Vec<Option<Placement>> = scored
        .iter()
        .zip(&about)
        .map(|((seg, ev, v), (_, reason))| {
            let mut p = place_existing(ev, seg.share, v, labels)?;
            if let Some(reason) = reason {
                p.reason = reason.clone();
            }
            Some(p)
        })
        .collect();
    // 3. Cluster what is new by what it shares, inside one project; a
    //    cluster with enough minutes is one new task, however scattered
    //    its stretches. A project that does not derive mints nothing.
    let mut clusters: Vec<(Seg, Vec<usize>, Option<String>)> = Vec::new();
    for (i, (seg, ..)) in scored.iter().enumerate() {
        if placed[i].is_some() {
            continue;
        }
        let project = &about[i].0;
        if project.as_deref().is_some_and(|p| !sinks.derives(p)) {
            continue;
        }
        match clusters
            .iter_mut()
            .find(|(c, _, cp)| cp == project && c.akin(seg))
        {
            Some((c, members, _)) => {
                c.absorb(seg);
                members.push(i);
            }
            None => clusters.push((seg.clone(), vec![i], project.clone())),
        }
    }
    for (ci, (c, members, project)) in clusters.iter().enumerate() {
        if c.minutes < sp.new_task_min {
            continue;
        }
        // The placeholder until the naming job runs (m35 chunk 5): the
        // project and "new work", never a window title — a title read as
        // a label was the m30 leak of raw screen text into task lists.
        let label = placeholder_label(project.as_deref());
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
            });
        }
    }
    // 3b. A short new stretch still unplaced, sandwiched by one task in
    //     its own project, goes to that task as an excursion (unsure).
    for i in 0..scored.len() {
        if placed[i].is_some() || scored[i].0.minutes >= sp.new_task_min {
            continue;
        }
        let beside = |j: usize| (part[j] == part[i]).then(|| placed[j].as_ref()).flatten();
        let (Some(prev), Some(next)) = (
            i.checked_sub(1).and_then(beside),
            (i + 1 < scored.len()).then(|| beside(i + 1)).flatten(),
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
            share: seg.share,
        });
    }
    // 3c. What is still new inside a project is the project's other work
    //     (m35 chunk 1). Unfiled new work stays unplaced, as before.
    for i in 0..scored.len() {
        if placed[i].is_some() {
            continue;
        }
        let Some(project) = about[i].0.clone() else {
            continue;
        };
        let (seg, _, v) = &scored[i];
        placed[i] = Some(Placement {
            lo: seg.lo,
            hi: seg.hi,
            target: Target::General(project.clone()),
            confidence: v.new_task,
            confident: false,
            reason: format!("other work in {project}"),
            margin: 0.0,
            runner_up: v.ranked.first().map(|c| c.task_id),
            kind: String::new(),
            share: seg.share,
        });
    }
    // 4. Inside one project, contiguous whole rows on one target become
    //    one; every row takes the kind of its own project's spans; then
    //    time order.
    let mut out: Vec<(usize, Placement)> = Vec::new();
    for (i, p) in placed.into_iter().enumerate() {
        let Some(p) = p else {
            continue;
        };
        if let Some((pj, last)) = out.last_mut()
            && *pj == part[i]
            && last.target == p.target
            && last.share >= 1.0
            && p.share >= 1.0
            && p.lo - last.hi < AFK_GAP_MS
        {
            last.hi = last.hi.max(p.hi);
            last.confident = last.confident && p.confident;
            last.confidence = last.confidence.min(p.confidence);
            last.margin = last.margin.min(p.margin);
            continue;
        }
        out.push((part[i], p));
    }
    for (pi, p) in &mut out {
        p.kind = kind_of(&parts[*pi].1, p.lo, p.hi, distractions).to_owned();
    }
    out.sort_by_key(|(pi, p)| (p.lo, p.hi, *pi));
    out.into_iter().map(|(_, p)| p).collect()
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
/// placement with two or more sessions of its own project writing within
/// [`CONCURRENT_MS`] of it (m35 chunk 2: a row is one project's, and a
/// session's project is the one its spans on screen are filed into, else
/// what the rules make of its scope) becomes one row per task, each over
/// the whole range with the row's `share` divided by the prompts typed
/// into that task's sessions (their focus time when no prompt was kept).
/// A session's task follows the sink order of its project: the current
/// declared task, a ticket holder, what its own spans score to among the
/// project's tasks, else a new task there (one per project across the
/// window, given `new_task_min` of shared time; less stays with the
/// segment's target) or the project's other work when it does not
/// derive. A session with no project stays with the segment's target.
/// The row's kind is its own sessions' (`supervise` for one never on
/// screen). Every other placement passes through whole.
#[allow(clippy::too_many_arguments)]
pub fn split_concurrent(
    placements: Vec<Placement>,
    spans: &[AnchoredSpan],
    sessions: &[LiveSession],
    profiles: &[Profile],
    projects: &HashMap<i64, Option<String>>,
    sinks: &Sinks,
    matcher: &Matcher,
    params: &Params,
    sp: &SegParams,
    distractions: &[Regex],
) -> Vec<Placement> {
    let buckets = by_project(profiles, projects);
    let none: Vec<Profile> = Vec::new();
    let mut out = Vec::with_capacity(placements.len());
    // Projects the split has opened a new task for, with its label, in
    // order: the cluster id.
    let mut opened: Vec<(String, String)> = Vec::new();
    for p in placements {
        if sessions.len() < 2 || !matches!(p.kind.as_str(), "agent" | "supervise") {
            out.push(p);
            continue;
        }
        // The row is one project's (m35 chunk 2); only that project's
        // sessions share it.
        let project: Option<String> = match &p.target {
            Target::Existing(id) => projects.get(id).cloned().flatten(),
            Target::General(pr) => Some(pr.clone()),
            Target::New { project, .. } => project.clone(),
        };
        let (lo, hi) = (p.lo - CONCURRENT_MS, p.hi + CONCURRENT_MS);
        let within = |t: &i64| (lo..=hi).contains(t);
        let mut live: Vec<&LiveSession> = Vec::new();
        let mut owned: Vec<Vec<AnchoredSpan>> = Vec::new();
        for s in sessions.iter().filter(|s| s.writes.iter().any(within)) {
            let own: Vec<AnchoredSpan> = spans
                .iter()
                .filter(|sp| sp.end_ts > p.lo && sp.start_ts < p.hi)
                .filter(|sp| {
                    sp.anchors
                        .iter()
                        .any(|a| a.kind == AnchorKind::Session && a.value == s.id)
                })
                .cloned()
                .collect();
            if session_project(s, &own, matcher) == project {
                live.push(s);
                owned.push(own);
            }
        }
        if live.len() < 2 {
            out.push(p);
            continue;
        }
        let candidates = sinks.admitted(buckets.get(&project).unwrap_or(&none), p.lo, p.hi);
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
            let verdict =
                session_verdict(s, &owned[i], p.lo, p.hi, &candidates, params, distractions);
            let target = session_target(
                s,
                &owned[i],
                verdict.as_ref(),
                &p.target,
                &candidates,
                (p.lo, p.hi),
                project.as_deref(),
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
        // A project's new task needs its share of the range to be worth
        // one; a sliver stays with the segment's target.
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

/// A live session's project (m35 chunk 1): the one its own spans on
/// screen are filed into, else what the rules make of its scope anchors
/// and title.
fn session_project(s: &LiveSession, owned: &[AnchoredSpan], matcher: &Matcher) -> Option<String> {
    let mut ms: HashMap<Option<&str>, i64> = HashMap::new();
    for sp in owned {
        *ms.entry(sp.project.as_deref()).or_insert(0) += sp.end_ts - sp.start_ts;
    }
    if let Some((Some(p), _)) = ms
        .into_iter()
        .max_by_key(|(p, m)| (*m, p.is_some(), std::cmp::Reverse(*p)))
    {
        return Some(p.to_owned());
    }
    matcher
        .file("", s.title.as_deref().unwrap_or(""), &s.anchors)
        .map(str::to_owned)
}

/// Where one live session goes given its verdict among its project's
/// tasks (m35 chunk 1): the current declared task, unless another task
/// of the project holds the session's ticket and it does not; else the
/// task holding that ticket; else the scorer's best; else a new task in
/// the project, one per project over the window, or the project's other
/// work when it does not derive. A session with no project scores
/// against the tasks with none and, called new, stays with the segment's
/// own target.
#[allow(clippy::too_many_arguments)]
fn session_target(
    s: &LiveSession,
    owned: &[AnchoredSpan],
    verdict: Option<&Verdict>,
    fallback: &Target,
    candidates: &[Profile],
    (lo, hi): (i64, i64),
    project: Option<&str>,
    sinks: &Sinks,
    opened: &mut Vec<(String, String)>,
) -> Target {
    let items: Vec<Key> = s
        .anchors
        .iter()
        .chain(owned.iter().flat_map(|sp| sp.anchors.iter()))
        .filter(|a| a.kind == AnchorKind::Item)
        .map(|a| Key::Anchor(AnchorKind::Item, a.value.clone()))
        .collect();
    let empty = Verdict {
        ranked: Vec::new(),
        new_task: 0.0,
        best: None,
        margin: 0.0,
        confident: false,
    };
    let holder = holder(verdict.unwrap_or(&empty), candidates, &items);
    if let Some(project) = project
        && let Some(cur) = sinks.current_in(project, lo, hi)
    {
        let cur_holds = candidates
            .iter()
            .any(|pr| pr.task_id == cur && items.iter().any(|k| pr.minutes.contains_key(k)));
        if cur_holds || holder.is_none() {
            return Target::Existing(cur);
        }
    }
    if let Some((t, _)) = holder {
        return Target::Existing(t);
    }
    if let Some(t) = verdict.and_then(|v| v.best) {
        return Target::Existing(t);
    }
    let Some(project) = project else {
        return fallback.clone();
    };
    if !sinks.derives(project) {
        return Target::General(project.to_owned());
    }
    let cluster = match opened.iter().position(|(o, _)| o == project) {
        Some(i) => i,
        None => {
            let label = s
                .title
                .as_deref()
                .map(crate::evidence::strip_glyphs)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map_or_else(|| format!("{project} session"), str::to_owned);
            opened.push((project.to_owned(), label));
            opened.len() - 1
        }
    };
    Target::New {
        label: opened[cluster].1.clone(),
        project: Some(project.to_owned()),
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

/// The label a minted task carries until its naming job runs (m35 chunk
/// 5): the project and "new work", never a window title.
pub fn placeholder_label(project: Option<&str>) -> String {
    match project {
        Some(p) => format!("{p} \u{b7} new work"),
        None => "new work".to_owned(),
    }
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
    storage::normalize_task_projects(conn, &Matcher::from_config(config))?;
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
    let matcher = Matcher::from_config(config);
    let mut projects = projects.clone();
    crate::project::normalize_projects(&mut projects, &matcher);
    let sinks = Sinks::build(
        &storage::window_declared(conn, lo)?,
        &projects,
        &matcher,
        storage::closed_user_tasks(conn, lo)?,
    );
    let placements = decide(
        &spans,
        lo,
        hi,
        profiles,
        labels,
        &projects,
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
        &projects,
        &sinks,
        &matcher,
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
    // A task the person closed inside the window takes its own stretch
    // and no later one.
    let (profiles, labels) = storage::window_profiles(conn, lo)?;
    let matcher = Matcher::from_config(config);
    let mut projects = storage::task_projects(conn)?;
    crate::project::normalize_projects(&mut projects, &matcher);
    let sinks = Sinks::build(
        &storage::window_declared(conn, lo)?,
        &projects,
        &matcher,
        storage::closed_user_tasks(conn, lo)?,
    );
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
        &matcher,
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
            project: None,
        }
    }

    /// File each span into the project its place anchor names (m35).
    fn file_by_place(spans: &mut [AnchoredSpan]) {
        for s in spans {
            s.project = s
                .anchors
                .iter()
                .find(|a| a.kind == AnchorKind::Place)
                .map(|a| a.value.clone());
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

    /// Projects interleaved minute by minute are each their own rows
    /// (m35 chunk 2): every project's spans are cut on their own, and a
    /// row's share is its focus minutes over every span's in its range.
    /// chronicle 12 min stays whole on its task; contoso's 4 × 1 min clear
    /// `new_task_min` and mint inside contoso; fabrikam-web's 2 × 1 min do not
    /// and are fabrikam-web's other work, one row each since five minutes
    /// apart is an AFK gap inside fabrikam-web. Rows come out in time order.
    #[test]
    fn interleaved_projects_are_each_their_own_rows() {
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
        let mut spans = vec![
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
        file_by_place(&mut spans);
        let mut minutes = HashMap::new();
        minutes.insert(Key::Anchor(AnchorKind::Branch, "m33".into()), 30.0);
        minutes.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 30.0);
        let profiles = vec![Profile {
            task_id: 7,
            minutes,
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
            &Sinks::default(),
            &[],
            &Params::default(),
            &SegParams {
                new_task_min: 4.0,
                ..SegParams::default()
            },
        );
        assert_eq!(out.len(), 4, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(7));
        assert_eq!((out[0].lo, out[0].hi), (0, 17 * M));
        assert!(
            (out[0].share - 12.0 / 17.0).abs() < 1e-9,
            "{}",
            out[0].share
        );
        assert_eq!(out[0].kind, "author");
        assert!(
            matches!(&out[1].target, Target::New { project: Some(p), .. } if p == "contoso"),
            "{:?}",
            out[1].target
        );
        assert_eq!((out[1].lo, out[1].hi), (3 * M, 18 * M));
        assert!((out[1].share - 4.0 / 15.0).abs() < 1e-9, "{}", out[1].share);
        // The two fabrikam-web glances are five minutes apart, an AFK gap in
        // their own partition: each a whole row of fabrikam-web's other work.
        for (p, lo) in out[2..].iter().zip([7, 13]) {
            assert_eq!(p.target, Target::General("fabrikam-web".into()), "{p:?}");
            assert_eq!((p.lo, p.hi), (lo * M, (lo + 1) * M));
            assert_eq!(p.share, 1.0);
        }
        // One project alone: a whole row, as before.
        let alone: Vec<AnchoredSpan> = spans
            .iter()
            .filter(|s| s.project.as_deref() == Some("chronicle"))
            .cloned()
            .collect();
        let out = decide(
            &alone,
            0,
            18 * M,
            &profiles,
            &HashMap::from([(7, "m33 work".to_owned())]),
            &HashMap::from([(7, Some("chronicle".to_owned()))]),
            &Sinks::default(),
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].share, 1.0);
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
            &Sinks::default(),
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
        // The placeholder is the project and "new work" (m35 chunk 5), never
        // the document's title; the naming job writes the real label.
        assert!(
            matches!(&out[1].target, Target::New { cluster: 0, label, .. } if label == "new work"),
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
            &Sinks::default(),
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
            &Sinks::default(),
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!((out[0].lo, out[0].hi), (0, 40 * M));
        assert!(!out[0].confident, "the folded stretch makes the row unsure");
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
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 9,
                minutes: b,
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
            &Sinks::default(),
            &Matcher::default(),
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
            &Sinks::default(),
            &Matcher::default(),
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
            &Sinks::default(),
            &Matcher::default(),
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
            &Sinks::default(),
            &Matcher::default(),
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
            &Sinks::default(),
            &Matcher::default(),
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
            &Sinks::default(),
            &Matcher::default(),
            &Params::default(),
            &SegParams::default(),
            &[],
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].share, 1.0);
    }

    /// Candidates are the segment project's tasks (m35 chunk 1): a task in
    /// another project never takes a segment, whatever it learned; the
    /// segment mints inside its own project, under the project's name; a
    /// project that does not derive sends it to its other work; an unfiled
    /// segment scores against the tasks with no project only.
    #[test]
    fn decide_keeps_candidates_inside_the_segment_project() {
        let contoso = |id: i64, lo: i64, hi: i64| {
            let mut s = span(
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
            );
            s.project = Some("acme".into());
            s
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
            last_ts: Some(0),
            vec: None,
        };
        let labels = HashMap::from([(7, "m30 work".to_owned())]);
        let run =
            |spans: &[AnchoredSpan], projects: &HashMap<i64, Option<String>>, sinks: &Sinks| {
                decide(
                    spans,
                    0,
                    24 * M,
                    std::slice::from_ref(&profile),
                    &labels,
                    projects,
                    sinks,
                    &[],
                    &Params::default(),
                    &SegParams::default(),
                )
            };
        let chronicle = HashMap::from([(7, Some("chronicle".to_owned()))]);
        let out = run(&spans, &chronicle, &Sinks::default());
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            matches!(&out[0].target, Target::New { project: Some(p), .. } if p == "acme"),
            "{:?}",
            out[0].target
        );
        assert_eq!(
            out[0].runner_up, None,
            "a task of another project is no runner-up"
        );
        // Inside its own project it wins as before.
        let acme = HashMap::from([(7, Some("acme".to_owned()))]);
        let out = run(&spans, &acme, &Sinks::default());
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
        // A project that does not derive: the new stretch is its other work.
        let quiet = Sinks {
            no_derive: HashSet::from(["acme".to_owned()]),
            ..Sinks::default()
        };
        let out = run(&spans, &chronicle, &quiet);
        assert_eq!(out[0].target, Target::General("acme".into()), "{out:?}");
        assert_eq!(out[0].reason, "other work in acme");
        assert!(!out[0].confident);
        // Unfiled spans: the tasks with no project are the candidates, and
        // new work there carries no project.
        let mut unfiled = spans.clone();
        for s in &mut unfiled {
            s.project = None;
        }
        let out = run(&unfiled, &HashMap::from([(7, None)]), &Sinks::default());
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
        let out = run(&unfiled, &acme, &Sinks::default());
        assert!(
            matches!(&out[0].target, Target::New { project: None, .. }),
            "{:?}",
            out[0].target
        );
    }

    /// The sink order inside a project (m35 chunk 1): the current declared
    /// task takes the project's segments over a derived task with more
    /// evidence — unless the segment carries a ticket it does not hold
    /// and another task of the project does, which takes it then.
    #[test]
    fn decide_sinks_a_project_into_its_current_task() {
        let contoso = |id: i64, lo: i64, hi: i64, ticket: bool| {
            let mut anchors = vec![
                (AnchorKind::Place, "contoso"),
                (AnchorKind::Branch, "contoso@main"),
            ];
            if ticket {
                anchors.push((AnchorKind::Item, "ACME-1"));
            }
            let mut s = span(id, lo, hi, "Code", "tasks.py - contoso", &anchors);
            s.project = Some("acme".into());
            s
        };
        let mut learned = HashMap::new();
        learned.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 60.0);
        learned.insert(Key::Anchor(AnchorKind::Branch, "contoso@main".into()), 60.0);
        learned.insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()), 60.0);
        let mut seeded = HashMap::new();
        seeded.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 10.0);
        let profiles = vec![
            Profile {
                task_id: 7,
                minutes: learned,
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 9,
                minutes: seeded,
                last_ts: None,
                vec: None,
            },
        ];
        let labels = HashMap::from([(7, "derived".to_owned()), (9, "declared".to_owned())]);
        let projects = HashMap::from([(7, Some("acme".to_owned())), (9, Some("acme".to_owned()))]);
        let sinks = Sinks {
            current: HashMap::from([("acme".to_owned(), 9)]),
            ..Sinks::default()
        };
        let run = |spans: &[AnchoredSpan]| {
            decide(
                spans,
                0,
                24 * M,
                &profiles,
                &labels,
                &projects,
                &sinks,
                &[],
                &Params::default(),
                &SegParams::default(),
            )
        };
        // No ticket on screen: the declared task, whatever the scorer says.
        let out = run(&[contoso(1, 0, 12, false), contoso(2, 12, 24, false)]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(9));
        assert_eq!(out[0].reason, "declared in acme");
        assert_eq!(
            out[0].runner_up,
            Some(7),
            "the scorer's view stays one click away"
        );
        // A ticket the sink does not hold and the derived task does.
        let out = run(&[contoso(1, 0, 12, true), contoso(2, 12, 24, true)]);
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
        assert_eq!(out[0].reason, "holds ACME-1");
        // No current task: the ticket holder still wins over the scorer.
        let out = decide(
            &[contoso(1, 0, 12, true), contoso(2, 12, 24, true)],
            0,
            24 * M,
            &profiles,
            &labels,
            &projects,
            &Sinks::default(),
            &[],
            &Params::default(),
            &SegParams::default(),
        );
        assert_eq!(out[0].target, Target::Existing(7), "{out:?}");
    }

    /// The sinks (m35 chunk 1): per project the task flagged current,
    /// else the newest declared; tasks resolve to configured names first;
    /// derive follows the config and defaults on for a project no rule
    /// knows.
    #[test]
    fn sinks_take_the_flagged_task_over_the_newest() {
        use crate::config::ProjectCfg;
        use storage::DeclaredTask;
        let declared = |id: i64, project: &str, current: bool| DeclaredTask {
            id,
            project: Some(project.to_owned()),
            current,
            created_ts: id,
            closed_ts: None,
        };
        let rows = vec![
            declared(1, "contoso", false),
            declared(2, "contoso", true),
            declared(3, "contoso", false),
            declared(4, "chronicle", false),
            declared(5, "chronicle", false),
            DeclaredTask {
                id: 6,
                project: None,
                current: true,
                created_ts: 6,
                closed_ts: None,
            },
        ];
        let matcher = Matcher::new(&[
            ProjectCfg {
                name: "acme".into(),
                derive: false,
                ..ProjectCfg::default()
            },
            ProjectCfg {
                name: "chronicle".into(),
                ..ProjectCfg::default()
            },
        ]);
        let mut projects: HashMap<i64, Option<String>> =
            rows.iter().map(|d| (d.id, d.project.clone())).collect();
        // `contoso` is no configured name: as a place it would resolve to
        // acme through a repo path; without one it is unfiled.
        crate::project::normalize_projects(&mut projects, &matcher);
        assert_eq!(projects[&1], None);
        assert_eq!(projects[&4].as_deref(), Some("chronicle"));
        projects.insert(1, Some("acme".into()));
        projects.insert(2, Some("acme".into()));
        projects.insert(3, Some("acme".into()));
        let sinks = Sinks::build(&rows, &projects, &matcher, HashMap::new());
        assert_eq!(sinks.current_in("acme", 0, M), Some(2));
        assert_eq!(sinks.current_in("Chronicle", 0, M), Some(5));
        assert_eq!(sinks.current_in("sprog", 0, M), None);
        assert!(!sinks.derives("acme"));
        assert!(sinks.derives("chronicle"));
        assert!(sinks.derives("sprog"));
    }

    /// A close is scoped by time: the task the person closed at T takes
    /// the project's segments before T as its declared sink and none
    /// after; with a task declared later, the sink before T is the one
    /// open then, after T the new one.
    #[test]
    fn decide_scopes_a_close_by_time() {
        use storage::DeclaredTask;
        let contoso = |id: i64, lo: i64, hi: i64| {
            let mut s = span(
                id,
                lo,
                hi,
                "Code",
                "tasks.py - contoso",
                &[(AnchorKind::Place, "contoso")],
            );
            s.project = Some("acme".into());
            s
        };
        let seeded = || {
            let mut m = HashMap::new();
            m.insert(Key::Anchor(AnchorKind::Place, "contoso".into()), 10.0);
            m
        };
        let profile = |task_id: i64| Profile {
            task_id,
            minutes: seeded(),
            last_ts: None,
            vec: None,
        };
        let labels = HashMap::from([(9, "x".to_owned()), (11, "y".to_owned())]);
        let projects = HashMap::from([(9, Some("acme".to_owned())), (11, Some("acme".to_owned()))]);
        let matcher = Matcher::default();
        let x = DeclaredTask {
            id: 9,
            project: Some("acme".into()),
            current: false,
            created_ts: M,
            closed_ts: Some(13 * M),
        };
        let closed = HashMap::from([(9, 13 * M)]);
        // An AFK gap either side of the close: two segments.
        let spans = [contoso(1, 0, 10), contoso(2, 16, 26)];
        let run = |profiles: &[Profile], sinks: &Sinks| {
            decide(
                &spans,
                0,
                30 * M,
                profiles,
                &labels,
                &projects,
                sinks,
                &[],
                &Params::default(),
                &SegParams::default(),
            )
        };
        let sinks = Sinks::build(&[x.clone()], &projects, &matcher, closed.clone());
        assert!(sinks.admits(9, 0, 10 * M));
        assert!(!sinks.admits(9, 16 * M, 26 * M));
        assert!(sinks.admits(11, 16 * M, 26 * M));
        let out = run(&[profile(9)], &sinks);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!((out[0].lo, out[0].hi), (0, 10 * M));
        assert_eq!(out[0].target, Target::Existing(9));
        assert_eq!(out[0].reason, "declared in acme");
        assert_eq!((out[1].lo, out[1].hi), (16 * M, 26 * M));
        assert_ne!(out[1].target, Target::Existing(9), "{out:?}");
        // Y declared after the close: the sink before T is X, after T Y.
        let y = DeclaredTask {
            id: 11,
            project: Some("acme".into()),
            current: false,
            created_ts: 14 * M,
            closed_ts: None,
        };
        let sinks = Sinks::build(&[x, y], &projects, &matcher, closed);
        assert_eq!(sinks.current_in("acme", 0, 10 * M), Some(9));
        assert_eq!(sinks.current_in("acme", 16 * M, 26 * M), Some(11));
        let out = run(&[profile(9), profile(11)], &sinks);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(9));
        assert_eq!(out[1].target, Target::Existing(11));
        assert!(
            out.iter().all(|p| p.reason == "declared in acme"),
            "{out:?}"
        );
    }

    /// The session split under a close scoped by time: a session with
    /// no ticket goes to the sink as of the row, the task closed at T
    /// before T and the one declared after it afterwards.
    #[test]
    fn split_concurrent_scopes_a_close_by_time() {
        use storage::DeclaredTask;
        let with = |items: &[&str]| {
            let mut m = HashMap::new();
            m.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 60.0);
            for i in items {
                m.insert(Key::Anchor(AnchorKind::Item, (*i).to_owned()), 60.0);
            }
            m
        };
        let profiles = vec![
            Profile {
                task_id: 85,
                minutes: with(&["ACME-1"]),
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 86,
                minutes: with(&[]),
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 87,
                minutes: with(&["ACME-3"]),
                last_ts: Some(0),
                vec: None,
            },
        ];
        let projects: HashMap<i64, Option<String>> = [85, 86, 87]
            .into_iter()
            .map(|id| (id, Some("chronicle".to_owned())))
            .collect();
        let declared = |id: i64, created_ts: i64, closed_ts: Option<i64>| DeclaredTask {
            id,
            project: Some("chronicle".into()),
            current: false,
            created_ts,
            closed_ts,
        };
        let sinks = Sinks::build(
            &[declared(85, 0, Some(13 * M)), declared(86, 14 * M, None)],
            &projects,
            &Matcher::default(),
            HashMap::from([(85, 13 * M)]),
        );
        let mut spans = vec![
            agent_span(1, 0, 10, "s1", "chronicle"),
            agent_span(2, 0, 10, "s3", "chronicle"),
            agent_span(3, 16, 26, "s1", "chronicle"),
            agent_span(4, 16, 26, "s3", "chronicle"),
        ];
        file_by_place(&mut spans);
        let sessions = [
            live("s1", &[2, 8, 18, 24], &[1, 17], &[]),
            live(
                "s3",
                &[3, 7, 19, 23],
                &[2, 18],
                &[(AnchorKind::Session, "s3"), (AnchorKind::Item, "ACME-3")],
            ),
        ];
        let split = |row: Placement| -> Vec<i64> {
            let mut ids: Vec<i64> = split_concurrent(
                vec![row],
                &spans,
                &sessions,
                &profiles,
                &projects,
                &sinks,
                &Matcher::default(),
                &Params::default(),
                &SegParams::default(),
                &[],
            )
            .iter()
            .filter_map(|p| match p.target {
                Target::Existing(id) => Some(id),
                _ => None,
            })
            .collect();
            ids.sort();
            ids
        };
        assert_eq!(split(whole(0, 10, 85, "agent")), [85, 87]);
        assert_eq!(split(whole(16, 26, 85, "agent")), [86, 87]);
    }

    /// Sessions share only rows of their own project (m35 chunk 2): a
    /// contoso session never splits a chronicle row, whatever ticket it
    /// carries; two chronicle sessions divide one by their own tickets
    /// and evidence, then by the project's current declared task.
    #[test]
    fn a_session_stays_inside_its_own_project() {
        use crate::config::ProjectCfg;
        let matcher = Matcher::new(&[
            ProjectCfg {
                name: "chronicle".into(),
                repos: vec!["/no/such/chronicle".into()],
                ..ProjectCfg::default()
            },
            ProjectCfg {
                name: "contoso".into(),
                repos: vec!["/no/such/contoso".into()],
                ..ProjectCfg::default()
            },
        ]);
        let mut a = HashMap::new();
        a.insert(Key::Anchor(AnchorKind::Item, "ACME-1".into()), 60.0);
        a.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 60.0);
        a.insert(
            Key::Anchor(AnchorKind::Branch, "chronicle@main".into()),
            60.0,
        );
        let mut b = HashMap::new();
        b.insert(Key::Anchor(AnchorKind::Item, "ACME-2".into()), 60.0);
        b.insert(Key::Anchor(AnchorKind::Place, "chronicle".into()), 60.0);
        let profiles = vec![
            Profile {
                task_id: 85,
                minutes: a,
                last_ts: Some(0),
                vec: None,
            },
            Profile {
                task_id: 86,
                minutes: b,
                last_ts: Some(0),
                vec: None,
            },
        ];
        let projects = HashMap::from([
            (85, Some("chronicle".to_owned())),
            (86, Some("chronicle".to_owned())),
        ]);
        let mut spans = vec![
            agent_span(1, 0, 30, "s1", "chronicle"),
            agent_span(2, 30, 40, "s2", "contoso"),
        ];
        spans[0].anchors.push(Anchor {
            kind: AnchorKind::Branch,
            value: "chronicle@main".into(),
        });
        file_by_place(&mut spans);
        let s1 = live("s1", &[5, 15, 25], &[1, 10], &[]);
        let nyc = live(
            "s2",
            &[32, 38],
            &[31, 35],
            &[
                (AnchorKind::Session, "s2"),
                (AnchorKind::Place, "contoso"),
                (AnchorKind::Item, "ACME-1"),
            ],
        );
        // Never on screen: its project comes from its own scope.
        let s3 = live(
            "s3",
            &[12, 22],
            &[8, 20],
            &[
                (AnchorKind::Session, "s3"),
                (AnchorKind::Place, "chronicle"),
                (AnchorKind::Item, "ACME-2"),
            ],
        );
        let split = |sessions: &[LiveSession], sinks: &Sinks| {
            split_concurrent(
                vec![whole(0, 40, 85, "agent")],
                &spans,
                sessions,
                &profiles,
                &projects,
                sinks,
                &matcher,
                &Params::default(),
                &SegParams::default(),
                &[],
            )
        };
        let out = split(&[s1.clone(), nyc.clone()], &Sinks::default());
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(85));
        assert_eq!(out[0].share, 1.0);
        let out = split(&[s1.clone(), nyc.clone(), s3.clone()], &Sinks::default());
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].target, Target::Existing(85));
        assert_eq!(out[0].share, 0.5);
        assert_eq!(out[1].target, Target::Existing(86));
        assert_eq!(out[1].share, 0.5);
        assert_eq!(out[1].kind, "supervise");
        // The current declared task takes the session with no ticket; the
        // one on a ticket it does not hold stays with the holder.
        let sinks = Sinks {
            current: HashMap::from([("chronicle".to_owned(), 9)]),
            ..Sinks::default()
        };
        let out = split(&[s1, nyc, s3], &sinks);
        let mut ids: Vec<i64> = out
            .iter()
            .filter_map(|p| match p.target {
                Target::Existing(id) => Some(id),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, [9, 86], "{out:?}");
    }
}
