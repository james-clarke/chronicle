//! Settings › Projects (m35 chunk 0): one row per project with its rules as
//! editable lists, and "test the last 7 days" inline — minutes per project,
//! the unfiled share and the top unfiled titles, matched from the rows as
//! edited, so the rules are tuned before Save writes them.

use chronicle_core::config::{Config, ProjectCfg};
use eframe::egui;
use jiff::Timestamp;

use super::theme::{self, palette};

const TEST_DAYS: u32 = 7;
const TEST_TOP: usize = 12;

/// One project as edited: lists are comma-separated text until save.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name: String,
    parent: String,
    repos: String,
    tickets: String,
    domains: String,
    titles: String,
    apps: String,
    derive: bool,
}

impl Row {
    fn from_cfg(c: &ProjectCfg) -> Self {
        Self {
            name: c.name.clone(),
            parent: c.parent.clone().unwrap_or_default(),
            repos: c.repos.join(", "),
            tickets: c.tickets.join(", "),
            domains: c.domains.join(", "),
            titles: c.titles.join(", "),
            apps: c.apps.join(", "),
            derive: c.derive,
        }
    }

    fn to_cfg(&self) -> ProjectCfg {
        let list = |s: &str| -> Vec<String> {
            s.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let parent = self.parent.trim();
        ProjectCfg {
            name: self.name.trim().to_owned(),
            parent: (!parent.is_empty()).then(|| parent.to_owned()),
            repos: list(&self.repos),
            tickets: list(&self.tickets),
            domains: list(&self.domains),
            titles: list(&self.titles),
            apps: list(&self.apps),
            derive: self.derive,
        }
    }
}

pub(super) struct ProjectsPanel {
    rows: Vec<Row>,
    join_min: u32,
    /// True when config.toml had no `[[projects]]` and the rows are the
    /// per-repo defaults; saving them unchanged keeps the file as it was.
    defaulted: bool,
    /// Index armed for removal (second click deletes).
    arm_remove: Option<usize>,
    result: Option<String>,
}

impl ProjectsPanel {
    pub(super) fn from_config(config: &Config) -> Self {
        Self {
            rows: config
                .projects_effective()
                .iter()
                .map(Row::from_cfg)
                .collect(),
            join_min: config.project_join_min,
            defaulted: config.projects.is_empty(),
            arm_remove: None,
            result: None,
        }
    }

    fn cfgs(&self) -> Vec<ProjectCfg> {
        self.rows
            .iter()
            .map(Row::to_cfg)
            .filter(|c| !c.name.is_empty())
            .collect()
    }

    /// Errors name the row and the regex that failed to compile.
    pub(super) fn apply(&self, config: &mut Config) -> Result<(), String> {
        let cfgs = self.cfgs();
        for c in &cfgs {
            for t in &c.titles {
                regex::Regex::new(t)
                    .map_err(|e| format!("project {}: title regex {t}: {e}", c.name))?;
            }
        }
        chronicle_core::config::validate_projects(&cfgs)?;
        config.project_join_min = self.join_min;
        let defaults = config.projects_effective();
        config.projects = if self.defaulted && cfgs == defaults {
            Vec::new()
        } else {
            cfgs
        };
        Ok(())
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui, conn: Option<&rusqlite::Connection>) {
        ui.weak(
            "every focus span files into the first project a rule matches: repo path or \
             folder, ticket prefix, site, title regex, app; lists are comma-separated, rules \
             on a project with a parent run before the parent's",
        );
        let width = ui.available_width();
        let mut remove: Option<usize> = None;
        let mut arm: Option<usize> = None;
        for (i, row) in self.rows.iter_mut().enumerate() {
            ui.add_space(theme::SPACE_SM);
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut row.name)
                        .desired_width(160.0)
                        .hint_text("name"),
                );
                ui.checkbox(&mut row.derive, "derive");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let armed = self.arm_remove == Some(i);
                    let label = if armed { "delete?" } else { "\u{d7}" };
                    if theme::ghost_button(ui, label).clicked() {
                        if armed {
                            remove = Some(i);
                        } else {
                            arm = Some(i);
                        }
                    }
                });
            });
            egui::Grid::new(("settings_project", i))
                .num_columns(2)
                .striped(false)
                .min_col_width(52.0)
                .show(ui, |ui| {
                    let field = |ui: &mut egui::Ui, label: &str, s: &mut String, hint: &str| {
                        ui.weak(label);
                        ui.add(
                            egui::TextEdit::singleline(s)
                                .desired_width(width - 72.0)
                                .font(egui::TextStyle::Monospace)
                                .hint_text(hint),
                        );
                        ui.end_row();
                    };
                    field(ui, "parent", &mut row.parent, "client or umbrella project");
                    field(
                        ui,
                        "repos",
                        &mut row.repos,
                        "~/dev/project (worktrees count)",
                    );
                    field(ui, "tickets", &mut row.tickets, "ACME");
                    field(ui, "sites", &mut row.domains, "example.atlassian.net");
                    field(ui, "titles", &mut row.titles, "(?i)regex");
                    field(ui, "apps", &mut row.apps, "slack");
                });
        }
        if let Some(i) = arm {
            self.arm_remove = Some(i);
        }
        if let Some(i) = remove {
            self.rows.remove(i);
            self.arm_remove = None;
        }
        ui.add_space(theme::SPACE_SM);
        ui.horizontal(|ui| {
            if theme::secondary_button(ui, "add project").clicked() {
                self.rows.push(Row::from_cfg(&ProjectCfg::default()));
            }
            ui.label("join glances under");
            ui.add(egui::DragValue::new(&mut self.join_min).range(0..=30));
            ui.weak("min");
            if conn.is_some()
                && theme::secondary_button(ui, &format!("test last {TEST_DAYS} days")).clicked()
            {
                self.result = Some(self.run_test(conn));
            }
        });
        if let Some(text) = &self.result {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .font(egui::TextStyle::Monospace.resolve(ui.style()))
                        .color(palette::TEXT_DIM),
                )
                .wrap(),
            );
        }
    }

    fn run_test(&self, conn: Option<&rusqlite::Connection>) -> String {
        let Some(conn) = conn else {
            return "no database".to_owned();
        };
        let config = Config {
            projects: self.cfgs(),
            project_join_min: self.join_min,
            ..Config::default()
        };
        let hi = Timestamp::now().as_millisecond();
        let lo = hi - i64::from(TEST_DAYS) * 86_400_000;
        let scopes = chronicle_core::storage::task_scopes(conn, Some(lo)).unwrap_or_default();
        match chronicle_core::storage::anchored_spans(conn, lo, hi) {
            Ok(spans) => crate::project::Report::build(&config, &spans, &scopes, lo, hi, TEST_TOP)
                .render(TEST_DAYS),
            Err(e) => format!("test failed: {e}"),
        }
    }
}
