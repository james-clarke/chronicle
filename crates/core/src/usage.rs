//! What you work with (m41 chunk 3): the tools Chronicle saw the person use
//! over the last month — window classes and browser domains from the focus
//! spans — set against what the registry can read, and the list the person
//! declares by hand. The mined list is the demand signal no form can
//! replace: it is weighted by minutes, not enthusiasm.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use jiff::tz::TimeZone;
use jiff::{Timestamp, civil};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::connectors::{self, Platform, Support};
use crate::extract::{self, Family};
use crate::sessionizer::domain;
use crate::storage::{self, StorageError};

/// How a tool showed up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Seen {
    /// A window class the focus tracker reported.
    App,
    /// A domain from a browser tab's URL.
    Domain,
}

/// One mined tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// The window class or the domain, as captured.
    pub name: String,
    pub seen: Seen,
    pub family: Family,
    pub minutes: i64,
    /// Distinct local days it was in focus.
    pub days: usize,
    /// The connector that reads this tool (or plans to), if any.
    pub claimed_by: Option<&'static connectors::Connector>,
}

impl Tool {
    /// Unclaimed by any connector that actually reads today: the rows a
    /// person can request.
    pub fn unread(&self) -> bool {
        !self
            .claimed_by
            .is_some_and(|c| matches!(c.state, Support::Supported | Support::Partial { .. }))
    }

    /// The window class made readable: `Sublime_merge` → `Sublime merge`.
    pub fn label(&self) -> String {
        match self.seen {
            Seen::Domain => self.name.clone(),
            Seen::App => {
                let s = self.name.replace(['_', '-'], " ");
                let mut chars = s.chars();
                match chars.next() {
                    Some(c) => c.to_uppercase().chain(chars).collect(),
                    None => s,
                }
            }
        }
    }
}

/// What a connector reads, in terms a focus span has: an app family, a
/// window-class fragment, or a domain suffix. Kept beside the registry
/// rather than in it: the descriptor says what a connector *produces*;
/// this says which of the person's windows that covers.
enum Claim {
    Family(Family),
    App(&'static str),
    Domain(&'static str),
}

const CLAIMS: &[(&str, Claim)] = &[
    ("browser_history", Claim::Family(Family::Browser)),
    ("shell_hook", Claim::Family(Family::Terminal)),
    ("editor_workspaces", Claim::Family(Family::Editor)),
    ("git_repos", Claim::Family(Family::Vcs)),
    ("github_prs", Claim::Domain("github.com")),
    ("gitlab_mrs", Claim::Domain("gitlab.com")),
    ("jira", Claim::Domain("atlassian.net")),
    ("linear", Claim::Domain("linear.app")),
    ("sentry", Claim::Domain("sentry.io")),
    ("slack", Claim::App("slack")),
    ("slack", Claim::Domain("slack.com")),
    ("google_calendar", Claim::Domain("meet.google.com")),
    ("google_calendar", Claim::Domain("calendar.google.com")),
    ("listening_ports", Claim::Domain("localhost")),
    ("listening_ports", Claim::Domain("127.0.0.1")),
    ("tmux", Claim::App("tmux")),
    ("docker_compose", Claim::App("docker")),
];

fn claim_for(seen: Seen, name: &str, family: Family) -> Option<&'static connectors::Connector> {
    let lower = name.to_ascii_lowercase();
    let id = CLAIMS.iter().find_map(|(id, claim)| match (seen, claim) {
        (Seen::App, Claim::App(frag)) if lower.contains(frag) => Some(*id),
        (Seen::App, Claim::Family(f)) if *f == family => Some(*id),
        (Seen::Domain, Claim::Domain(suffix))
            if lower == *suffix || lower.ends_with(&format!(".{suffix}")) =>
        {
            Some(*id)
        }
        _ => None,
    })?;
    connectors::REGISTRY.iter().find(|c| c.id == id)
}

/// Focus minutes below which a tool is noise, not a tool.
pub const MIN_MINUTES: i64 = 5;
/// A tool seen on one day only is a visit, not something you work with.
pub const MIN_DAYS: usize = 2;

/// Every app and domain in focus over the last `days` days, ranked by
/// minutes. Chronicle's own window is left out; so is anything under
/// [`MIN_MINUTES`] or seen on fewer than [`MIN_DAYS`] days. Browser windows
/// count as their domain when the span carries a URL and as the browser
/// otherwise, which is what the history route or the extension would have
/// read.
pub fn mined(
    conn: &Connection,
    cfg: &Config,
    tz: &TimeZone,
    now_ms: i64,
    days: i64,
) -> Result<Vec<Tool>, StorageError> {
    let since = now_ms - days * 86_400_000;
    let mut stmt = conn.prepare(
        "SELECT app, url, start_ts, end_ts FROM spans \
         WHERE kind = 'focus' AND start_ts >= ?1 AND end_ts > start_ts",
    )?;
    struct Acc {
        family: Family,
        ms: i64,
        days: HashSet<civil::Date>,
    }
    let mut acc: BTreeMap<(Seen, String), Acc> = BTreeMap::new();
    let rows = stmt.query_map([since], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (app, url, start, end) = row?;
        if app.is_empty() || app.eq_ignore_ascii_case("chronicle") {
            continue;
        }
        let family = extract::family(&app);
        let is_browser = family == Family::Browser
            || cfg
                .browser_apps
                .iter()
                .any(|b| app.to_lowercase().contains(&b.to_lowercase()));
        // Port dropped so every local dev server is one `localhost` row;
        // `about:blank`, `newtab` and friends have no dot and are not sites.
        let site = url
            .as_deref()
            .map(domain)
            .map(|d| d.split(':').next().unwrap_or(d))
            .filter(|d| d.contains('.') || *d == "localhost");
        let key = match site {
            Some(d) if is_browser => (Seen::Domain, d.to_owned()),
            Some(_) => (Seen::App, app.clone()),
            None if is_browser && url.is_some() => continue,
            None => (Seen::App, app.clone()),
        };
        let day = Timestamp::from_millisecond(start)
            .map(|t| t.to_zoned(tz.clone()).date())
            .ok();
        let e = acc.entry(key).or_insert_with(|| Acc {
            family,
            ms: 0,
            days: HashSet::new(),
        });
        e.ms += end - start;
        if let Some(d) = day {
            e.days.insert(d);
        }
    }
    let mut out: Vec<Tool> = acc
        .into_iter()
        .filter(|(_, a)| a.ms / 60_000 >= MIN_MINUTES && a.days.len() >= MIN_DAYS)
        .map(|((seen, name), a)| Tool {
            claimed_by: claim_for(seen, &name, a.family),
            family: a.family,
            minutes: a.ms / 60_000,
            days: a.days.len(),
            name,
            seen,
        })
        .collect();
    out.sort_by(|a, b| b.minutes.cmp(&a.minutes).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// What the person says they use: connector ids ticked in Settings, and a
/// free line for anything the registry has no row for. Stored locally in
/// `meta`; never sent on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declared {
    #[serde(default)]
    pub ids: BTreeSet<String>,
    #[serde(default)]
    pub other: String,
}

pub const DECLARED_KEY: &str = "declared_tools";

impl Declared {
    pub fn load(conn: &Connection) -> Declared {
        storage::get_meta(conn, DECLARED_KEY)
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, conn: &Connection) -> Result<(), StorageError> {
        let json = serde_json::to_string(self).unwrap_or_default();
        storage::set_meta(conn, DECLARED_KEY, Some(&json))
    }

    pub fn uses(&self, id: &str) -> bool {
        self.ids.contains(id)
    }
}

/// `h:mm`-style minutes for a chip: `4 h 12 m`, `35 m`.
pub fn minutes_label(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} m"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} m"),
    }
}

/// Percent-encode for a query string: RFC 3986 unreserved characters pass,
/// everything else (spaces, newlines, backticks, `#`) is `%XX`.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub const ISSUES_URL: &str = "https://github.com/james-clarke/chronicle/issues/new";
pub const REQUEST_LABEL: &str = "integration-request";

/// A prefilled GitHub issue URL for an integration request (m41 chunk 4
/// grows the body; this is the shape both share). Opens in the person's
/// browser under their own account — Chronicle makes no request itself.
pub fn request_url(title: &str, body: &str) -> String {
    format!(
        "{ISSUES_URL}?labels={REQUEST_LABEL}&title={}&body={}",
        encode(title),
        encode(body)
    )
}

impl Tool {
    /// The one-word kind for a chip or a request's category.
    pub fn category(&self) -> &'static str {
        match (self.seen, self.family) {
            (Seen::Domain, _) => "site",
            (_, Family::Terminal) => "terminal",
            (_, Family::Editor) => "editor",
            (_, Family::Vcs) => "git client",
            (_, Family::Browser) => "browser",
            (_, Family::Chat) => "chat",
            (_, Family::Mail) => "mail",
            (_, Family::Meeting) => "meetings",
            (_, Family::Document) => "documents",
            (_, Family::Other) => "app",
        }
    }
}

/// The platform word the request carries.
pub fn platform_label() -> &'static str {
    match Platform::HOST {
        Platform::Linux => "Linux",
        Platform::MacOs => "macOS",
        Platform::Windows => "Windows",
    }
}

/// The window title the app spent most of the window in: one line of
/// evidence a maintainer can recognise the tool by. Titles are the private
/// part of a span, so the caller redacts it and shows it as a removable chip.
pub fn sample_title(conn: &Connection, app: &str, since_ms: i64) -> Option<String> {
    conn.query_row(
        "SELECT title FROM spans WHERE kind = 'focus' AND app = ?1 AND start_ts >= ?2 \
         AND title <> '' GROUP BY title ORDER BY SUM(end_ts - start_ts) DESC LIMIT 1",
        rusqlite::params![app, since_ms],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// One integration request (m41 chunk 4). `body()` is the issue body in the
/// shape `.github/ISSUE_TEMPLATE/integration-request.yml` produces for a
/// hand-filed issue, so both routes land the same; the URL carries it
/// verbatim, so the preview *is* the issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub tool: String,
    pub category: String,
    pub platform: String,
    pub why: String,
    /// Kept evidence lines, already redacted.
    pub evidence: Vec<String>,
}

impl Request {
    pub fn for_tool(t: &Tool, sample_title: Option<String>, days_window: i64) -> Request {
        let what = match t.seen {
            Seen::App => "window class",
            Seen::Domain => "domain",
        };
        let mut evidence = vec![
            format!(
                "seen {} over {} days in the last {days_window}",
                minutes_label(t.minutes),
                t.days
            ),
            format!("{what} `{}`", t.name),
        ];
        if let Some(title) = sample_title {
            evidence.push(format!("sample title `{title}`"));
        }
        Request {
            tool: t.label(),
            category: t.category().to_owned(),
            platform: platform_label().to_owned(),
            why: String::new(),
            evidence,
        }
    }

    pub fn for_connector(c: &connectors::Connector) -> Request {
        let state = match c.state {
            Support::Planned => "planned".to_owned(),
            Support::Detected => "detected, not supported".to_owned(),
            Support::WontDo { reason } => format!("not planned: {reason}"),
            Support::Partial { note } => format!("partial: {note}"),
            Support::Supported => "supported".to_owned(),
        };
        Request {
            tool: c.name.to_owned(),
            category: c.kind.label().to_ascii_lowercase(),
            platform: platform_label().to_owned(),
            why: String::new(),
            evidence: vec![format!("registry row `{}`, {state}", c.id)],
        }
    }

    pub fn title(&self) -> String {
        format!("Integration request: {}", self.tool.trim())
    }

    /// GitHub renders an issue-form submission as `### Label` blocks with
    /// `_No response_` for an empty optional field; this is that shape.
    pub fn body(&self) -> String {
        fn block(label: &str, value: &str) -> String {
            let v = value.trim();
            format!(
                "### {label}\n\n{}\n\n",
                if v.is_empty() { "_No response_" } else { v }
            )
        }
        let evidence = if self.evidence.is_empty() {
            String::new()
        } else {
            self.evidence
                .iter()
                .map(|l| format!("- {l}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut out = String::new();
        out.push_str(&block("Tool", &self.tool));
        out.push_str(&block("Category", &self.category));
        out.push_str(&block("Platform", &self.platform));
        out.push_str(&block("What I'd want out of it", &self.why));
        out.push_str(&block("Evidence", &evidence));
        out.trim_end().to_owned() + "\n"
    }

    pub fn url(&self) -> String {
        request_url(&self.title(), &self.body())
    }

    /// The file a person without a GitHub account keeps: title, then body.
    pub fn markdown(&self) -> String {
        format!("# {}\n\n{}", self.title(), self.body())
    }

    /// A file name for `markdown()`: the tool, lowercased, non-alphanumerics
    /// folded to one dash.
    pub fn slug(&self) -> String {
        let mut out = String::new();
        for c in self.tool.to_lowercase().chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.ends_with('-') {
                out.push('-');
            }
        }
        let s = out.trim_matches('-');
        if s.is_empty() {
            "request".to_owned()
        } else {
            s.to_owned()
        }
    }
}

/// Local counters behind the "is issue friction losing signal" question
/// (m41 decision 3): composed vs. actually opened in the browser.
pub const REQUESTS_COMPOSED: &str = "requests_composed";
pub const REQUESTS_FILED: &str = "requests_filed";

pub fn bump(conn: &Connection, key: &str) {
    let n: i64 = storage::get_meta(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let _ = storage::set_meta(conn, key, Some(&(n + 1).to_string()));
}
