//! Projects (m35 chunk 0): configuration matched by rule, no model in the
//! loop. A project is named by James, identified by the git remote of its
//! repos (`host/org/repo`), and instanced by the repo paths and their
//! worktrees. Every focus span is filed into the first project whose rule
//! matches it, in the order repo path or place → ticket prefix → domain →
//! title regex → app, else left unfiled.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::config::{Config, ProjectCfg, expand_home};
use crate::extract::{self, Anchor, AnchorKind};

/// One project compiled for matching.
#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub derive: bool,
    /// Every instance path: the configured repos, expanded, plus their
    /// worktrees.
    pub paths: Vec<PathBuf>,
    /// Lowercased folder names of `paths` — what a place anchor carries.
    pub places: HashSet<String>,
    /// `org/repo` per remote, lowercased, for `org/repo#123` keys.
    pub slugs: HashSet<String>,
    /// Uppercased ticket prefixes.
    pub tickets: Vec<String>,
    /// Lowercased sites.
    pub domains: Vec<String>,
    pub titles: Vec<Regex>,
    /// Lowercased app names.
    pub apps: Vec<String>,
    /// `host/path` globs from the repos' link files (m37 chunk 2,
    /// `links::read_links`): a dashboard page files here by its Link anchor.
    pub links: Vec<String>,
    /// Found under a configured repo's parent, not configured (m37 chunk 2).
    pub discovered: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Matcher {
    pub projects: Vec<Project>,
}

impl Matcher {
    pub fn from_config(config: &Config) -> Self {
        let mut m = Self::new(&config.projects_effective());
        if config.discover_repos {
            m.discover();
        }
        m
    }

    /// Append every git repo found one level under a configured repo's
    /// parent that no project claims, as a discovered project named after
    /// its folder (m37 chunk 2): time in `~/dev/continental` files without
    /// a config edit. A folder whose name a configured project already has
    /// is skipped.
    pub fn discover(&mut self) {
        let known: Vec<PathBuf> = self.projects.iter().flat_map(|p| p.paths.clone()).collect();
        let parents = parents_of(&known);
        for repo in discover_repos(&parents, &known) {
            if self
                .projects
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(&repo.name))
            {
                continue;
            }
            let mut slugs = HashSet::new();
            if let Some(slug) = repo
                .remote
                .as_deref()
                .and_then(|r| r.split_once('/').map(|(_, s)| s))
            {
                slugs.insert(slug.to_ascii_lowercase());
            }
            let links = link_patterns(std::slice::from_ref(&repo.path));
            let mut places = HashSet::new();
            places.insert(repo.name.clone());
            self.projects.push(Project {
                name: repo.name,
                derive: true,
                paths: vec![repo.path],
                places,
                slugs,
                tickets: Vec::new(),
                domains: Vec::new(),
                titles: Vec::new(),
                apps: Vec::new(),
                links,
                discovered: true,
            });
        }
    }

    pub fn new(cfgs: &[ProjectCfg]) -> Self {
        let projects = cfgs
            .iter()
            .filter(|c| !c.name.trim().is_empty())
            .map(|c| {
                let mut paths = Vec::new();
                let mut slugs = HashSet::new();
                for repo in &c.repos {
                    let root = expand_home(repo);
                    if let Some(remote) = remote_of(&root)
                        && let Some(slug) = remote.split_once('/').map(|(_, s)| s)
                    {
                        slugs.insert(slug.to_ascii_lowercase());
                    }
                    for p in worktrees_of(&root) {
                        if !paths.contains(&p) {
                            paths.push(p);
                        }
                    }
                    if !paths.contains(&root) {
                        paths.push(root);
                    }
                }
                let places = paths
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())
                    .collect();
                let links = link_patterns(&paths);
                Project {
                    name: c.name.trim().to_owned(),
                    derive: c.derive,
                    links,
                    discovered: false,
                    paths,
                    places,
                    slugs,
                    tickets: c
                        .tickets
                        .iter()
                        .map(|t| t.trim().to_ascii_uppercase())
                        .filter(|t| !t.is_empty())
                        .collect(),
                    domains: c
                        .domains
                        .iter()
                        .map(|d| d.trim().to_ascii_lowercase())
                        .filter(|d| !d.is_empty())
                        .collect(),
                    titles: c.titles.iter().filter_map(|t| Regex::new(t).ok()).collect(),
                    apps: c
                        .apps
                        .iter()
                        .map(|a| a.trim().to_ascii_lowercase())
                        .filter(|a| !a.is_empty())
                        .collect(),
                }
            })
            .collect();
        Self { projects }
    }

    pub fn is_empty(&self) -> bool {
        self.projects.is_empty()
    }

    /// The configured name `name` stands for: a project named so in any
    /// case, else the one whose instance folder it names (a task's project
    /// from before m35 was a place; `contoso` resolves to `acme`).
    /// `None` for a name no project claims.
    pub fn resolve(&self, name: &str) -> Option<&str> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        if let Some(p) = self
            .projects
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            return Some(&p.name);
        }
        let lower = name.to_ascii_lowercase();
        self.projects
            .iter()
            .find(|p| p.places.contains(&lower))
            .map(|p| p.name.as_str())
    }

    /// Whether `name` mints derived tasks: as configured, and yes for a
    /// project no rule knows (unfiled time keeps deriving).
    pub fn derives(&self, name: &str) -> bool {
        self.projects
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .is_none_or(|p| p.derive)
    }
}

/// Rewrite every task's project to the configured name it resolves to
/// (m35 chunk 1: a task lives inside a configured project or is unfiled);
/// one no project claims becomes `None` for placement, so its task is a
/// candidate nowhere but the unfiled runs.
pub fn normalize_projects(
    projects: &mut std::collections::HashMap<i64, Option<String>>,
    matcher: &Matcher,
) {
    for p in projects.values_mut() {
        *p = p
            .as_deref()
            .and_then(|name| matcher.resolve(name))
            .map(str::to_owned);
    }
}

impl Matcher {
    /// The project a span files into, by the first rule that matches:
    /// a path the title shows under an instance path, a place or branch
    /// anchor naming an instance folder, an item key with a listed ticket
    /// prefix (or a branch carrying one, or `org/repo#n` of a remote), a
    /// domain anchor under a listed site, a title regex, the app.
    pub fn file(&self, app: &str, title: &str, anchors: &[Anchor]) -> Option<&str> {
        if self.projects.is_empty() {
            return None;
        }
        if let Some(p) = extract::title_path(title) {
            let path = expand_home(p);
            // Longest instance path wins so a worktree under the main repo
            // files where the deeper rule says.
            let mut best: Option<(usize, &Project)> = None;
            for proj in &self.projects {
                for inst in &proj.paths {
                    if path.starts_with(inst) {
                        let n = inst.components().count();
                        if best.is_none_or(|(m, _)| n > m) {
                            best = Some((n, proj));
                        }
                    }
                }
            }
            if let Some((_, proj)) = best {
                return Some(&proj.name);
            }
        }
        let places = anchors.iter().filter_map(|a| match a.kind {
            AnchorKind::Place => Some(a.value.to_ascii_lowercase()),
            AnchorKind::Branch => a
                .value
                .split_once('@')
                .map(|(repo, _)| repo.to_ascii_lowercase()),
            _ => None,
        });
        for place in places {
            if let Some(p) = self.projects.iter().find(|p| p.places.contains(&place)) {
                return Some(&p.name);
            }
        }
        for a in anchors {
            match a.kind {
                AnchorKind::Item | AnchorKind::Change => {
                    if let Some(p) = self.by_key(&a.value) {
                        return Some(&p.name);
                    }
                }
                AnchorKind::Branch => {
                    let branch = a.value.split_once('@').map_or(a.value.as_str(), |(_, b)| b);
                    if let Some(p) = self.by_branch(branch) {
                        return Some(&p.name);
                    }
                }
                _ => {}
            }
        }
        for a in anchors.iter().filter(|a| a.kind == AnchorKind::Domain) {
            let host = a.value.to_ascii_lowercase();
            if let Some(p) = self.projects.iter().find(|p| {
                p.domains.iter().any(|d| {
                    host == *d
                        || host
                            .strip_suffix(d.as_str())
                            .is_some_and(|r| r.ends_with('.'))
                })
            }) {
                return Some(&p.name);
            }
        }
        for a in anchors.iter().filter(|a| a.kind == AnchorKind::Link) {
            if let Some(p) = self.projects.iter().find(|p| {
                p.links
                    .iter()
                    .any(|pat| extract::link_matches(pat, &a.value))
            }) {
                return Some(&p.name);
            }
        }
        if let Some(p) = self
            .projects
            .iter()
            .find(|p| p.titles.iter().any(|re| re.is_match(title)))
        {
            return Some(&p.name);
        }
        let app_l = app.to_ascii_lowercase();
        self.projects
            .iter()
            .find(|p| p.apps.contains(&app_l))
            .map(|p| p.name.as_str())
    }

    /// `ACME-123` by prefix; `org/repo#123` by the remote slug or folder.
    fn by_key(&self, key: &str) -> Option<&Project> {
        if let Some((repo, n)) = key.split_once('#')
            && n.chars().all(|c| c.is_ascii_digit())
        {
            let repo = repo.to_ascii_lowercase();
            let folder = repo.rsplit('/').next().unwrap_or(&repo).to_owned();
            return self
                .projects
                .iter()
                .find(|p| p.slugs.contains(&repo) || p.places.contains(&folder));
        }
        let prefix = key.split_once('-')?.0.to_ascii_uppercase();
        self.projects.iter().find(|p| p.tickets.contains(&prefix))
    }

    /// A branch name carrying `<prefix>-<n>` in any case.
    fn by_branch(&self, branch: &str) -> Option<&Project> {
        let upper = branch.to_ascii_uppercase();
        self.projects.iter().find(|p| {
            p.tickets.iter().any(|t| {
                upper.match_indices(t.as_str()).any(|(i, _)| {
                    let rest = &upper[i + t.len()..];
                    let before_ok = i == 0
                        || !upper[..i]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c.is_ascii_alphanumeric());
                    before_ok
                        && rest
                            .strip_prefix('-')
                            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
                })
            })
        })
    }
}

/// Apply the join rule over one ordered run of focus spans: an unfiled span
/// shorter than `join_ms` between two spans of one project joins it.
/// `spans` is `(start_ms, end_ms, project)` in time order.
pub fn join_short(spans: &mut [(i64, i64, Option<String>)], join_ms: i64) {
    if join_ms <= 0 {
        return;
    }
    for i in 1..spans.len().saturating_sub(1) {
        if spans[i].2.is_some() || spans[i].1 - spans[i].0 >= join_ms {
            continue;
        }
        if let (Some(prev), Some(next)) = (&spans[i - 1].2, &spans[i + 1].2)
            && prev == next
        {
            spans[i].2 = Some(prev.clone());
        }
    }
}

// ------------------------------------------------------------ git reading

/// The repo's git dir: `.git` itself, or the directory a `.git` file's
/// `gitdir:` line names (worktrees, submodules).
fn git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let rel = text.strip_prefix("gitdir:")?.trim();
    let dir = repo.join(rel);
    dir.is_dir().then_some(dir)
}

/// The git dir every worktree of a repo shares: the repo's own for a main
/// checkout, the `commondir` target for a linked worktree.
fn common_dir(repo: &Path) -> Option<PathBuf> {
    let dir = git_dir(repo)?;
    match std::fs::read_to_string(dir.join("commondir")) {
        Ok(rel) => {
            let common = dir.join(rel.trim());
            Some(std::fs::canonicalize(&common).unwrap_or(common))
        }
        Err(_) => Some(dir),
    }
}

/// Every checkout of the repo at `repo` other than itself: the main
/// worktree (the common dir's parent) and each `worktrees/<n>/gitdir`
/// target that still exists.
pub fn worktrees_of(repo: &Path) -> Vec<PathBuf> {
    let Some(common) = common_dir(repo) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(main) = common.parent()
        && main != repo
        && main.is_dir()
    {
        out.push(main.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        for e in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path().join("gitdir")) else {
                continue;
            };
            let Some(wt) = Path::new(text.trim()).parent() else {
                continue;
            };
            if wt != repo && wt.is_dir() && !out.iter().any(|p| p == wt) {
                out.push(wt.to_path_buf());
            }
        }
    }
    out
}

/// The `origin` remote (else the first remote) of the repo at `repo` as
/// `host/org/repo`, read from the shared git config.
pub fn remote_of(repo: &Path) -> Option<String> {
    let common = common_dir(repo)?;
    let text = std::fs::read_to_string(common.join("config")).ok()?;
    let mut section: Option<String> = None;
    let mut origin = None;
    let mut first = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("[remote \"") {
            section = rest.split('"').next().map(str::to_owned);
        } else if line.starts_with('[') {
            section = None;
        } else if let Some(name) = &section
            && let Some(url) = line.strip_prefix("url")
            && let Some(url) = url.trim_start().strip_prefix('=')
        {
            let id = remote_id(url.trim());
            if name == "origin" {
                origin = origin.or_else(|| id.clone());
            }
            first = first.or(id);
        }
    }
    origin.or(first)
}

/// `git@github.com:org/repo.git`, `https://github.com/org/repo`,
/// `ssh://git@host/org/repo.git` → `github.com/org/repo`.
pub fn remote_id(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let rest = if let Some((_, r)) = url.split_once("://") {
        r
    } else if let Some((userhost, path)) = url.split_once(':')
        && !userhost.contains('/')
    {
        // scp-like: user@host:path
        return finish(userhost.rsplit('@').next()?, path);
    } else {
        return None;
    };
    let (hostport, path) = rest.split_once('/')?;
    let host = hostport.rsplit('@').next()?;
    let host = host.split(':').next()?;
    finish(host, path)
}

fn finish(host: &str, path: &str) -> Option<String> {
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!(
        "{}/{}",
        host.to_ascii_lowercase(),
        path.to_ascii_lowercase()
    ))
}

/// The `host/path` globs the link files under `paths` name.
fn link_patterns(paths: &[PathBuf]) -> Vec<String> {
    let mut out: Vec<String> = paths
        .iter()
        .flat_map(|p| crate::links::read_links(p))
        .filter_map(|l| l.url_pattern)
        .collect();
    out.sort();
    out.dedup();
    out
}

// ---- Repo discovery (m37 chunk 2): the parents of the configured repos,
// scanned one level deep for git repos no project claims. Pure filesystem
// reads; nothing here writes or shells out to git.

/// One repo found under a scanned parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRepo {
    pub path: PathBuf,
    /// Folder basename, lowercased like `config::projects_effective`.
    pub name: String,
    /// `host/org/repo`, via `project::remote_of`.
    pub remote: Option<String>,
}

/// Immediate children of each of `parents` that are git repos, excluding
/// `known` paths and their worktrees. A hidden child (name starting with
/// `.`) is skipped, as is a parent that does not exist. Sorted by name.
pub fn discover_repos(parents: &[PathBuf], known: &[PathBuf]) -> Vec<DiscoveredRepo> {
    let excluded = excluded_paths(known);
    let mut out = Vec::new();
    for parent in parents {
        let Ok(entries) = std::fs::read_dir(parent) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if name.starts_with('.') || !is_git_repo(&path) {
                continue;
            }
            let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if excluded.contains(&canon) {
                continue;
            }
            out.push(DiscoveredRepo {
                remote: remote_of(&path),
                name: name.to_ascii_lowercase(),
                path,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Unique parents of `repos`, first-seen order.
pub fn parents_of(repos: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for repo in repos {
        if let Some(parent) = repo.parent()
            && seen.insert(parent.to_path_buf())
        {
            out.push(parent.to_path_buf());
        }
    }
    out
}

fn is_git_repo(path: &Path) -> bool {
    let dot = path.join(".git");
    dot.is_dir() || dot.is_file()
}

/// `known` paths plus their worktrees, canonicalised so a discovered path
/// reached through a symlinked parent still matches.
fn excluded_paths(known: &[PathBuf]) -> HashSet<PathBuf> {
    let mut set = HashSet::new();
    for k in known {
        set.insert(std::fs::canonicalize(k).unwrap_or_else(|_| k.clone()));
        for wt in worktrees_of(k) {
            set.insert(std::fs::canonicalize(&wt).unwrap_or(wt));
        }
    }
    set
}

#[cfg(test)]
mod discover_tests {
    #[test]
    fn matcher_discovers_sibling_repos_once() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("dev");
        for name in ["known", "found"] {
            std::fs::create_dir_all(parent.join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(parent.join("plain")).unwrap();
        let cfg = crate::config::ProjectCfg {
            name: "known".into(),
            repos: vec![parent.join("known").display().to_string()],
            ..Default::default()
        };
        let mut m = super::Matcher::new(&[cfg]);
        m.discover();
        m.discover();
        let names: Vec<&str> = m.projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["known", "found"]);
        assert!(m.projects[1].discovered && m.projects[1].derive);
        assert_eq!(m.resolve("found"), Some("found"));
    }

    use super::*;

    fn make_repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn discovers_unknown_repos_only() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        let repo_a = parent.join("repo-a");
        let repo_b = parent.join("Repo-B");
        let plain = parent.join("plain-dir");
        make_repo(&repo_a);
        make_repo(&repo_b);
        std::fs::create_dir_all(&plain).unwrap();

        let known = [repo_a.clone()];
        let found = discover_repos(&[parent.to_path_buf()], &known);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, repo_b);
        assert_eq!(found[0].name, "repo-b");
    }

    #[test]
    fn skips_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        make_repo(&tmp.path().join(".hidden-repo"));
        assert!(discover_repos(&[tmp.path().to_path_buf()], &[]).is_empty());
    }

    #[test]
    fn missing_parent_is_skipped_silently() {
        let found = discover_repos(&[PathBuf::from("/no/such/parent-xyz")], &[]);
        assert!(found.is_empty());
    }

    #[test]
    fn parents_of_repos_unique_and_ordered() {
        let repos = vec![
            PathBuf::from("/a/one"),
            PathBuf::from("/a/two"),
            PathBuf::from("/b/three"),
        ];
        assert_eq!(
            parents_of(&repos),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(name: &str) -> ProjectCfg {
        ProjectCfg {
            name: name.into(),
            ..ProjectCfg::default()
        }
    }

    fn anchor(kind: AnchorKind, value: &str) -> Anchor {
        Anchor {
            kind,
            value: value.into(),
        }
    }

    // Paths need not exist to match; only worktree and remote reads touch
    // the filesystem. Home-relative because that is what a title shows.
    fn matcher() -> Matcher {
        let mut acme = cfg("acme");
        acme.repos = vec!["~/dev/contoso".into()];
        acme.tickets = vec!["acme".into()];
        acme.domains = vec!["contoso.atlassian.net".into()];
        let mut chronicle = cfg("chronicle");
        chronicle.repos = vec!["~/dev/chronicle".into()];
        chronicle.titles = vec!["(?i)chronicled\\.dev".into()];
        chronicle.apps = vec!["Chronicle".into()];
        Matcher::new(&[acme, chronicle])
    }

    #[test]
    fn files_by_place_anchor() {
        let m = matcher();
        assert_eq!(
            m.file("Alacritty", "zsh", &[anchor(AnchorKind::Place, "CONTOSO")]),
            Some("acme")
        );
        assert_eq!(
            m.file(
                "Alacritty",
                "zsh",
                &[anchor(AnchorKind::Branch, "chronicle@main")]
            ),
            Some("chronicle")
        );
    }

    #[test]
    fn files_by_title_path_longest_instance() {
        if std::env::var_os("HOME").is_none() {
            return;
        }
        let m = matcher();
        let title = "~/dev/contoso/src/models.py \u{2014} nvim";
        assert_eq!(m.file("Alacritty", title, &[]), Some("acme"));
        assert_eq!(
            m.file("Alacritty", "~/dev/contoso-old/x \u{2014} nvim", &[]),
            None
        );
    }

    #[test]
    fn files_by_ticket_prefix_and_branch_any_case() {
        let m = matcher();
        assert_eq!(
            m.file(
                "firefox",
                "board",
                &[anchor(AnchorKind::Item, "acme-11032")]
            ),
            Some("acme")
        );
        assert_eq!(
            m.file(
                "Alacritty",
                "zsh",
                &[anchor(AnchorKind::Branch, "other@feat/AcMe-12")]
            ),
            Some("acme")
        );
        assert_eq!(
            m.file(
                "Alacritty",
                "zsh",
                &[anchor(AnchorKind::Branch, "other@xacme-12")]
            ),
            None
        );
    }

    #[test]
    fn files_by_domain_title_and_app_in_that_order() {
        let m = matcher();
        assert_eq!(
            m.file(
                "firefox",
                "x",
                &[anchor(AnchorKind::Domain, "contoso.atlassian.net")]
            ),
            Some("acme")
        );
        assert_eq!(
            m.file(
                "firefox",
                "x",
                &[anchor(AnchorKind::Domain, "sub.contoso.atlassian.net")]
            ),
            Some("acme")
        );
        assert_eq!(
            m.file(
                "firefox",
                "x",
                &[anchor(AnchorKind::Domain, "notcontoso.atlassian.net")]
            ),
            None
        );
        assert_eq!(
            m.file("firefox", "chronicled.dev \u{2014} Firefox", &[]),
            Some("chronicle")
        );
        assert_eq!(m.file("chronicle", "Home", &[]), Some("chronicle"));
        assert_eq!(m.file("firefox", "news", &[]), None);
    }

    #[test]
    fn place_beats_ticket_when_both_match() {
        let m = matcher();
        let anchors = [
            anchor(AnchorKind::Item, "ACME-1"),
            anchor(AnchorKind::Place, "chronicle"),
        ];
        assert_eq!(m.file("Alacritty", "zsh", &anchors), Some("chronicle"));
    }

    #[test]
    fn join_short_fills_a_glance_between_one_project() {
        let mut spans = vec![
            (0, 60_000, Some("a".to_owned())),
            (60_000, 90_000, None),
            (90_000, 150_000, Some("a".to_owned())),
            (150_000, 400_000, None),
            (400_000, 500_000, Some("a".to_owned())),
            (500_000, 510_000, None),
            (510_000, 600_000, Some("b".to_owned())),
        ];
        join_short(&mut spans, 120_000);
        assert_eq!(spans[1].2.as_deref(), Some("a"));
        assert_eq!(spans[3].2, None, "too long to be a glance");
        assert_eq!(spans[5].2, None, "different projects either side");
    }

    #[test]
    fn remote_ids() {
        assert_eq!(
            remote_id("git@github.com:northwind/contoso.git").as_deref(),
            Some("github.com/northwind/contoso")
        );
        assert_eq!(
            remote_id("https://github.com/James-Clarke/chronicle").as_deref(),
            Some("github.com/james-clarke/chronicle")
        );
        assert_eq!(
            remote_id("ssh://git@gitlab.example.com:2222/org/repo.git").as_deref(),
            Some("gitlab.example.com/org/repo")
        );
        assert_eq!(remote_id("/srv/git/repo.git"), None);
    }

    // m35 chunk 1: a task's project resolves to a configured name by name
    // in any case, or by a repo folder the project has; derive defaults on
    // for a project no rule knows.
    #[test]
    fn resolve_by_name_or_instance_folder() {
        let m = Matcher::new(&[
            ProjectCfg {
                name: "acme".into(),
                repos: vec!["/no/such/contoso".into(), "/no/such/mailer".into()],
                derive: false,
                ..ProjectCfg::default()
            },
            cfg("chronicle"),
        ]);
        assert_eq!(m.resolve("Contoso"), Some("acme"));
        assert_eq!(m.resolve("CONTOSO"), Some("acme"));
        assert_eq!(m.resolve("mailer"), Some("acme"));
        assert_eq!(m.resolve("chronicle"), Some("chronicle"));
        assert_eq!(m.resolve("sprog"), None);
        assert_eq!(m.resolve("  "), None);
        assert!(!m.derives("acme"));
        assert!(m.derives("chronicle"));
        assert!(m.derives("sprog"));
        let mut projects = std::collections::HashMap::from([
            (1, Some("contoso".to_owned())),
            (2, Some("sprog".to_owned())),
            (3, None),
        ]);
        normalize_projects(&mut projects, &m);
        assert_eq!(projects[&1].as_deref(), Some("acme"));
        assert_eq!(projects[&2], None);
        assert_eq!(projects[&3], None);
    }
}
