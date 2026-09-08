//! Span anchors (m30 chunk 1): typed, tool-neutral evidence pulled from a
//! focus span's app, title and URL, plus the collector events that overlap
//! it. The anchor kinds are the contract; the app families and site rules
//! below are adapters, one table row each, and an app nobody wrote a row
//! for falls back to the generic grammar (strip the app suffix, keep the
//! rest as a document, keep the domain).

use std::sync::LazyLock;

use regex::Regex;

use crate::evidence::strip_glyphs;
use crate::types::{ActivityEvent, ActivityKind};

/// What an anchor names. Strength is fixed per kind: an item, change,
/// branch, tool session or calendar event identifies the work; a document
/// or place narrows it; people and domains only hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnchorKind {
    /// Work item in any tracker: `ACME-11382`, `owner/repo#123`, `asana:…`.
    Item,
    /// Pull / merge request: `owner/repo#123`.
    Change,
    /// VCS branch.
    Branch,
    /// One run of an AI / agent tool (collector `ext_id`).
    Session,
    /// Calendar entry or meeting room (collector `ext_id`, `meet:…`).
    Event,
    /// A named document: file, page, design file, mail thread, note.
    Doc,
    /// Where the work lives: repo or project folder, `owner/repo`, vault.
    Place,
    /// Who it is with: chat channel or DM, attendee.
    People,
    /// Site.
    Domain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    Weak,
    Medium,
    Strong,
}

impl AnchorKind {
    pub const ALL: [AnchorKind; 9] = [
        AnchorKind::Item,
        AnchorKind::Change,
        AnchorKind::Branch,
        AnchorKind::Session,
        AnchorKind::Event,
        AnchorKind::Doc,
        AnchorKind::Place,
        AnchorKind::People,
        AnchorKind::Domain,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AnchorKind::Item => "item",
            AnchorKind::Change => "change",
            AnchorKind::Branch => "branch",
            AnchorKind::Session => "session",
            AnchorKind::Event => "event",
            AnchorKind::Doc => "doc",
            AnchorKind::Place => "place",
            AnchorKind::People => "people",
            AnchorKind::Domain => "domain",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn strength(self) -> Strength {
        match self {
            AnchorKind::Item
            | AnchorKind::Change
            | AnchorKind::Branch
            | AnchorKind::Session
            | AnchorKind::Event => Strength::Strong,
            AnchorKind::Doc | AnchorKind::Place => Strength::Medium,
            AnchorKind::People | AnchorKind::Domain => Strength::Weak,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Anchor {
    pub kind: AnchorKind,
    pub value: String,
}

impl Anchor {
    fn new(kind: AnchorKind, value: impl Into<String>) -> Self {
        Anchor {
            kind,
            value: value.into(),
        }
    }
}

/// App family by window class. Decides which title grammar applies and
/// which collector events may attach; never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Terminal,
    Editor,
    /// Git GUIs: the title names the repo.
    Vcs,
    Browser,
    Chat,
    Mail,
    Meeting,
    /// Design, office, PDF, notes: the title names a document.
    Document,
    /// Chronicle's own window and anything without a grammar.
    Other,
}

/// `(family, window-class fragments)`; matched case-insensitively as
/// substrings so `Google-chrome`, `chromium-browser` and `org.gnome.Console`
/// all land. First match wins, so the specific rows come first.
const FAMILIES: &[(Family, &[&str])] = &[
    (
        Family::Vcs,
        &[
            "sublime_merge",
            "gitkraken",
            "github desktop",
            "sourcetree",
            "fork",
            "gittyup",
            "lazygit",
        ],
    ),
    (
        Family::Terminal,
        &[
            "terminator",
            "gnome-terminal",
            "konsole",
            "alacritty",
            "kitty",
            "xterm",
            "wezterm",
            "foot",
            "iterm",
            "terminal",
            "tilix",
            "urxvt",
            "rxvt",
            "st-256color",
            "tmux",
            "console",
            "hyper",
            "ghostty",
            "warp",
            "cmd",
            "powershell",
            "windowsterminal",
            "mintty",
        ],
    ),
    (
        Family::Editor,
        &[
            "code",
            "vscodium",
            "cursor",
            "windsurf",
            "sublime_text",
            "subl",
            "jetbrains",
            "idea",
            "pycharm",
            "webstorm",
            "clion",
            "goland",
            "rubymine",
            "phpstorm",
            "rider",
            "datagrip",
            "android-studio",
            "gvim",
            "nvim",
            "neovide",
            "vim",
            "emacs",
            "zed",
            "kate",
            "gedit",
            "notepad++",
            "helix",
            "lapce",
            "xcode",
            "textmate",
            "nova",
            "bbedit",
        ],
    ),
    (
        Family::Browser,
        &[
            "google-chrome",
            "chrome",
            "chromium",
            "firefox",
            "librewolf",
            "brave",
            "msedge",
            "edge",
            "safari",
            "arc",
            "vivaldi",
            "opera",
            "zen",
            "epiphany",
            "falkon",
            "qutebrowser",
        ],
    ),
    (
        Family::Chat,
        &[
            "slack",
            "discord",
            "element",
            "telegram",
            "zulip",
            "mattermost",
            "rocket.chat",
            "signal",
            "whatsapp",
            "messages",
            "beeper",
            "ferdium",
            "rambox",
            "teams",
        ],
    ),
    (
        Family::Mail,
        &[
            "thunderbird",
            "evolution",
            "geary",
            "outlook",
            "mailspring",
            "kmail",
            "mail",
            "spark",
        ],
    ),
    (
        Family::Meeting,
        &[
            "zoom", "webex", "skype", "jitsi", "meet", "facetime", "around", "whereby",
        ],
    ),
    (
        Family::Document,
        &[
            "figma",
            "sketch",
            "inkscape",
            "gimp",
            "krita",
            "blender",
            "affinity",
            "penpot",
            "libreoffice",
            "soffice",
            "winword",
            "excel",
            "powerpnt",
            "onlyoffice",
            "wps",
            "okular",
            "evince",
            "zathura",
            "acroread",
            "acrobat",
            "foxit",
            "preview",
            "xreader",
            "obsidian",
            "logseq",
            "joplin",
            "notion",
            "bear",
            "typora",
            "zettlr",
            "anytype",
        ],
    ),
];

pub fn family(app: &str) -> Family {
    let app = app.to_ascii_lowercase();
    // Exact class names first so `code` does not catch `xcode`-like classes
    // in the wrong row, then substrings.
    for (fam, names) in FAMILIES {
        if names.iter().any(|n| app == *n) {
            return *fam;
        }
    }
    for (fam, names) in FAMILIES {
        if names.iter().any(|n| app.contains(n)) {
            return *fam;
        }
    }
    Family::Other
}

/// Anchors from a span's own app, title and URL. `ticket_re` is the
/// configured work-item key pattern.
pub fn extract(app: &str, title: &str, url: Option<&str>, ticket_re: &Regex) -> Vec<Anchor> {
    let fam = family(app);
    let title_raw = title;
    let title = clean_title(title);
    let mut out = Vec::new();
    match fam {
        Family::Terminal => {
            if let Some(place) = path_place(&title) {
                out.push(Anchor::new(AnchorKind::Place, place));
            } else if is_tool_title(title_raw) {
                // An agent tool names the conversation in the terminal title
                // (`✳ m27 chunks 2-7`); that name is the document, the bare
                // product name is not.
                if let Some(doc) = doc_value(&title)
                    .filter(|d| !TOOL_NAMES.contains(&d.to_ascii_lowercase().as_str()))
                {
                    out.push(Anchor::new(AnchorKind::Doc, doc));
                }
            }
        }
        Family::Editor => editor_title(&title, app, &mut out),
        Family::Vcs => {
            let body = strip_app_suffix(&title, app);
            if let Some(place) = path_place(body)
                .or_else(|| project_name(body))
                .or_else(|| parts(body).first().and_then(|p| place_value(p)))
            {
                out.push(Anchor::new(AnchorKind::Place, place));
            }
        }
        Family::Browser => browser(&title, url, &mut out),
        Family::Chat => chat_title(&title, app, &mut out),
        Family::Mail => {
            if let Some(doc) = mail_subject(strip_app_suffix(&title, app)) {
                out.push(Anchor::new(AnchorKind::Doc, doc));
            }
        }
        Family::Meeting => {}
        Family::Document => document_title(&title, app, &mut out),
        Family::Other => {
            if let Some(doc) = doc_value(strip_app_suffix(&title, app))
                && !doc.eq_ignore_ascii_case(app)
            {
                out.push(Anchor::new(AnchorKind::Doc, doc));
            }
        }
    }
    // A work-item key anywhere in the title or URL names the item whatever
    // the family; the first one, like `evidence::keys_in_spans`.
    if let Some(key) = ticket_re
        .find(&title)
        .or_else(|| url.and_then(|u| ticket_re.find(u)))
    {
        out.push(Anchor::new(AnchorKind::Item, key.as_str()));
    }
    dedup(out)
}

/// Anchors from collector events that overlap the span. `events` must
/// cover the span's window plus enough history for the latest checkout per
/// repo. Only families that can host the event attach it: an AI session or
/// edit to a terminal/editor span, a shell fold to a terminal, a meeting to
/// anything.
pub fn from_activity(
    app: &str,
    title: &str,
    start_ms: i64,
    end_ms: i64,
    own: &[Anchor],
    events: &[ActivityEvent],
    ticket_re: &Regex,
) -> Vec<Anchor> {
    let fam = family(app);
    let mut out = Vec::new();
    let overlaps = |e: &ActivityEvent| {
        let lo = e.ts.as_millisecond();
        let hi = e.end_ts.map(|t| t.as_millisecond()).unwrap_or(lo);
        lo < end_ms && hi >= start_ms
    };
    let hosts_tool = matches!(fam, Family::Terminal | Family::Editor);
    // A span that names its own place only takes tool events from that
    // place: a session running all afternoon in one repo must not claim a
    // terminal sitting in another. A bare `Claude Code` title names none
    // and takes every overlapping session.
    let own_places: Vec<&str> = own
        .iter()
        .filter(|a| a.kind == AnchorKind::Place)
        .map(|a| a.value.as_str())
        .collect();
    let same_place = |e: &ActivityEvent| {
        own_places.is_empty()
            || e.repo.is_empty()
            || own_places.iter().any(|p| p.eq_ignore_ascii_case(&e.repo))
    };
    // Concurrent sessions in one place (m32 chunk 2): the span belongs to
    // the session that owned its title (the tool writes the conversation's
    // name into the terminal title, and the title stays up after the
    // transcript's last write, so a titled session need not overlap); else
    // to the overlapping one a prompt was typed into most recently at or
    // before the span's end; else to the one whose transcript was written
    // to most recently before or during it. Rows captured before the
    // collector recorded any of those carry nothing, and for those every
    // overlapping session still attaches.
    let session_ok = |e: &ActivityEvent| {
        e.kind == ActivityKind::AiSession && overlaps(e) && hosts_tool && same_place(e)
    };
    let want = clean_title(title);
    let titled = |e: &ActivityEvent| {
        !want.is_empty()
            && detail_strings(e.detail.as_deref(), "titles")
                .iter()
                .any(|t| clean_title(t).eq_ignore_ascii_case(&want))
    };
    let title_ok = |e: &ActivityEvent| {
        e.kind == ActivityKind::AiSession
            && hosts_tool
            && same_place(e)
            && e.ts.as_millisecond() < end_ms
            && titled(e)
    };
    let chosen_session = if events.iter().any(title_ok) {
        nearest_prompter(events, title_ok, end_ms)
            .or_else(|| nearest_writer(events, title_ok, end_ms))
            .or_else(|| events.iter().position(title_ok))
    } else {
        nearest_prompter(events, session_ok, end_ms)
            .or_else(|| nearest_writer(events, session_ok, end_ms))
    };
    // A bare terminal's place is where its shell is, not every directory
    // it visited while the span was open: a cwd row still being refreshed
    // at the span's end, the latest arrival among those. With none alive
    // the span stays unattached rather than borrowing the place a shell
    // left earlier (m32 chunk 2); a one-shot shell fold still counts.
    let shell_ok = |e: &ActivityEvent| {
        matches!(e.kind, ActivityKind::Shell | ActivityKind::Cwd)
            && overlaps(e)
            && fam == Family::Terminal
            && same_place(e)
    };
    let alive = |e: &ActivityEvent| {
        e.kind == ActivityKind::Shell || e.end_ts.unwrap_or(e.ts).as_millisecond() >= end_ms
    };
    let chosen_shell = if own_places.is_empty() {
        events
            .iter()
            .enumerate()
            .filter(|(_, e)| shell_ok(e) && alive(e))
            .max_by_key(|(_, e)| e.ts)
            .map(|(i, _)| i)
    } else {
        None
    };
    for (i, e) in events
        .iter()
        .enumerate()
        .filter(|(i, e)| overlaps(e) || chosen_session == Some(*i))
    {
        match e.kind {
            ActivityKind::AiSession if hosts_tool && same_place(e) => {
                if chosen_session.is_some_and(|c| c != i) {
                    continue;
                }
                if let Some(id) = &e.ext_id {
                    out.push(Anchor::new(AnchorKind::Session, id.clone()));
                }
                push_scope(&mut out, &e.repo, &e.branch, ticket_re);
                for p in session_docs(e.detail.as_deref(), start_ms, end_ms) {
                    if let Some(doc) = doc_value(basename(&p)) {
                        out.push(Anchor::new(AnchorKind::Doc, doc));
                    }
                }
            }
            ActivityKind::Edit if fam == Family::Editor && same_place(e) => {
                push_scope(&mut out, &e.repo, &e.branch, ticket_re);
                if let Some(doc) = e.summary.as_deref().and_then(doc_value) {
                    out.push(Anchor::new(AnchorKind::Doc, doc));
                }
            }
            ActivityKind::Shell | ActivityKind::Cwd if fam == Family::Terminal && same_place(e) => {
                if own_places.is_empty() && chosen_shell != Some(i) {
                    continue;
                }
                push_scope(&mut out, &e.repo, "", ticket_re);
            }
            // A call is a meeting too (m32 chunk 1): its `call:<start>` id
            // makes the span `meet` for the segmenter.
            ActivityKind::Meeting | ActivityKind::Call => {
                if let Some(id) = &e.ext_id {
                    out.push(Anchor::new(AnchorKind::Event, id.clone()));
                }
                for who in detail_strings(e.detail.as_deref(), "attendees") {
                    if let Some(p) = people_value(&who) {
                        out.push(Anchor::new(AnchorKind::People, p));
                    }
                }
            }
            _ => {}
        }
    }
    // The branch checked out in a place the span names, as of the span's
    // end: the latest checkout/commit marker per repo.
    let places: Vec<&str> = own
        .iter()
        .chain(out.iter())
        .filter(|a| a.kind == AnchorKind::Place)
        .map(|a| a.value.as_str())
        .collect();
    if !places.is_empty() && !out.iter().any(|a| a.kind == AnchorKind::Branch) {
        let latest = events
            .iter()
            .filter(|e| e.kind.is_vcs() && e.ts.as_millisecond() < end_ms)
            .filter(|e| places.iter().any(|p| p.eq_ignore_ascii_case(&e.repo)))
            .max_by_key(|e| e.ts);
        if let Some(e) = latest {
            push_branch(&mut out, &e.repo, &e.branch, ticket_re);
        }
    }
    dedup(out)
}

/// Same-span anchors from the store's own lens: the span's anchors plus the
/// collector ones, deduplicated.
pub fn merge(own: Vec<Anchor>, more: Vec<Anchor>) -> Vec<Anchor> {
    let mut all = own;
    all.extend(more);
    dedup(all)
}

const MAX_PATH_DOCS: usize = 8;
/// How far before a span's start a session's file touches still name its
/// documents: the tool edits, then the person looks at the terminal.
const DOC_GRACE_MS: i64 = 15 * 60_000;

/// The files a session touched around `[start_ms, end_ms]`, most recent
/// first, at most `MAX_PATH_DOCS` (m32 chunk 2): touches at or before the
/// span's end and within `DOC_GRACE_MS` of its start, or, with none that
/// close, the files of the latest touch minute before it. A session that
/// worked all day names the files it was on, not the first eight it
/// opened. Rows without `touches` (captured before the collector kept
/// them) fall back to the first paths.
fn session_docs(detail: Option<&str>, start_ms: i64, end_ms: i64) -> Vec<String> {
    let paths = detail_strings(detail, "paths");
    let Some(detail) = detail else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return Vec::new();
    };
    let Some(touches) = v.get("touches").and_then(|t| t.as_array()) else {
        return paths.into_iter().take(MAX_PATH_DOCS).collect();
    };
    let mut touches: Vec<(i64, usize)> = touches
        .iter()
        .filter_map(|t| {
            let t = t.as_array()?;
            Some((t.first()?.as_i64()?, t.get(1)?.as_u64()? as usize))
        })
        .filter(|&(m, _)| m <= end_ms)
        .collect();
    touches.sort_by_key(|&(m, i)| (std::cmp::Reverse(m), i));
    let Some(&(latest, _)) = touches.first() else {
        return Vec::new();
    };
    let lo = (start_ms - DOC_GRACE_MS).min(latest);
    let mut out: Vec<String> = Vec::new();
    for (m, i) in touches {
        if m < lo || out.len() >= MAX_PATH_DOCS {
            break;
        }
        if let Some(p) = paths.get(i)
            && !out.contains(p)
        {
            out.push(p.clone());
        }
    }
    out
}

/// Product names agent tools show when the conversation has no title yet.
const TOOL_NAMES: &[&str] = &[
    "claude code",
    "claude",
    "codex",
    "aider",
    "gemini",
    "opencode",
];

/// A terminal title an agent tool wrote: the status glyph prefix.
fn is_tool_title(raw: &str) -> bool {
    strip_glyphs(raw).len() < raw.trim_start().len()
}
const MAX_DOC_CHARS: usize = 120;

fn push_scope(out: &mut Vec<Anchor>, repo: &str, branch: &str, ticket_re: &Regex) {
    if let Some(place) = place_value(repo) {
        out.push(Anchor::new(AnchorKind::Place, place));
    }
    push_branch(out, repo, branch, ticket_re);
}

/// The branch anchor is repo-qualified (`chronicle@main`, m32 chunk 2):
/// `main` in one repo is not `main` in another, and the profiler must not
/// learn them as one value. The work-item key still comes from the bare
/// branch name.
fn push_branch(out: &mut Vec<Anchor>, repo: &str, branch: &str, ticket_re: &Regex) {
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return;
    }
    let value = match place_value(repo) {
        Some(place) => format!("{place}@{branch}"),
        None => branch.to_owned(),
    };
    out.push(Anchor::new(AnchorKind::Branch, value));
    if let Some(key) = ticket_re.find(branch) {
        out.push(Anchor::new(AnchorKind::Item, key.as_str()));
    }
}

/// Among the events `ok` admits, the index of the session a prompt was
/// typed into most recently at or before `end_ms` (ties go to the
/// later-ending session). `None` when no admitted event has one — rows
/// captured before the collector kept prompt minutes, or sessions the
/// person has not typed into yet — and the caller falls back to writes.
fn nearest_prompter<F: Fn(&ActivityEvent) -> bool>(
    events: &[ActivityEvent],
    ok: F,
    end_ms: i64,
) -> Option<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| ok(e))
        .filter_map(|(i, e)| {
            let last = detail_i64s(e.detail.as_deref(), "prompt_minutes")
                .into_iter()
                .filter(|&p| p <= end_ms)
                .max()?;
            Some((i, (last, e.end_ts.unwrap_or(e.ts).as_millisecond())))
        })
        .max_by_key(|&(_, key)| key)
        .map(|(i, _)| i)
}

/// Among the events `ok` admits, the index of the session whose latest
/// write at or before `end_ms` is the most recent (a write inside the span
/// beats one before it; ties go to the later-ending session). `None` when
/// no admitted event carries write times — the caller then keeps them all.
/// A session whose kept writes all fall after the span loses to any with
/// one before its end, and among only such sessions the earliest wins.
fn nearest_writer<F: Fn(&ActivityEvent) -> bool>(
    events: &[ActivityEvent],
    ok: F,
    end_ms: i64,
) -> Option<usize> {
    let mut best: Option<(usize, (bool, i64, i64))> = None;
    let mut any_writes = false;
    for (i, e) in events.iter().enumerate().filter(|(_, e)| ok(e)) {
        let writes = detail_i64s(e.detail.as_deref(), "writes");
        if writes.is_empty() {
            continue;
        }
        any_writes = true;
        let before = writes.iter().copied().filter(|&w| w <= end_ms).max();
        let key = match before {
            Some(w) => (true, w, e.end_ts.unwrap_or(e.ts).as_millisecond()),
            None => (
                false,
                -writes.iter().copied().min().unwrap_or(i64::MAX),
                e.end_ts.unwrap_or(e.ts).as_millisecond(),
            ),
        };
        if best.is_none_or(|(_, k)| key > k) {
            best = Some((i, key));
        }
    }
    if any_writes {
        best.map(|(i, _)| i)
    } else {
        None
    }
}

/// Transcript write times (ms) of an AI session row; empty for rows captured
/// before the collector recorded them.
pub fn session_writes(e: &ActivityEvent) -> Vec<i64> {
    detail_i64s(e.detail.as_deref(), "writes")
}

fn detail_i64s(detail: Option<&str>, field: &str) -> Vec<i64> {
    let Some(detail) = detail else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return Vec::new();
    };
    v.get(field)
        .and_then(|a| a.as_array())
        .map(|a| a.iter().filter_map(|n| n.as_i64()).collect())
        .unwrap_or_default()
}

fn detail_strings(detail: Option<&str>, field: &str) -> Vec<String> {
    let Some(detail) = detail else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) else {
        return Vec::new();
    };
    v.get(field)
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn dedup(mut v: Vec<Anchor>) -> Vec<Anchor> {
    let mut seen = std::collections::HashSet::new();
    v.retain(|a| seen.insert((a.kind, a.value.clone())));
    v
}

// ---------------------------------------------------------------- titles

/// Glyphs, dirty markers and whitespace runs gone.
fn clean_title(title: &str) -> String {
    let t = strip_glyphs(title);
    let t = t.trim_start_matches(|c: char| {
        matches!(c, '●' | '•' | '*' | '+' | '○') || c.is_whitespace()
    });
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Title separators apps put between the document and their own name.
const SEPS: [&str; 4] = [" - ", " — ", " – ", " | "];

/// Split a title on its separators, right to left: `["Doc", "Proj", "App"]`.
fn parts(title: &str) -> Vec<&str> {
    let mut out = vec![title];
    for sep in SEPS {
        out = out.into_iter().flat_map(|p| p.split(sep)).collect();
    }
    out.into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// Drop the trailing segment when it names the app (`… - Sublime Text`,
/// `… — Mozilla Firefox`, `… - GNU Emacs at box`): case-insensitive token
/// overlap between the segment and the window class.
fn strip_app_suffix<'a>(title: &'a str, app: &str) -> &'a str {
    let Some((head, tail)) = rsplit_sep(title) else {
        return title;
    };
    if names_app(tail, app) {
        head.trim()
    } else {
        title
    }
}

fn rsplit_sep(title: &str) -> Option<(&str, &str)> {
    let mut best: Option<(usize, &str)> = None;
    for sep in SEPS {
        if let Some(i) = title.rfind(sep)
            && best.is_none_or(|(b, _)| i > b)
        {
            best = Some((i, sep));
        }
    }
    best.map(|(i, sep)| (&title[..i], title[i + sep.len()..].trim()))
}

/// Product names apps put in titles that their window class does not spell.
const APP_WORDS: &[&str] = &[
    "visual studio code",
    "sublime text",
    "sublime merge",
    "mozilla firefox",
    "google chrome",
    "chromium",
    "microsoft edge",
    "brave",
    "vivaldi",
    "opera",
    "gnu emacs",
    "vim",
    "nvim",
    "neovim",
    "kate",
    "zed",
    "cursor",
    "windsurf",
    "slack",
    "discord",
    "microsoft teams",
    "mozilla thunderbird",
    "thunderbird",
    "outlook",
    "obsidian",
    "notion",
    "figma",
    "okular",
    "evince",
    "document viewer",
    "libreoffice",
    "onlyoffice",
    "gimp",
    "inkscape",
    "blender",
    "jetbrains",
    "intellij idea",
    "pycharm",
    "webstorm",
    "clion",
    "goland",
    "rider",
    "android studio",
    "xcode",
    "typora",
    "logseq",
    "joplin",
    "gitkraken",
    "github desktop",
    "youtube",
    "gmail",
    "google docs",
    "google sheets",
    "google slides",
    "google drive",
    "confluence",
    "jira",
    "linear",
    "asana",
    "trello",
    "zoom",
    "notepad++",
    "unregistered",
];

fn names_app(segment: &str, app: &str) -> bool {
    let seg = segment.to_ascii_lowercase();
    let seg = seg.trim_end_matches(" (unregistered)").trim();
    let app = app.to_ascii_lowercase();
    let app_tokens: Vec<&str> = app
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .collect();
    let seg_tokens: Vec<&str> = seg
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if seg_tokens.len() <= 4 && seg_tokens.iter().any(|t| app_tokens.contains(t)) {
        return true;
    }
    APP_WORDS
        .iter()
        .any(|w| seg == *w || seg.starts_with(&format!("{w} ")))
}

/// A path the title shows, as `~/x/y`, `/home/u/x`, `/Users/u/x`, `C:\…`.
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:~|/home/[^/\s]+|/Users/[^/\s]+|/root|[A-Za-z]:\\Users\\[^\\\s]+)(?:[/\\][^:"'()\[\]]*)?"#)
        .unwrap()
});

/// Folder names that hold projects rather than being one.
const ROOTS: &[&str] = &[
    "dev",
    "src",
    "code",
    "projects",
    "project",
    "work",
    "repos",
    "repo",
    "git",
    "github",
    "gitlab",
    "workspace",
    "workspaces",
    "developer",
    "documents",
    "source",
    "sources",
    "www",
    "sites",
    "clients",
    "apps",
    "go",
    "rust",
    "python",
    "js",
];

/// The project folder a home-relative path names: the component after a
/// known root (`~/dev/<x>`, `C:\Users\u\source\repos\<x>`), else the first
/// component under home. `~` alone names nothing.
pub fn path_place(text: &str) -> Option<String> {
    let p = path_in(text)?;
    let rest = p
        .trim_start_matches('~')
        .trim_start_matches(|c: char| c != '/' && c != '\\');
    let mut comps: Vec<&str> = rest.split(['/', '\\']).filter(|c| !c.is_empty()).collect();
    // Drop the home prefix: `home/<u>`, `Users/<u>`, `root`.
    let home_len = if p.starts_with('~') {
        0
    } else if p.starts_with("/root") {
        1
    } else {
        2
    };
    comps.drain(..home_len.min(comps.len()));
    let mut i = 0;
    while i < comps.len() && ROOTS.contains(&comps[i].to_ascii_lowercase().as_str()) {
        i += 1;
    }
    comps.get(i).and_then(|c| place_value(c))
}

/// The path a title shows. Spaces are allowed inside it (`~/Documents/Acme
/// Ltd`), so the match is cut at the first title separator and trailing
/// whitespace.
fn path_in(text: &str) -> Option<&str> {
    let m = PATH.find(text)?;
    let mut p = m.as_str();
    for sep in SEPS {
        if let Some(i) = p.find(sep) {
            p = &p[..i];
        }
    }
    Some(p.trim_end())
}

/// The place a filesystem path names: the project folder under a known
/// root for home-relative paths, else the last component. Home itself and
/// the filesystem root name nothing.
pub fn place_from_path(path: &str) -> Option<String> {
    if PATH.is_match(path) {
        path_place(path)
    } else {
        place_value(path)
    }
}

fn place_value(s: &str) -> Option<String> {
    let s = s.trim().trim_end_matches(['/', '\\']);
    let s = basename(s);
    if s.is_empty() || s == "~" || s.starts_with('.') && s.len() <= 2 {
        return None;
    }
    Some(s.to_ascii_lowercase())
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(p)
}

/// A bare `(project)` or `[project]` segment, as Sublime and JetBrains
/// print next to the file.
fn project_name(text: &str) -> Option<String> {
    static PAREN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(([^()]{1,60})\)").unwrap());
    PAREN
        .captures(text)
        .and_then(|c| path_place(&c[1]).or_else(|| place_value(&c[1])))
}

fn looks_like_file(s: &str) -> bool {
    static FILE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[^\s/\\]+\.[A-Za-z0-9]{1,8}$").unwrap());
    let s = basename(s.trim());
    FILE.is_match(s) || s == "Makefile" || s == "Dockerfile"
}

const DOC_STOP: &[&str] = &[
    "new tab",
    "untitled",
    "home",
    "inbox",
    "loading",
    "loading…",
    "blank",
    "start page",
    "welcome",
    "settings",
    "preferences",
    "search",
    "dashboard",
    "index",
];

/// A document value: cleaned, capped, and not a placeholder.
fn doc_value(s: &str) -> Option<String> {
    let s = clean_title(s);
    let s = s.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
    if s.is_empty() || DOC_STOP.contains(&s.to_ascii_lowercase().as_str()) {
        return None;
    }
    let mut it = s.chars();
    let head: String = it.by_ref().take(MAX_DOC_CHARS).collect();
    Some(if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    })
}

fn people_value(s: &str) -> Option<String> {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim_matches(|c: char| c == '"' || c == '\'');
    (!s.is_empty() && s.len() <= 80).then(|| s.to_owned())
}

/// Editors: the file and the project, from a path when the title shows one
/// (Sublime, Vim, JetBrains), else from the `file - project` /
/// `project – file` segments (VS Code, Cursor, Zed).
fn editor_title(title: &str, app: &str, out: &mut Vec<Anchor>) {
    let body = strip_app_suffix(title, app);
    let body = body.trim_end_matches(|c: char| c == '+' || c == '*' || c.is_whitespace());
    if let Some(path) = path_in(body) {
        if let Some(place) = path_place(path) {
            out.push(Anchor::new(AnchorKind::Place, place));
        }
        let file = basename(path);
        if looks_like_file(file) {
            if let Some(doc) = doc_value(file) {
                out.push(Anchor::new(AnchorKind::Doc, doc));
            }
        } else if let Some(doc) = body.split_whitespace().find(|p| looks_like_file(p)) {
            // `models.py + (~/dev/x) - VIM`: the file sits before the path.
            out.push(Anchor::new(AnchorKind::Doc, basename(doc).to_owned()));
        }
        return;
    }
    if let Some(place) = project_name(body) {
        out.push(Anchor::new(AnchorKind::Place, place));
    }
    // JetBrains appends the module as `[shop.api]`; it is not the file.
    let ps: Vec<&str> = parts(body)
        .into_iter()
        .map(|p| p.split(" [").next().unwrap_or(p).trim())
        .filter(|p| !p.is_empty())
        .collect();
    let file = ps.iter().find(|p| looks_like_file(p));
    if let Some(f) = file {
        if let Some(doc) = doc_value(basename(f)) {
            out.push(Anchor::new(AnchorKind::Doc, doc));
        }
        if !out.iter().any(|a| a.kind == AnchorKind::Place) {
            // `file - project`, `project – file`, `file - project [module]`.
            if let Some(other) = ps.iter().find(|p| *p != f)
                && let Some(place) = place_value(other).filter(|p| !looks_like_file(p))
            {
                out.push(Anchor::new(AnchorKind::Place, place));
            }
        }
    } else if let Some(doc) = ps.first().and_then(|p| doc_value(p)) {
        out.push(Anchor::new(AnchorKind::Doc, doc));
    }
}

/// Chat: `Name (DM) - Workspace - Slack`, `#chan - Server - Discord`,
/// `Chat | Name | Microsoft Teams`, `Name — Element`.
fn chat_title(title: &str, app: &str, out: &mut Vec<Anchor>) {
    let body = strip_app_suffix(title, app);
    let ps = parts(body);
    let Some(first) = ps.first() else {
        return;
    };
    let first = first
        .trim_end_matches(" (DM)")
        .trim_end_matches(" (Channel)")
        .trim_end_matches(" (Private)")
        .trim_end_matches(" (Group)")
        .trim();
    const GENERIC: &[&str] = &[
        "chat", "activity", "calendar", "calls", "call", "meeting", "files", "teams", "home",
        "threads", "dms", "mentions", "unreads", "later", "search", "friends", "discord", "slack",
    ];
    let lower = first.to_ascii_lowercase();
    let unread = lower.trim_start_matches(|c: char| {
        c == '(' || c == '*' || c.is_ascii_digit() || c == ')' || c == ' '
    });
    if unread.is_empty() || GENERIC.contains(&unread) {
        // `Chat | Name | Microsoft Teams` puts the person second; Slack's
        // `Activity - Workspace - Slack` puts the workspace there.
        if let Some(second) = ps.get(1).filter(|_| body.contains(" | "))
            && !GENERIC.contains(&second.to_ascii_lowercase().as_str())
            && let Some(p) = people_value(second)
        {
            out.push(Anchor::new(AnchorKind::People, p));
        }
        return;
    }
    if let Some(p) = people_value(first.trim_start_matches(|c: char| {
        c == '(' || c == '*' || c.is_ascii_digit() || c == ')' || c == ' '
    })) {
        out.push(Anchor::new(AnchorKind::People, p));
    }
}

/// Mail: the subject unless the title is a folder.
fn mail_subject(body: &str) -> Option<String> {
    const FOLDERS: &[&str] = &[
        "inbox", "sent", "drafts", "archive", "trash", "spam", "junk", "all mail", "starred",
        "outbox",
    ];
    let first = parts(body).into_iter().next()?;
    let lower = first.to_ascii_lowercase();
    let lower =
        lower.trim_start_matches(|c: char| c == '(' || c.is_ascii_digit() || c == ')' || c == ' ');
    if FOLDERS.iter().any(|f| lower.starts_with(f)) || first.contains('@') {
        return None;
    }
    doc_value(first)
}

/// Design, office, PDF, notes: `file.ext - App`, `Note - Vault - Obsidian`.
fn document_title(title: &str, app: &str, out: &mut Vec<Anchor>) {
    let body = strip_app_suffix(title, app);
    let ps = parts(body);
    let Some(first) = ps.first() else {
        return;
    };
    if let Some(doc) = doc_value(basename(first)) {
        out.push(Anchor::new(AnchorKind::Doc, doc));
    }
    // Obsidian/Logseq print the vault after the note; office apps do not.
    let notes = app.to_ascii_lowercase();
    if ["obsidian", "logseq", "joplin", "notion"]
        .iter()
        .any(|n| notes.contains(n))
        && let Some(vault) = ps.get(1).and_then(|v| place_value(v))
    {
        out.push(Anchor::new(AnchorKind::Place, vault));
    }
}

// ------------------------------------------------------------------ web

/// Browser: the site rules decide by domain and path; the title, with the
/// site and browser names stripped, is the document for everything else.
fn browser(title: &str, url: Option<&str>, out: &mut Vec<Anchor>) {
    let body = strip_browser_suffix(title);
    let Some(url) = url.and_then(parse_url) else {
        if let Some(doc) = doc_value(body) {
            out.push(Anchor::new(AnchorKind::Doc, doc));
        }
        return;
    };
    out.push(Anchor::new(AnchorKind::Domain, url.domain.clone()));
    let host = url.host.as_str();
    let segs = &url.segs;
    let seg = |i: usize| segs.get(i).map(String::as_str).unwrap_or("");
    let site_doc = |out: &mut Vec<Anchor>| {
        if let Some(doc) = doc_value(strip_site_suffix(body, host)) {
            out.push(Anchor::new(AnchorKind::Doc, doc));
        }
    };
    if host_is(
        host,
        &["github.com", "gitlab.com", "bitbucket.org", "codeberg.org"],
    ) || host.contains("gitlab.")
    {
        if segs.len() >= 2 && !seg(0).is_empty() && !seg(1).is_empty() {
            let repo = format!("{}/{}", seg(0), seg(1)).to_ascii_lowercase();
            out.push(Anchor::new(AnchorKind::Place, seg(1).to_ascii_lowercase()));
            // GitLab nests groups and puts `/-/` before the kind.
            let rest: Vec<&str> = segs[2..]
                .iter()
                .map(String::as_str)
                .filter(|s| *s != "-")
                .collect();
            match (rest.first().copied(), rest.get(1).copied()) {
                (Some("pull" | "pulls" | "merge_requests" | "pull-requests"), Some(n))
                    if n.chars().all(|c| c.is_ascii_digit()) =>
                {
                    out.push(Anchor::new(AnchorKind::Change, format!("{repo}#{n}")));
                }
                (Some("issues" | "issue"), Some(n)) if n.chars().all(|c| c.is_ascii_digit()) => {
                    out.push(Anchor::new(AnchorKind::Item, format!("{repo}#{n}")));
                }
                _ => site_doc(out),
            }
        }
        return;
    }
    if host.ends_with(".atlassian.net") {
        if seg(0) == "browse" && !seg(1).is_empty() {
            out.push(Anchor::new(AnchorKind::Item, seg(1).to_ascii_uppercase()));
        } else {
            // Confluence pages and boards: the page title is the document.
            site_doc(out);
        }
        return;
    }
    if host == "linear.app" {
        if seg(1) == "issue" && !seg(2).is_empty() {
            out.push(Anchor::new(AnchorKind::Item, seg(2).to_ascii_uppercase()));
        } else {
            site_doc(out);
        }
        return;
    }
    if host == "app.asana.com" && seg(0) == "0" && !seg(2).is_empty() {
        out.push(Anchor::new(AnchorKind::Item, format!("asana:{}", seg(2))));
        return;
    }
    if host == "trello.com" && seg(0) == "c" && !seg(1).is_empty() {
        out.push(Anchor::new(AnchorKind::Item, format!("trello:{}", seg(1))));
        return;
    }
    if host == "app.clickup.com" && seg(0) == "t" && !seg(1).is_empty() {
        out.push(Anchor::new(AnchorKind::Item, format!("clickup:{}", seg(1))));
        return;
    }
    if host.ends_with(".monday.com") && seg(2) == "pulses" && !seg(3).is_empty() {
        out.push(Anchor::new(AnchorKind::Item, format!("monday:{}", seg(3))));
        return;
    }
    if host == "meet.google.com" && !seg(0).is_empty() && seg(0).contains('-') {
        out.push(Anchor::new(AnchorKind::Event, format!("meet:{}", seg(0))));
        return;
    }
    if host.ends_with("zoom.us") && seg(0) == "j" && !seg(1).is_empty() {
        out.push(Anchor::new(AnchorKind::Event, format!("zoom:{}", seg(1))));
        return;
    }
    if host_is(
        host,
        &[
            "app.slack.com",
            "teams.microsoft.com",
            "teams.live.com",
            "discord.com",
        ],
    ) || host.ends_with(".slack.com")
    {
        chat_title(body, "slack", out);
        return;
    }
    if host == "mail.google.com" || host.starts_with("outlook.") {
        if let Some(doc) = mail_subject(body) {
            out.push(Anchor::new(AnchorKind::Doc, doc));
        }
        return;
    }
    // Docs, design, wikis, everything else: the page title is the document.
    site_doc(out);
}

fn host_is(host: &str, hosts: &[&str]) -> bool {
    hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

const BROWSER_WORDS: &[&str] = &[
    "google chrome",
    "mozilla firefox",
    "chromium",
    "microsoft edge",
    "brave",
    "vivaldi",
    "opera",
    "safari",
    "arc",
    "firefox",
    "librewolf",
    "zen browser",
];

fn strip_browser_suffix(title: &str) -> &str {
    match rsplit_sep(title) {
        Some((head, tail)) => {
            let t = tail.to_ascii_lowercase();
            let t = t
                .trim_end_matches(" (private browsing)")
                .trim_end_matches(" (incognito)");
            if BROWSER_WORDS
                .iter()
                .any(|w| t == *w || t.starts_with(&format!("{w} ")))
            {
                head.trim()
            } else {
                title
            }
        }
        None => title,
    }
}

/// `Doc title - Google Docs`, `Page - Confluence`, `Repo · GitHub`: drop a
/// trailing segment that names the site.
fn strip_site_suffix<'a>(title: &'a str, host: &str) -> &'a str {
    let Some((head, tail)) = rsplit_sep(title) else {
        return title;
    };
    let tail_l = tail.to_ascii_lowercase();
    let labels: Vec<&str> = host
        .split('.')
        .filter(|l| l.len() >= 3 && *l != "www" && *l != "com")
        .collect();
    let tail_tokens: Vec<String> = tail_l
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect();
    let joined: String = tail_tokens.concat();
    let names_site = tail_tokens.len() <= 4
        && labels
            .iter()
            .any(|l| tail_tokens.iter().any(|t| t == l) || joined == *l);
    if names_site || APP_WORDS.iter().any(|w| tail_l == *w) {
        head.trim()
    } else {
        title
    }
}

struct Url {
    host: String,
    domain: String,
    segs: Vec<String>,
}

fn parse_url(u: &str) -> Option<Url> {
    let rest = u.split_once("://").map(|(_, r)| r).unwrap_or(u);
    let (hostport, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = hostport
        .rsplit('@')
        .next()
        .unwrap_or(hostport)
        .split(':')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    if host.is_empty() || !host.contains('.') {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let segs = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect();
    Some(Url {
        domain: registrable(&host),
        host,
        segs,
    })
}

/// `docs.google.com` → `google.com`, `x.atlassian.net` → `atlassian.net`,
/// `foo.co.uk` → `foo.co.uk`.
fn registrable(host: &str) -> String {
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    if n <= 2 {
        return host.to_owned();
    }
    const SECOND: &[&str] = &["co", "com", "org", "net", "gov", "ac", "edu"];
    if labels[n - 1].len() == 2 && SECOND.contains(&labels[n - 2]) {
        labels[n - 3..].join(".")
    } else {
        labels[n - 2..].join(".")
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;

    fn re() -> Regex {
        Regex::new(r"[A-Z][A-Z0-9]+-[0-9]+").unwrap()
    }

    fn has(v: &[Anchor], kind: AnchorKind, value: &str) -> bool {
        v.iter().any(|a| a.kind == kind && a.value == value)
    }

    fn kinds(v: &[Anchor], kind: AnchorKind) -> Vec<&str> {
        v.iter()
            .filter(|a| a.kind == kind)
            .map(|a| a.value.as_str())
            .collect()
    }

    #[test]
    fn families_by_class() {
        assert_eq!(family("Terminator"), Family::Terminal);
        assert_eq!(family("Google-chrome"), Family::Browser);
        assert_eq!(family("firefox-esr"), Family::Browser);
        assert_eq!(family("Sublime_merge"), Family::Vcs);
        assert_eq!(family("Slack"), Family::Chat);
        assert_eq!(family("Code"), Family::Editor);
        assert_eq!(family("jetbrains-idea"), Family::Editor);
        assert_eq!(family("org.gnome.Console"), Family::Terminal);
        assert_eq!(family("chronicle"), Family::Other);
        assert_eq!(family("obsidian"), Family::Document);
    }

    #[test]
    fn terminal_paths_name_the_project() {
        let a = extract(
            "Terminator",
            "sam@workstation:~/dev/chronicle",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        let a = extract(
            "Terminator",
            "/home/james/dev/mailer/mailer - fish",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Place), ["mailer"]);
        let a = extract("kitty", "~/Documents/Acme Ltd/notes", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Place), ["acme ltd"]);
        let a = extract("Terminator", "\u{2733} Claude Code", None, &re());
        assert!(a.is_empty());
        let a = extract(
            "Terminator",
            "\u{25d0} 11381 and 11374 merged and deployed",
            None,
            &re(),
        );
        assert_eq!(
            kinds(&a, AnchorKind::Doc),
            ["11381 and 11374 merged and deployed"]
        );
        let a = extract("Terminator", "git log", None, &re());
        assert!(a.is_empty());
        let a = extract("Terminator", "sam@box:~", None, &re());
        assert!(a.is_empty());
        let a = extract("cmd", r"C:\Users\jc\source\repos\Shop\api", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Place), ["shop"]);
    }

    #[test]
    fn editor_titles() {
        let a = extract(
            "Sublime_text",
            "~/dev/mailer/mailer/models.py (mailer) - Sublime Text",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Place), ["mailer"]);
        assert_eq!(kinds(&a, AnchorKind::Doc), ["models.py"]);
        let a = extract(
            "Code",
            "● theme.rs - chronicle - Visual Studio Code",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["theme.rs"]);
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        let a = extract("Cursor", "home.rs - chronicle - Cursor", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        let a = extract(
            "jetbrains-idea",
            "shop – src/main/Order.java [shop.api]",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Order.java"]);
        assert_eq!(kinds(&a, AnchorKind::Place), ["shop"]);
        let a = extract(
            "Gvim",
            "models.py + (~/dev/mailer/mailer) - VIM",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["models.py"]);
        assert_eq!(kinds(&a, AnchorKind::Place), ["mailer"]);
        let a = extract("Zed", "lib.rs — chronicle", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        let a = extract("Emacs", "init.el - GNU Emacs at box", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Doc), ["init.el"]);
        assert!(kinds(&a, AnchorKind::Place).is_empty());
    }

    #[test]
    fn vcs_gui_names_the_repo() {
        let a = extract(
            "Sublime_merge",
            "~/dev/chronicle - Sublime Merge (UNREGISTERED)",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        let a = extract("GitKraken", "chronicle - GitKraken", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
    }

    #[test]
    fn browser_code_hosts_and_trackers() {
        let a = extract(
            "Google-chrome",
            "Acai 25 by sam · Pull Request #39 · acme/mailer - Google Chrome",
            Some("https://github.com/acme/mailer/pull/39/files"),
            &re(),
        );
        assert!(has(&a, AnchorKind::Change, "acme/mailer#39"));
        assert!(has(&a, AnchorKind::Place, "mailer"));
        assert!(has(&a, AnchorKind::Domain, "github.com"));
        assert!(kinds(&a, AnchorKind::Doc).is_empty());
        let a = extract(
            "firefox",
            "Bug: flaky test · Issue #12 · acme/shop — Mozilla Firefox",
            Some("https://gitlab.com/acme/shop/-/issues/12"),
            &re(),
        );
        assert!(has(&a, AnchorKind::Item, "acme/shop#12"));
        let a = extract(
            "Google-chrome",
            "[ACME-11382] SMS: identification prefix rules - Jira",
            Some("https://acme.atlassian.net/browse/ACME-11382"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Item), ["ACME-11382"]);
        let a = extract(
            "Google-chrome",
            "ENG-42 Checkout flicker – Linear",
            Some("https://linear.app/acme/issue/ENG-42/checkout-flicker"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Item), ["ENG-42"]);
        let a = extract(
            "chrome",
            "Board - Asana",
            Some("https://app.asana.com/0/1200/1201"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Item), ["asana:1201"]);
        let a = extract(
            "chrome",
            "Onboarding | Trello",
            Some("https://trello.com/c/AbC123/4-onboarding"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Item), ["trello:AbC123"]);
    }

    #[test]
    fn browser_docs_chat_mail_meetings() {
        let a = extract(
            "Google-chrome",
            "Q4 pricing page draft - Google Docs - Google Chrome",
            Some("https://docs.google.com/document/d/1abc/edit"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Q4 pricing page draft"]);
        assert!(has(&a, AnchorKind::Domain, "google.com"));
        let a = extract(
            "Google-chrome",
            "Onboarding spec – Figma",
            Some("https://www.figma.com/design/XYZ/Onboarding-spec?node-id=1"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Onboarding spec"]);
        let a = extract(
            "Google-chrome",
            "Roadmap 2027 - Confluence",
            Some("https://acme.atlassian.net/wiki/spaces/PROD/pages/123/Roadmap+2027"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Roadmap 2027"]);
        let a = extract(
            "Google-chrome",
            "core-dev (Channel) - PB - Slack",
            Some("https://app.slack.com/client/T1/C2"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::People), ["core-dev"]);
        let a = extract(
            "Google-chrome",
            "Re: invoice 2041 - sam@acme.com - Gmail",
            Some("https://mail.google.com/mail/u/0/#inbox/abc"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Re: invoice 2041"]);
        let a = extract(
            "Google-chrome",
            "Inbox (3) - sam@acme.com - Gmail",
            Some("https://mail.google.com/mail/u/0/"),
            &re(),
        );
        assert!(kinds(&a, AnchorKind::Doc).is_empty());
        let a = extract(
            "Google-chrome",
            "Meet - abc-defg-hij",
            Some("https://meet.google.com/abc-defg-hij"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Event), ["meet:abc-defg-hij"]);
        let a = extract("firefox-esr", "New Tab — Mozilla Firefox", None, &re());
        assert!(a.is_empty());
        let a = extract(
            "firefox-esr",
            "mawww/kakoune: A better code editor — Mozilla Firefox",
            None,
            &re(),
        );
        assert_eq!(
            kinds(&a, AnchorKind::Doc),
            ["mawww/kakoune: A better code editor"]
        );
        let a = extract(
            "Google-chrome",
            "How to sessionize events - Stack Overflow",
            Some("https://stackoverflow.com/questions/1/how"),
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["How to sessionize events"]);
    }

    #[test]
    fn chat_mail_document_apps() {
        let a = extract("Slack", "Corbin Schmeil (DM) - PB - Slack", None, &re());
        assert_eq!(kinds(&a, AnchorKind::People), ["Corbin Schmeil"]);
        let a = extract("Slack", "* core-dev (Channel) - PB - Slack", None, &re());
        assert_eq!(kinds(&a, AnchorKind::People), ["core-dev"]);
        let a = extract("discord", "#general - Rustaceans - Discord", None, &re());
        assert_eq!(kinds(&a, AnchorKind::People), ["#general"]);
        let a = extract("teams", "Chat | Priya Nair | Microsoft Teams", None, &re());
        assert_eq!(kinds(&a, AnchorKind::People), ["Priya Nair"]);
        let a = extract("Slack", "Activity - PB - Slack", None, &re());
        assert!(a.is_empty());
        let a = extract(
            "thunderbird",
            "Re: contract draft - Mozilla Thunderbird",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Re: contract draft"]);
        let a = extract(
            "thunderbird",
            "Inbox - sam@acme.com - Mozilla Thunderbird",
            None,
            &re(),
        );
        assert!(a.is_empty());
        let a = extract(
            "obsidian",
            "Weekly review - Work - Obsidian v1.5",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Weekly review"]);
        assert_eq!(kinds(&a, AnchorKind::Place), ["work"]);
        let a = extract("okular", "paper.pdf — Okular", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Doc), ["paper.pdf"]);
        let a = extract(
            "libreoffice-writer",
            "invoice-2041.odt - LibreOffice Writer",
            None,
            &re(),
        );
        assert_eq!(kinds(&a, AnchorKind::Doc), ["invoice-2041.odt"]);
        let a = extract("Figma", "Checkout v2 – Figma", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Checkout v2"]);
        let a = extract("zoom", "Zoom Meeting", None, &re());
        assert!(a.is_empty());
        let a = extract("chronicle", "Chronicle", None, &re());
        assert!(a.is_empty());
        let a = extract("Spotify", "Daft Punk - Around the World", None, &re());
        assert_eq!(kinds(&a, AnchorKind::Doc), ["Daft Punk - Around the World"]);
    }

    fn ev(
        kind: ActivityKind,
        lo: i64,
        hi: i64,
        repo: &str,
        branch: &str,
        ext: &str,
        detail: Option<&str>,
    ) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(lo),
            end_ts: Some(ms_to_ts(hi)),
            repo: repo.into(),
            branch: branch.into(),
            kind,
            ext_id: Some(ext.into()),
            summary: None,
            detail: detail.map(str::to_owned),
        }
    }

    #[test]
    fn activity_attaches_by_family_and_overlap() {
        let r = re();
        let m = 60_000;
        let events = vec![
            ev(ActivityKind::Checkout, 0, 0, "chronicle", "m30", "", None),
            ev(
                ActivityKind::AiSession,
                10 * m,
                40 * m,
                "chronicle",
                "m30",
                "sess-1",
                Some(
                    r#"{"prompts":["anchors"],"paths":["crates/core/src/extract.rs","crates/core/src/storage.rs"]}"#,
                ),
            ),
            ev(
                ActivityKind::Edit,
                12 * m,
                20 * m,
                "mailer",
                "ACME-11382-sms",
                "mailer@x#1",
                Some(r#"{"path":"/home/j/dev/mailer/models.py"}"#),
            ),
            ev(
                ActivityKind::Meeting,
                30 * m,
                45 * m,
                "",
                "",
                "cal-9",
                Some(r#"{"attendees":["Priya Nair","ops@acme.com"]}"#),
            ),
            ev(
                ActivityKind::Shell,
                15 * m,
                16 * m,
                "mailer",
                "",
                "sh#1",
                None,
            ),
        ];
        // Terminal span with the AI session: session, place, branch, docs.
        let a = from_activity("Terminator", "", 11 * m, 30 * m, &[], &events, &r);
        assert!(has(&a, AnchorKind::Session, "sess-1"));
        assert!(has(&a, AnchorKind::Place, "chronicle"));
        assert!(has(&a, AnchorKind::Branch, "chronicle@m30"));
        assert_eq!(kinds(&a, AnchorKind::Doc), ["extract.rs", "storage.rs"]);
        assert!(has(&a, AnchorKind::Place, "mailer")); // shell fold
        assert!(!a.iter().any(|x| x.kind == AnchorKind::Event));
        // Editor span with the edit fold: branch carries the item.
        let a = from_activity("Code", "", 12 * m, 19 * m, &[], &events, &r);
        assert!(has(&a, AnchorKind::Branch, "mailer@ACME-11382-sms"));
        assert!(has(&a, AnchorKind::Item, "ACME-11382"));
        assert!(has(&a, AnchorKind::Place, "mailer"));
        assert!(has(&a, AnchorKind::Session, "sess-1"));
        // A browser span overlapping the meeting gets the event and people, not the session.
        let a = from_activity("Google-chrome", "", 31 * m, 35 * m, &[], &events, &r);
        assert!(has(&a, AnchorKind::Event, "cal-9"));
        assert_eq!(
            kinds(&a, AnchorKind::People),
            ["Priya Nair", "ops@acme.com"]
        );
        assert!(!a.iter().any(|x| x.kind == AnchorKind::Session));
        // A terminal that names another place does not take the session.
        let own = vec![Anchor::new(AnchorKind::Place, "mailer")];
        let a = from_activity("Terminator", "", 11 * m, 30 * m, &own, &events, &r);
        assert!(!has(&a, AnchorKind::Session, "sess-1"));
        assert!(has(&a, AnchorKind::Place, "mailer"));
        // Chronicle's own window hosts no tool session.
        let a = from_activity("chronicle", "", 11 * m, 30 * m, &[], &events, &r);
        assert!(!a.iter().any(|x| x.kind == AnchorKind::Session));
        // A span that names a place gets that repo's latest checkout as its branch.
        let own = vec![Anchor::new(AnchorKind::Place, "chronicle")];
        let a = from_activity("Terminator", "", 50 * m, 55 * m, &own, &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["chronicle@m30"]);
    }

    // m32 chunk 1: a call row is an event anchor like a calendar entry.
    #[test]
    fn calls_attach_as_events() {
        let r = re();
        let m = 60_000;
        let events = vec![ev(
            ActivityKind::Call,
            30 * m,
            45 * m,
            "",
            "",
            "call:1800000",
            None,
        )];
        let a = from_activity("Firefox", "", 32 * m, 40 * m, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Event), ["call:1800000"]);
        let a = from_activity("Firefox", "", 50 * m, 55 * m, &[], &events, &r);
        assert!(kinds(&a, AnchorKind::Event).is_empty());
    }

    #[test]
    fn concurrent_sessions_attach_by_nearest_write() {
        let r = re();
        let m = 60_000;
        let writes = |ts: &[i64]| {
            let w: Vec<i64> = ts.iter().map(|t| t * m).collect();
            serde_json::json!({ "prompts": [], "paths": [], "writes": w }).to_string()
        };
        let events = vec![
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m30",
                "sess-a",
                Some(&writes(&[0, 5, 12, 40])),
            ),
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m31",
                "sess-b",
                Some(&writes(&[1, 8, 25])),
            ),
        ];
        // sess-a wrote at 12, inside [10, 15): it wins over sess-b's 8.
        let a = from_activity("Terminator", "", 10 * m, 15 * m, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["chronicle@m30"]);
        // Nothing written in [20, 24): the most recent write before it (sess-a at 12
        // vs sess-b at 8) still picks sess-a; at [26, 30) sess-b's 25 is nearest.
        let a = from_activity("Terminator", "", 20 * m, 24 * m, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        let a = from_activity("Terminator", "", 26 * m, 30 * m, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-b"]);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["chronicle@m31"]);
        // Rows without write times (captured before m30 chunk 2.5) all attach.
        let legacy = vec![
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m30",
                "sess-a",
                None,
            ),
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m31",
                "sess-b",
                None,
            ),
        ];
        let a = from_activity("Terminator", "", 10 * m, 15 * m, &[], &legacy, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a", "sess-b"]);
    }

    #[test]
    fn bare_terminal_takes_the_last_seen_place() {
        let r = re();
        let m = 60_000;
        let events = vec![
            ev(
                ActivityKind::Cwd,
                0,
                12 * m,
                "chronicle",
                "",
                "cwd:1:chronicle",
                None,
            ),
            ev(
                ActivityKind::Cwd,
                11 * m,
                30 * m,
                "mailer",
                "",
                "cwd:1:mailer",
                None,
            ),
            ev(ActivityKind::Checkout, 0, 0, "chronicle", "m30", "", None),
            ev(
                ActivityKind::Checkout,
                0,
                0,
                "mailer",
                "ACME-1-x",
                "",
                None,
            ),
        ];
        // Both rows overlap [10, 20); the shell was last seen in mailer.
        let a = from_activity("Terminator", "", 10 * m, 20 * m, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Place), ["mailer"]);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["mailer@ACME-1-x"]);
        // A short visit to mailer that ended before the span did leaves the
        // span in chronicle, where the shell still is.
        let visit = vec![
            ev(
                ActivityKind::Cwd,
                0,
                40 * m,
                "chronicle",
                "",
                "cwd:1:chronicle",
                None,
            ),
            ev(
                ActivityKind::Cwd,
                11 * m,
                12 * m,
                "mailer",
                "",
                "cwd:1:mailer",
                None,
            ),
        ];
        let a = from_activity("Terminator", "", 10 * m, 20 * m, &[], &visit, &r);
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        // A terminal naming its own place keeps it; other places' rows add nothing.
        let own = vec![Anchor::new(AnchorKind::Place, "chronicle")];
        let a = from_activity("Terminator", "", 10 * m, 20 * m, &own, &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Place), ["chronicle"]);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["chronicle@m30"]);
    }

    // m32 chunk 2: the title the tool wrote names the session; a typed
    // prompt beats a transcript write; a bare terminal with no live cwd
    // row stays unattached.
    #[test]
    fn sessions_attach_by_title_then_prompt() {
        let r = re();
        let m = 60_000;
        let detail = |writes: &[i64], prompts: &[i64], titles: &[&str]| {
            serde_json::json!({
                "prompts": [], "paths": [],
                "writes": writes.iter().map(|t| t * m).collect::<Vec<_>>(),
                "prompt_minutes": prompts.iter().map(|t| t * m).collect::<Vec<_>>(),
                "titles": titles,
            })
            .to_string()
        };
        let events = vec![
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m32",
                "sess-a",
                Some(&detail(&[0, 5, 12, 40], &[0, 11], &["Attention plan"])),
            ),
            ev(
                ActivityKind::AiSession,
                0,
                60 * m,
                "chronicle",
                "m32",
                "sess-b",
                Some(&detail(
                    &[1, 8, 14, 25],
                    &[1, 13],
                    &["Site refresh", "Hero loop"],
                )),
            ),
        ];
        // The title wins over both prompt and write recency (sess-b typed
        // at 13 and wrote at 14, inside the span).
        let a = from_activity(
            "Terminator",
            "✳ Attention plan",
            10 * m,
            15 * m,
            &[],
            &events,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        assert_eq!(kinds(&a, AnchorKind::Branch), ["chronicle@m32"]);
        // Any title the session carried, glyph and case aside.
        let a = from_activity(
            "Terminator",
            "◐ hero loop",
            10 * m,
            15 * m,
            &[],
            &events,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-b"]);
        // The title stays on screen after the transcript's last write: a
        // titled session that ended before the span still owns it, even
        // against a session that overlaps.
        let mut later = events.clone();
        later.push(ev(
            ActivityKind::AiSession,
            65 * m,
            90 * m,
            "chronicle",
            "m32",
            "sess-c",
            Some(&detail(&[65, 72], &[65, 71], &["Site refresh"])),
        ));
        let a = from_activity(
            "Terminator",
            "✳ Attention plan",
            70 * m,
            75 * m,
            &[],
            &later,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        let a = from_activity(
            "Terminator",
            "✳ Claude Code",
            70 * m,
            75 * m,
            &[],
            &later,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-c"]);
        // No title match: the nearest prompt at or before the end (sess-b at
        // 13) wins. Over [12, 12.5) the write rule would pick sess-a's 12
        // too, so compare prompts: sess-a's 11 beats sess-b's 1.
        let a = from_activity(
            "Terminator",
            "✳ Claude Code",
            10 * m,
            15 * m,
            &[],
            &events,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-b"]);
        let a = from_activity(
            "Terminator",
            "✳ Claude Code",
            12 * m,
            12 * m + m / 2,
            &[],
            &events,
            &r,
        );
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        // A span before any prompt falls back to writes (sess-b wrote at 1).
        let a = from_activity("Terminator", "", 0, 1, &[], &events, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-a"]);
        let early = vec![ev(
            ActivityKind::AiSession,
            0,
            60 * m,
            "chronicle",
            "m32",
            "sess-c",
            Some(&detail(&[0, 3], &[9], &[])),
        )];
        let a = from_activity("Terminator", "", 2 * m, 4 * m, &[], &early, &r);
        assert_eq!(kinds(&a, AnchorKind::Session), ["sess-c"]);

        // A bare terminal whose cwd rows all ended before the span did
        // names no place; a shell fold (one-shot) still does.
        let gone = vec![ev(
            ActivityKind::Cwd,
            0,
            5 * m,
            "mailer",
            "",
            "cwd:1:mailer",
            None,
        )];
        let a = from_activity("Terminator", "", 4 * m, 20 * m, &[], &gone, &r);
        assert!(kinds(&a, AnchorKind::Place).is_empty(), "{a:?}");
        let fold = vec![ev(
            ActivityKind::Shell,
            6 * m,
            6 * m,
            "mailer",
            "",
            "sh#1",
            None,
        )];
        let a = from_activity("Terminator", "", 4 * m, 20 * m, &[], &fold, &r);
        assert_eq!(kinds(&a, AnchorKind::Place), ["mailer"]);
    }

    #[test]
    fn session_docs_follow_the_touches() {
        let m = 60_000;
        let detail = serde_json::json!({
            "prompts": [], "paths": ["a.rs", "b.rs", "c.rs", "d.rs"], "writes": [],
            "touches": [[0, 0], [m, 1], [30 * m, 2], [31 * m, 2], [50 * m, 3]],
        })
        .to_string();
        // Inside the grace window before the span: c.rs, not the morning's
        // a.rs/b.rs; nothing after the span's end.
        assert_eq!(session_docs(Some(&detail), 40 * m, 45 * m), ["c.rs"]);
        // A span long after the last touch takes the latest touch minute.
        assert_eq!(session_docs(Some(&detail), 200 * m, 205 * m), ["d.rs"]);
        // Nothing touched yet: no docs. Legacy rows: the first paths.
        assert!(session_docs(Some(&detail), -10 * m, -5 * m).is_empty());
        let legacy = r#"{"prompts":[],"paths":["a.rs","b.rs"]}"#;
        assert_eq!(session_docs(Some(legacy), 0, m), ["a.rs", "b.rs"]);
        assert!(session_docs(None, 0, m).is_empty());
    }

    #[test]
    fn places_from_paths() {
        assert_eq!(
            place_from_path("/home/james/dev/chronicle/crates").as_deref(),
            Some("chronicle")
        );
        assert_eq!(place_from_path("/home/james"), None);
        assert_eq!(place_from_path("/"), None);
        assert_eq!(place_from_path("/srv/app").as_deref(), Some("app"));
        assert_eq!(
            place_from_path("/Users/jc/Documents/Acme Ltd/x").as_deref(),
            Some("acme ltd")
        );
    }

    #[test]
    fn url_helpers() {
        assert_eq!(registrable("docs.google.com"), "google.com");
        assert_eq!(registrable("acme.atlassian.net"), "atlassian.net");
        assert_eq!(registrable("shop.co.uk"), "shop.co.uk");
        assert_eq!(registrable("a.b.shop.co.uk"), "shop.co.uk");
        assert_eq!(percent_decode("Roadmap%202027"), "Roadmap 2027");
        assert_eq!(percent_decode("100%"), "100%");
        assert!(parse_url("about:blank").is_none());
        let u = parse_url("https://user@github.com:443/a/b?x=1#f").unwrap();
        assert_eq!(u.host, "github.com");
        assert_eq!(u.segs, ["a", "b"]);
    }
}
