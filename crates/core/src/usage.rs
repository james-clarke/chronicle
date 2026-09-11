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
use crate::connectors::{self, Support};
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

/// The request for a mined tool: what it is and how much of the month it
/// took, the two facts a maintainer ranks by.
pub fn request_for_tool(t: &Tool, days_window: i64) -> (String, String) {
    let what = match t.seen {
        Seen::App => "app",
        Seen::Domain => "site",
    };
    let title = format!("Integration request: {}", t.label());
    let body = format!(
        "**Tool:** {} ({what})\n**Seen:** {} over {} days in the last {days_window}\n\n\
         **What I'd want out of it:**\n\n",
        t.label(),
        minutes_label(t.minutes),
        t.days
    );
    (title, body)
}

/// The request for a registry row the person ticked but Chronicle cannot
/// read yet.
pub fn request_for_connector(c: &connectors::Connector) -> (String, String) {
    let title = format!("Integration request: {}", c.name);
    let body = format!(
        "**Tool:** {} (`{}`, {})\n**Status in the registry:** {}\n\n\
         **What I'd want out of it:**\n\n",
        c.name,
        c.id,
        c.kind.label(),
        match c.state {
            Support::Planned => "planned",
            Support::Detected => "detected, not supported",
            Support::WontDo { reason } => reason,
            Support::Partial { note } => note,
            Support::Supported => "supported",
        }
    );
    (title, body)
}
