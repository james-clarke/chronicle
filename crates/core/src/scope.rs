//! Declared task scope (m44 chunk 1): a task's ticket plus what the person
//! pinned to it — repos, branches, documents, sites, work items. A span
//! that carries any of them files to the task's project ahead of the
//! project rules, and the task is the sink for the stretch, so a ticket
//! whose prefix a sibling project shares still lands on the task that
//! holds it.

use crate::extract::{Anchor, AnchorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Repo,
    Branch,
    Doc,
    Domain,
    Item,
}

impl ScopeKind {
    pub const ALL: [ScopeKind; 5] = [
        ScopeKind::Repo,
        ScopeKind::Branch,
        ScopeKind::Doc,
        ScopeKind::Domain,
        ScopeKind::Item,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Repo => "repo",
            ScopeKind::Branch => "branch",
            ScopeKind::Doc => "doc",
            ScopeKind::Domain => "domain",
            ScopeKind::Item => "item",
        }
    }

    pub fn parse(s: &str) -> Option<ScopeKind> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// One declared task's scope, for filing and for the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScope {
    pub task_id: i64,
    pub project: Option<String>,
    /// `tasks.external_ref`.
    pub ticket: Option<String>,
    /// The scope holds from here: the start of the local day the task was
    /// declared, so the morning's spans before the declare count and the
    /// years before do not.
    pub since: i64,
    /// When the person closed it: the scope holds only the spans before.
    pub closed_ts: Option<i64>,
    pub entries: Vec<(ScopeKind, String)>,
}

impl TaskScope {
    /// The first thing in the scope the anchors carry, as it reads: the
    /// ticket, or a pinned value. `None` when nothing matches.
    pub fn matches(&self, anchors: &[Anchor]) -> Option<String> {
        for a in anchors {
            match a.kind {
                AnchorKind::Item | AnchorKind::Change => {
                    if let Some(t) = &self.ticket
                        && a.value.eq_ignore_ascii_case(t)
                    {
                        return Some(t.clone());
                    }
                    if let Some(v) =
                        self.entry(ScopeKind::Item, |v| a.value.eq_ignore_ascii_case(v))
                    {
                        return Some(v);
                    }
                }
                AnchorKind::Place => {
                    if let Some(v) = self.entry(ScopeKind::Repo, |v| {
                        folder(v) == a.value.to_ascii_lowercase()
                    }) {
                        return Some(v);
                    }
                }
                AnchorKind::Branch => {
                    let (repo, branch) = a
                        .value
                        .split_once('@')
                        .map_or(("", a.value.as_str()), |(r, b)| (r, b));
                    if !repo.is_empty()
                        && let Some(v) =
                            self.entry(ScopeKind::Repo, |v| folder(v) == repo.to_ascii_lowercase())
                    {
                        return Some(v);
                    }
                    if let Some(v) = self.entry(ScopeKind::Branch, |v| {
                        let want = v.split_once('@').map_or(v, |(_, b)| b);
                        branch.eq_ignore_ascii_case(want)
                    }) {
                        return Some(v);
                    }
                }
                AnchorKind::Domain => {
                    let host = a.value.to_ascii_lowercase();
                    if let Some(v) = self.entry(ScopeKind::Domain, |v| {
                        let d = v.to_ascii_lowercase();
                        host == d
                            || host
                                .strip_suffix(d.as_str())
                                .is_some_and(|r| r.ends_with('.'))
                    }) {
                        return Some(v);
                    }
                }
                AnchorKind::Doc | AnchorKind::Link => {
                    // The pinned value starts the name, whole words: "Agent
                    // design" takes "agent design v2.md", "notes" does not
                    // take "Sticky Notes" or "release-notes.md".
                    let have = a.value.trim();
                    if let Some(v) = self.entry(ScopeKind::Doc, |v| starts_with_words(have, v)) {
                        return Some(v);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn entry(&self, kind: ScopeKind, pred: impl Fn(&str) -> bool) -> Option<String> {
        self.entries
            .iter()
            .find(|(k, v)| *k == kind && pred(v))
            .map(|(_, v)| v.clone())
    }
}

/// `name` begins with `prefix` (ASCII case-insensitive) and the match ends
/// at a word boundary: the end of `name`, or a character that is not a
/// letter or digit.
fn starts_with_words(name: &str, prefix: &str) -> bool {
    let prefix = prefix.trim();
    if prefix.is_empty() || name.len() < prefix.len() || !name.is_char_boundary(prefix.len()) {
        return false;
    }
    name[..prefix.len()].eq_ignore_ascii_case(prefix)
        && name[prefix.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric())
}

impl TaskScope {
    /// Whether the scope holds at `ts`: from the task's day on, and before
    /// the close once closed.
    pub fn holds_at(&self, ts: i64) -> bool {
        ts >= self.since && self.closed_ts.is_none_or(|c| ts <= c)
    }
}

/// The last path segment, lowercased: a repo entry may be a path or a
/// folder name, a place anchor is the folder.
fn folder(v: &str) -> String {
    v.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(v)
        .to_ascii_lowercase()
}

/// The first task whose scope the anchors carry and that holds at `ts`
/// (oldest task first, as `scopes` comes), with what matched.
pub fn holder<'a>(
    scopes: &'a [TaskScope],
    anchors: &[Anchor],
    ts: i64,
) -> Option<(&'a TaskScope, String)> {
    scopes
        .iter()
        .filter(|s| s.holds_at(ts))
        .find_map(|s| s.matches(anchors).map(|m| (s, m)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(kind: AnchorKind, value: &str) -> Anchor {
        Anchor {
            kind,
            value: value.into(),
        }
    }

    fn scope() -> TaskScope {
        TaskScope {
            task_id: 183,
            project: Some("acme-ai".into()),
            ticket: Some("ACME-11342".into()),
            since: 10,
            closed_ts: None,
            entries: vec![
                (ScopeKind::Repo, "~/dev/agent-backend".into()),
                (ScopeKind::Branch, "feat/ACME-11342-agent".into()),
                (ScopeKind::Domain, "docs.example.com".into()),
                (ScopeKind::Doc, "Agent design".into()),
                (ScopeKind::Item, "ACME-11350".into()),
            ],
        }
    }

    #[test]
    fn matches_ticket_and_each_pinned_kind() {
        let s = scope();
        let m = |k, v| s.matches(&[anchor(k, v)]);
        assert_eq!(
            m(AnchorKind::Item, "acme-11342").as_deref(),
            Some("ACME-11342")
        );
        assert_eq!(
            m(AnchorKind::Item, "ACME-11350").as_deref(),
            Some("ACME-11350")
        );
        assert_eq!(m(AnchorKind::Item, "ACME-11351"), None);
        assert_eq!(
            m(AnchorKind::Place, "Agent-Backend").as_deref(),
            Some("~/dev/agent-backend")
        );
        assert_eq!(
            m(AnchorKind::Branch, "agent-backend@main").as_deref(),
            Some("~/dev/agent-backend")
        );
        assert_eq!(
            m(AnchorKind::Branch, "other@feat/acme-11342-agent").as_deref(),
            Some("feat/ACME-11342-agent")
        );
        assert_eq!(
            m(AnchorKind::Domain, "api.docs.example.com").as_deref(),
            Some("docs.example.com")
        );
        assert_eq!(m(AnchorKind::Domain, "notdocs.example.com"), None);
        assert_eq!(
            m(AnchorKind::Doc, "agent design v2.md").as_deref(),
            Some("Agent design")
        );
        assert_eq!(
            m(AnchorKind::Doc, "Agent design").as_deref(),
            Some("Agent design")
        );
        assert_eq!(m(AnchorKind::Doc, "Agent designs.md"), None);
        assert_eq!(m(AnchorKind::Doc, "Notes on agent design"), None);
        assert_eq!(m(AnchorKind::People, "bob"), None);
    }

    #[test]
    fn holder_respects_the_close() {
        let mut s = scope();
        s.closed_ts = Some(100);
        let scopes = [s];
        let a = [anchor(AnchorKind::Item, "ACME-11342")];
        assert!(holder(&scopes, &a, 50).is_some());
        assert!(holder(&scopes, &a, 150).is_none());
        assert!(holder(&scopes, &a, 5).is_none(), "before the task's day");
        assert_eq!(ScopeKind::parse("doc"), Some(ScopeKind::Doc));
        assert_eq!(ScopeKind::parse("x"), None);
    }
}
