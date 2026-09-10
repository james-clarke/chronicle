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
        println!("no projects: add [[projects]] to config.toml or git_repos for the default");
        return Ok(());
    }
    if config.projects.is_empty() {
        println!("(defaults: one project per git_repos entry; none written to config.toml)");
    }
    let matcher = Matcher::from_config(&config);
    for p in &matcher.projects {
        let remotes: Vec<String> = p
            .paths
            .iter()
            .filter_map(|r| project::remote_of(r))
            .collect();
        println!(
            "{}{}{}  {}",
            p.name,
            if p.derive { "" } else { "  (derive off)" },
            if p.discovered { "  (discovered)" } else { "" },
            if remotes.is_empty() {
                "no remote".to_owned()
            } else {
                remotes.join(", ")
            }
        );
        for path in &p.paths {
            println!("    {}", path.display());
        }
        let rule = |label: &str, items: &[String]| {
            if !items.is_empty() {
                println!("    {label}: {}", items.join(", "));
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
    let report = Report::build(&config, &spans, lo, hi, top);
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
    Ok(())
}

/// What `project test` prints; the Settings card renders the same lines.
#[derive(Debug, Clone, Default)]
pub(crate) struct Report {
    pub focus_ms: i64,
    /// Per project in config order, minutes filed.
    pub projects: Vec<(String, i64)>,
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
        lo: i64,
        hi: i64,
        top: usize,
    ) -> Self {
        let matcher = Matcher::from_config(config);
        let filed = storage::filed_spans(spans, &matcher, config.project_join_min);
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
            .projects
            .iter()
            .map(|p| {
                (
                    p.name.clone(),
                    per.get(p.name.as_str()).copied().unwrap_or(0),
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
        for (name, ms) in &self.projects {
            let _ = writeln!(out, "{:>7.1} min  {name}", min(*ms));
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
