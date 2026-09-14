//! `chronicle project list | test | rebuild` (m35 chunk 0): the projects in
//! force, how the last N days file under the current config, and a re-file
//! of stored spans after a config edit.

use std::collections::BTreeMap;
use std::path::Path;

use chronicle_core::config::Config;
use chronicle_core::extract::AnchorKind;
use chronicle_core::profile::AnchoredSpan;
use chronicle_core::project::{self, Matcher};
use chronicle_core::storage;
use jiff::Timestamp;

pub(crate) fn list(data_dir: &Path) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    let cfgs = config.projects_effective();
    if cfgs.is_empty() {
        println!(
            "no projects: add [[projects]] to config.toml, or dev_roots / git_repos for the default"
        );
        return Ok(());
    }
    if config.projects.is_empty() {
        println!(
            "(defaults: one project per watched repo, from dev_roots and git_repos; none written to config.toml)"
        );
    }
    let matcher = Matcher::from_config(&config);
    for (depth, p) in matcher.tree() {
        let indent = "    ".repeat(depth);
        let remotes: Vec<String> = p
            .paths
            .iter()
            .filter_map(|r| project::remote_of(r))
            .collect();
        // A project with children never mints, so it wears `(parent)`
        // where `(derive off)` would go (m44 chunk 0).
        let flag = if !p.children.is_empty() {
            "  (parent)"
        } else if !p.derive {
            "  (derive off)"
        } else {
            ""
        };
        println!(
            "{indent}{}{flag}{}  {}",
            p.name,
            if p.discovered { "  (discovered)" } else { "" },
            if remotes.is_empty() {
                "no remote".to_owned()
            } else {
                remotes.join(", ")
            }
        );
        for path in &p.paths {
            println!("{indent}    {}", path.display());
        }
        let rule = |label: &str, items: &[String]| {
            if !items.is_empty() {
                println!("{indent}    {label}: {}", items.join(", "));
            }
        };
        rule("tickets", &p.tickets);
        rule("domains", &p.domains);
        let titles: Vec<String> = p.titles.iter().map(|t| t.as_str().to_owned()).collect();
        rule("titles", &titles);
        rule("apps", &p.apps);
        rule("links", &p.links);
    }
    Ok(())
}

/// Minutes per project and the top unfiled titles and places over the
/// last `days`, matched fresh from config (the stored column is not read),
/// so a config edit can be tried before `rebuild`.
pub(crate) fn test(data_dir: &Path, days: u32, top: usize) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let hi = Timestamp::now().as_millisecond();
    let lo = hi - i64::from(days) * 86_400_000;
    let spans = storage::anchored_spans(&conn, lo, hi)?;
    let scopes = storage::task_scopes(&conn, Some(lo))?;
    let report = Report::build(&config, &spans, &scopes, lo, hi, top);
    print!("{}", report.render(days));
    Ok(())
}

pub(crate) fn rebuild(data_dir: &Path, days: Option<u32>) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let hi = i64::MAX;
    let lo = days.map_or(0, |d| {
        Timestamp::now().as_millisecond() - i64::from(d) * 86_400_000
    });
    let matcher = Matcher::from_config(&config);
    // A hand edit applies to the daemon from here on, too.
    let _ = crate::send_ctrl(&crate::socket_path(data_dir), "reload");
    let n = storage::file_spans(&mut conn, lo, hi, &matcher, config.project_join_min)?;
    println!(
        "filed {n} focus spans{}",
        days.map_or(String::new(), |d| format!(" over the last {d} days"))
    );
    // Tasks live inside configured projects (m35 chunk 1): a project named
    // by a repo folder from before the config takes the project's name;
    // one no rule knows is listed for the person to map (`task rename
    // --project`) or leave, and counts as unfiled until then.
    let (changed, unknown) = storage::normalize_task_projects(&mut conn, &matcher)?;
    if !changed.is_empty() {
        println!("renamed the project on {} tasks:", changed.len());
        for (id, from, to) in &changed {
            println!("  {id:>5}  {from} \u{2192} {to}");
        }
    }
    if !unknown.is_empty() {
        println!(
            "{} open tasks in a project no rule knows (unfiled until mapped):",
            unknown.len()
        );
        for (id, label, project) in &unknown {
            println!("  {id:>5}  {project:<16}  {label}");
        }
    }
    // Derived tasks nobody confirmed, renamed or placed time on become
    // proposals (m44 chunk 2): their time sits on the project's other
    // work until the card is confirmed.
    let converted =
        storage::derived_tasks_to_proposals(&mut conn, Timestamp::now().as_millisecond())?;
    if !converted.is_empty() {
        println!(
            "{} derived tasks became proposals to review:",
            converted.len()
        );
        for (id, label) in &converted {
            println!("  {id:>5}  {label}");
        }
    }
    Ok(())
}

/// Add rules to a project through the one config writer, tell the daemon,
/// re-file the last `days` and print what moved (m44 chunk 3). The same
/// path the UI menus and the repo card take.
pub(crate) fn attach(
    data_dir: &Path,
    name: &str,
    rules: &[chronicle_core::config::ProjectRule],
    days: u32,
) -> anyhow::Result<()> {
    if rules.is_empty() {
        anyhow::bail!(
            "nothing to attach: give --repo, --ticket, --domain, --title, --app or --parent"
        );
    }
    let outcome = attach_and_refile(data_dir, name, rules, days)?;
    print!("{}", outcome.render());
    Ok(())
}

/// What an attach did, for the CLI and the UI status line alike.
#[derive(Debug, Clone, Default)]
pub(crate) struct AttachOutcome {
    pub project: String,
    pub rules: Vec<String>,
    pub days: u32,
    pub moved: Vec<storage::Moved>,
    pub unchanged: bool,
}

impl AttachOutcome {
    pub(crate) fn render(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        if self.unchanged {
            let _ = writeln!(out, "{}: every rule was already there", self.project);
            return out;
        }
        let _ = writeln!(out, "{}: {}", self.project, self.rules.join(", "));
        if self.moved.is_empty() {
            let _ = writeln!(out, "nothing moved over the last {} days", self.days);
            return out;
        }
        let _ = writeln!(out, "over the last {} days:", self.days);
        let min = |ms: i64| ms as f64 / 60_000.0;
        for m in &self.moved {
            let _ = writeln!(
                out,
                "{:>7.1} \u{2192} {:<7.1} min  {}",
                min(m.before_ms),
                min(m.after_ms),
                m.project.as_deref().unwrap_or("(unfiled)")
            );
        }
        out
    }
}

/// Write the rules, poke the daemon so it files with them from now on,
/// re-file the last `days`. Returns what moved.
pub(crate) fn attach_and_refile(
    data_dir: &Path,
    name: &str,
    rules: &[chronicle_core::config::ProjectRule],
    days: u32,
) -> anyhow::Result<AttachOutcome> {
    use chronicle_core::config::{attach_rules, write_projects};
    let path = data_dir.join("config.toml");
    let config = Config::load(&path)?;
    let (projects, changed) = attach_rules(&config, name, rules);
    let project = projects
        .iter()
        .find(|p| p.name.eq_ignore_ascii_case(name.trim()))
        .map_or_else(|| name.trim().to_owned(), |p| p.name.clone());
    let described: Vec<String> = rules
        .iter()
        .map(|r| match r {
            chronicle_core::config::ProjectRule::Parent(None) => "top level".to_owned(),
            r => format!("{} {}", r.kind(), r.value()),
        })
        .collect();
    if !changed {
        return Ok(AttachOutcome {
            project,
            rules: described,
            days,
            moved: Vec::new(),
            unchanged: true,
        });
    }
    let config = write_projects(&path, projects, None)?;
    let _ = crate::send_ctrl(&crate::socket_path(data_dir), "reload");
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let lo = Timestamp::now().as_millisecond() - i64::from(days) * 86_400_000;
    let matcher = Matcher::from_config(&config);
    let moved =
        storage::file_spans_report(&mut conn, lo, i64::MAX, &matcher, config.project_join_min)?;
    Ok(AttachOutcome {
        project,
        rules: described,
        days,
        moved,
        unchanged: false,
    })
}

/// `name`'s own filed ms plus every descendant's (m44 chunk 0): a parent's
/// total covers the shared furniture it files directly and the work its
/// children do.
fn descendant_ms(matcher: &Matcher, per: &BTreeMap<&str, i64>, name: &str) -> i64 {
    std::iter::once(name)
        .chain(matcher.descendants(name))
        .map(|n| per.get(n).copied().unwrap_or(0))
        .sum()
}

/// What `project test` prints; the Settings card renders the same lines.
#[derive(Debug, Clone, Default)]
pub(crate) struct Report {
    pub focus_ms: i64,
    /// Per project in tree order: name, ms (own filed ms plus every
    /// descendant's), depth.
    pub projects: Vec<(String, i64, usize)>,
    pub unfiled_ms: i64,
    /// `(app, title, ms)` of the unfiled spans, most time first.
    pub titles: Vec<(String, String, i64)>,
    /// `(place, ms)` place anchors on unfiled spans — repos no project
    /// claims, the discovery list.
    pub places: Vec<(String, i64)>,
    /// `(domain, ms)` domain anchors on unfiled spans.
    pub domains: Vec<(String, i64)>,
}

impl Report {
    pub(crate) fn build(
        config: &Config,
        spans: &[AnchoredSpan],
        scopes: &[chronicle_core::scope::TaskScope],
        lo: i64,
        hi: i64,
        top: usize,
    ) -> Self {
        let matcher = Matcher::from_config(config);
        let filed = storage::filed_spans(spans, &matcher, config.project_join_min, scopes);
        let mut per: BTreeMap<&str, i64> = BTreeMap::new();
        let mut titles: BTreeMap<(String, String), i64> = BTreeMap::new();
        let mut places: BTreeMap<String, i64> = BTreeMap::new();
        let mut domains: BTreeMap<String, i64> = BTreeMap::new();
        let mut focus_ms = 0;
        let mut unfiled_ms = 0;
        for (s, project) in spans.iter().zip(&filed) {
            let ms = (s.end_ts.min(hi) - s.start_ts.max(lo)).max(0);
            focus_ms += ms;
            match project {
                Some(p) => *per.entry(p.as_str()).or_default() += ms,
                None => {
                    unfiled_ms += ms;
                    *titles.entry((s.app.clone(), s.title.clone())).or_default() += ms;
                    for a in &s.anchors {
                        match a.kind {
                            AnchorKind::Place => *places.entry(a.value.clone()).or_default() += ms,
                            AnchorKind::Domain => {
                                *domains.entry(a.value.clone()).or_default() += ms;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        let projects = matcher
            .tree()
            .into_iter()
            .map(|(depth, p)| {
                (
                    p.name.clone(),
                    descendant_ms(&matcher, &per, &p.name),
                    depth,
                )
            })
            .collect();
        let mut titles: Vec<(String, String, i64)> =
            titles.into_iter().map(|((a, t), ms)| (a, t, ms)).collect();
        titles.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        titles.truncate(top);
        let top_of = |m: BTreeMap<String, i64>| {
            let mut v: Vec<(String, i64)> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            v.truncate(top);
            v
        };
        Self {
            focus_ms,
            projects,
            unfiled_ms,
            titles,
            places: top_of(places),
            domains: top_of(domains),
        }
    }

    pub(crate) fn filed_pct(&self) -> f64 {
        if self.focus_ms == 0 {
            0.0
        } else {
            100.0 * (self.focus_ms - self.unfiled_ms) as f64 / self.focus_ms as f64
        }
    }

    pub(crate) fn render(&self, days: u32) -> String {
        use std::fmt::Write;
        let min = |ms: i64| ms as f64 / 60_000.0;
        let mut out = String::new();
        let _ = writeln!(
            out,
            "focus {:>7.1} min over {days} days, {:.1}% filed",
            min(self.focus_ms),
            self.filed_pct()
        );
        for (name, ms, depth) in &self.projects {
            let indent = "  ".repeat(*depth);
            let _ = writeln!(out, "{:>7.1} min  {indent}{name}", min(*ms));
        }
        let _ = writeln!(out, "{:>7.1} min  (unfiled)", min(self.unfiled_ms));
        if !self.places.is_empty() {
            let _ = writeln!(out, "unfiled places:");
            for (place, ms) in &self.places {
                let _ = writeln!(out, "{:>7.1} min  {place}", min(*ms));
            }
        }
        if !self.domains.is_empty() {
            let _ = writeln!(out, "unfiled domains:");
            for (domain, ms) in &self.domains {
                let _ = writeln!(out, "{:>7.1} min  {domain}", min(*ms));
            }
        }
        if !self.titles.is_empty() {
            let _ = writeln!(out, "unfiled titles:");
            for (app, title, ms) in &self.titles {
                let title: String = title.chars().take(90).collect();
                let _ = writeln!(out, "{:>7.1} min  {app:<12} {title}", min(*ms));
            }
        }
        out
    }
}
