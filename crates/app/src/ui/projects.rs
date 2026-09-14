//! Projects screen (m44 chunk 4): the project tree beside one project's
//! rules as an editable form. The week's minutes sit on every tree row
//! (a parent carries its children's), the selected project lists its open
//! declared tasks and can be tested over the last 7 days, and the unfiled
//! row lists the week's places, sites and titles no rule claims with an
//! "attach to" menu on each. Saving writes `[[projects]]`, tells the
//! daemon and re-files the last 30 days, then says what moved.

use std::collections::HashMap;

use chronicle_core::config::{Config, ProjectCfg, ProjectRule};
use chronicle_core::project::Matcher;
use chronicle_core::storage;
use eframe::egui;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use rusqlite::Connection;

use super::theme::{self, palette};
use super::{TimelineApp, fmt_dur};

/// Left column width when the window is wide enough for two columns.
const TREE_W: f32 = 240.0;
/// Indent per tree level.
const TREE_INDENT: f32 = 10.0;
const TEST_DAYS: u32 = 7;
const TEST_TOP: usize = 12;
/// Places, sites and titles listed on the unfiled row.
const UNFILED_TOP: usize = 12;
/// Days a save or an attach re-files.
const REFILE_DAYS: u32 = 30;

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

/// One line of the left tree.
struct TreeRow {
    depth: usize,
    name: String,
    /// This week: own filed minutes plus every descendant's.
    ms: i64,
    /// Found under a watched folder, not configured.
    discovered: bool,
    /// The editable row for this project; `None` for a discovered one
    /// (config.toml has nothing to edit yet).
    row: Option<usize>,
    /// Repo paths, shown on the discovered card.
    paths: Vec<String>,
}

/// One open declared task of the selected project.
struct TaskRow {
    label: String,
    ticket: Option<String>,
    /// `kind value` per pinned scope entry.
    scope: Vec<String>,
}

/// What the detail pane shows.
#[derive(Clone, PartialEq, Eq)]
enum Pick {
    /// An editable row (a project in config, or one just added).
    Row(usize),
    /// A discovered project, by name.
    Discovered(String),
    Unfiled,
}

/// One deferred action: the form mutably borrows the screen while it runs,
/// so the write happens after the frame's UI.
enum Op {
    Save,
    Test,
    Attach {
        project: String,
        rule: ProjectRule,
    },
    /// Give a discovered project a row of its own to edit.
    Adopt(String),
}

pub(super) struct ProjectsScreen {
    rows: Vec<Row>,
    /// The rows as the config last held them: a save with nothing changed
    /// writes nothing (and leaves the per-repo defaults unwritten).
    loaded: Vec<ProjectCfg>,
    tree: Vec<TreeRow>,
    join_min: u32,
    loaded_join: u32,
    pick: Pick,
    unfiled_ms: i64,
    /// The week's unfiled places, sites and titles.
    unfiled: crate::project::Report,
    /// Open declared tasks by lowercased project name.
    tasks: HashMap<String, Vec<TaskRow>>,
    /// The selected project is armed for delete (a second click removes it).
    arm_remove: bool,
    test_out: Option<String>,
    /// Set after a write: the rows reload on the next frame.
    pub(super) dirty: bool,
    /// Last write's outcome, under the header.
    pub(super) status_line: Option<Result<String, String>>,
}

impl ProjectsScreen {
    /// Read config and the week's minutes; opens on the first project.
    pub(super) fn from_config(conn: Option<&Connection>, config: &Config, tz: &TimeZone) -> Self {
        let mut screen = Self {
            rows: Vec::new(),
            loaded: Vec::new(),
            tree: Vec::new(),
            join_min: config.project_join_min,
            loaded_join: config.project_join_min,
            pick: Pick::Unfiled,
            unfiled_ms: 0,
            unfiled: crate::project::Report::default(),
            tasks: HashMap::new(),
            arm_remove: false,
            test_out: None,
            dirty: false,
            status_line: None,
        };
        screen.reload(conn, config, tz);
        if let Some(t) = screen.tree.first() {
            screen.pick = t
                .row
                .map_or_else(|| Pick::Discovered(t.name.clone()), Pick::Row);
        }
        screen
    }

    /// Rebuild rows, tree, minutes and tasks from config and the database.
    /// The selection survives by name; edits in flight do not.
    pub(super) fn reload(&mut self, conn: Option<&Connection>, config: &Config, tz: &TimeZone) {
        self.dirty = false;
        self.arm_remove = false;
        let was_unfiled = self.pick == Pick::Unfiled;
        let want = self.picked_name();
        self.loaded = config.projects_effective();
        self.rows = self.loaded.iter().map(Row::from_cfg).collect();
        self.join_min = config.project_join_min;
        self.loaded_join = config.project_join_min;

        let now = Timestamp::now();
        let hi = now.as_millisecond();
        let lo = chronicle_core::timeref::week_start(now.to_zoned(tz.clone()).date())
            .and_then(|d| d.to_zoned(tz.clone()).ok())
            .map_or(hi - 7 * 86_400_000, |z| z.timestamp().as_millisecond());
        let per = conn
            .and_then(|c| storage::project_ms(c, lo, hi).ok())
            .unwrap_or_default();
        self.unfiled_ms = per
            .iter()
            .filter(|(p, _)| p.is_none())
            .map(|(_, ms)| ms)
            .sum();
        let own_ms = |name: &str| -> i64 {
            per.iter()
                .filter(|(p, _)| p.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(name)))
                .map(|(_, ms)| ms)
                .sum()
        };
        let matcher = Matcher::from_config(config);
        let rows = &self.rows;
        self.tree = matcher
            .tree()
            .into_iter()
            .map(|(depth, p)| TreeRow {
                depth,
                ms: matcher.subtree_ms(&p.name, own_ms),
                discovered: p.discovered,
                row: rows
                    .iter()
                    .position(|r| r.name.trim().eq_ignore_ascii_case(&p.name)),
                paths: p.paths.iter().map(|q| q.display().to_string()).collect(),
                name: p.name.clone(),
            })
            .collect();

        self.unfiled = conn
            .and_then(|c| {
                let scopes = storage::task_scopes(c, Some(lo)).ok()?;
                let spans = storage::anchored_spans(c, lo, hi).ok()?;
                Some(crate::project::Report::build(
                    config,
                    &spans,
                    &scopes,
                    lo,
                    hi,
                    UNFILED_TOP,
                ))
            })
            .unwrap_or_default();
        self.tasks = conn.map(declared_by_project).unwrap_or_default();

        self.pick = if was_unfiled {
            Pick::Unfiled
        } else {
            self.resolve(want.as_deref())
        };
    }

    /// The selected project's name; `None` on the unfiled row.
    fn picked_name(&self) -> Option<String> {
        match &self.pick {
            Pick::Row(i) => self.rows.get(*i).map(|r| r.name.trim().to_owned()),
            Pick::Discovered(n) => Some(n.clone()),
            Pick::Unfiled => None,
        }
    }

    /// A name back to a selection after a reload; the first project when
    /// the name is gone.
    fn resolve(&self, name: Option<&str>) -> Pick {
        let by_name = name.filter(|n| !n.is_empty()).and_then(|n| {
            self.rows
                .iter()
                .position(|r| r.name.trim().eq_ignore_ascii_case(n))
                .map(Pick::Row)
                .or_else(|| {
                    self.tree
                        .iter()
                        .find(|t| t.discovered && t.name.eq_ignore_ascii_case(n))
                        .map(|t| Pick::Discovered(t.name.clone()))
                })
        });
        by_name.unwrap_or_else(|| {
            self.tree.first().map_or(Pick::Unfiled, |t| {
                t.row
                    .map_or_else(|| Pick::Discovered(t.name.clone()), Pick::Row)
            })
        })
    }

    /// The projects to write: every row with a name.
    fn cfgs(&self) -> Vec<ProjectCfg> {
        self.rows
            .iter()
            .map(Row::to_cfg)
            .filter(|c| !c.name.is_empty())
            .collect()
    }

    /// Nothing differs from the file as loaded.
    pub(super) fn unchanged(&self) -> bool {
        self.join_min == self.loaded_join && self.cfgs() == self.loaded
    }

    /// Configured names, for the parent picker and the attach menus.
    fn names(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|r| r.name.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect()
    }
}

/// Open declared tasks keyed by lowercased project: label from the task
/// list, ticket and pinned scope from the scopes.
fn declared_by_project(conn: &Connection) -> HashMap<String, Vec<TaskRow>> {
    let scopes = storage::task_scopes(conn, None).unwrap_or_default();
    let labels: HashMap<i64, String> = storage::all_tasks(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.id, t.label))
        .collect();
    let mut out: HashMap<String, Vec<TaskRow>> = HashMap::new();
    for task in storage::open_declared(conn).unwrap_or_default() {
        let Some(project) = task
            .project
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        else {
            continue;
        };
        let scope = scopes.iter().find(|s| s.task_id == task.id);
        out.entry(project.to_ascii_lowercase())
            .or_default()
            .push(TaskRow {
                label: labels
                    .get(&task.id)
                    .cloned()
                    .unwrap_or_else(|| format!("task {}", task.id)),
                ticket: scope.and_then(|s| s.ticket.clone()),
                scope: scope
                    .map(|s| {
                        s.entries
                            .iter()
                            .map(|(k, v)| format!("{} {v}", k.as_str()))
                            .collect()
                    })
                    .unwrap_or_default(),
            });
    }
    out
}

impl TimelineApp {
    pub(super) fn projects_ui(&mut self, ui: &mut egui::Ui) {
        if self.projects_screen.is_none() {
            let config = self.config.clone().unwrap_or_default();
            self.projects_screen = Some(ProjectsScreen::from_config(
                self.conn.as_ref(),
                &config,
                &self.tz,
            ));
        }
        if self.projects_screen.as_ref().is_some_and(|s| s.dirty) {
            let config = self.config.clone().unwrap_or_default();
            if let Some(screen) = self.projects_screen.as_mut() {
                screen.reload(self.conn.as_ref(), &config, &self.tz);
            }
        }
        let wide = theme::wide(ui.ctx());
        let mut op: Option<Op> = None;
        let Some(screen) = &mut self.projects_screen else {
            return;
        };

        theme::page().show(ui, |ui| {
            let content_w = theme::content_width(ui);
            header_ui(ui, screen);
            ui.add_space(theme::SPACE_XS);
            if wide {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(TREE_W);
                        egui::ScrollArea::vertical()
                            .id_salt("projects_tree")
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                ui.set_width(TREE_W);
                                tree_ui(ui, screen);
                            });
                    });
                    ui.add_space(theme::SPACE_LG);
                    let detail_w = (content_w - TREE_W - theme::SPACE_LG).max(240.0);
                    ui.vertical(|ui| {
                        ui.set_width(detail_w);
                        egui::ScrollArea::vertical()
                            .id_salt("projects_detail")
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                ui.set_max_width(detail_w);
                                detail_ui(ui, detail_w, screen, &mut op);
                            });
                    });
                });
            } else {
                picker_ui(ui, screen);
                ui.add_space(theme::SPACE_SM);
                egui::ScrollArea::vertical()
                    .id_salt("projects_detail")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.set_max_width(content_w);
                        detail_ui(ui, content_w, screen, &mut op);
                    });
            }
        });

        match op {
            Some(Op::Save) => self.save_projects(),
            Some(Op::Attach { project, rule }) => self.attach_project_rule(&project, rule),
            Some(Op::Test) => {
                if let Some(screen) = self.projects_screen.as_mut() {
                    screen.test_out =
                        Some(run_test(self.conn.as_ref(), screen.cfgs(), screen.join_min));
                }
            }
            Some(Op::Adopt(name)) => {
                if let Some(screen) = self.projects_screen.as_mut() {
                    adopt(screen, &name);
                }
            }
            None => {}
        }
    }

    /// Write every row through the one config writer, tell the daemon and
    /// re-file the last 30 days; the status line says what moved.
    fn save_projects(&mut self) {
        let Some(screen) = self.projects_screen.as_ref() else {
            return;
        };
        if screen.unchanged() {
            if let Some(screen) = self.projects_screen.as_mut() {
                screen.status_line = Some(Ok("no changes".to_owned()));
            }
            return;
        }
        let (cfgs, join_min) = (screen.cfgs(), screen.join_min);
        let (loaded, loaded_join) = (screen.loaded.clone(), screen.loaded_join);
        let path = self.config_path.clone();
        // The rows replace `[[projects]]` whole, so a change another
        // surface wrote since this screen loaded (a Home card, a timeline
        // menu, `project attach`) would go with them: reload instead and
        // ask for the edit again.
        if let Ok(fresh) = Config::load(&path)
            && (fresh.projects_effective() != loaded || fresh.project_join_min != loaded_join)
        {
            if let Some(screen) = self.projects_screen.as_mut() {
                screen.reload(self.conn.as_ref(), &fresh, &self.tz);
                screen.status_line = Some(Err(
                    "config.toml changed since this screen loaded; reloaded it, apply your edit again"
                        .to_owned(),
                ));
            }
            self.config = Some(fresh);
            return;
        }
        let written = match chronicle_core::config::write_projects(&path, cfgs, Some(join_min)) {
            Ok(c) => c,
            Err(e) => {
                if let Some(screen) = self.projects_screen.as_mut() {
                    screen.status_line = Some(Err(e.to_string()));
                }
                return;
            }
        };
        let _ = crate::send_ctrl(&self.sock_path, "reload");
        let matcher = Matcher::from_config(&written);
        let lo = Timestamp::now().as_millisecond() - i64::from(REFILE_DAYS) * 86_400_000;
        let moved = match self.conn.as_mut() {
            Some(conn) => {
                storage::file_spans_report(conn, lo, i64::MAX, &matcher, written.project_join_min)
                    .map_err(|e| e.to_string())
            }
            None => Ok(Vec::new()),
        };
        let status = moved.map(|m| {
            format!(
                "saved \u{b7} {}",
                crate::project::moved_lines(&m, REFILE_DAYS)
                    .trim_end()
                    .replace('\n', " \u{b7} ")
            )
        });
        if let Some(screen) = self.projects_screen.as_mut() {
            screen.status_line = Some(status);
            screen.reload(self.conn.as_ref(), &written, &self.tz);
        }
        self.config = Some(written);
        self.loaded_at = None;
    }

    /// One rule onto one project, through the shared attach path.
    fn attach_project_rule(&mut self, project: &str, rule: ProjectRule) {
        let outcome =
            crate::project::attach_and_refile(&self.data_dir, project, &[rule], REFILE_DAYS);
        let status = match outcome {
            Ok(o) => Ok(o.render().trim_end().replace('\n', " \u{b7} ")),
            Err(e) => Err(e.to_string()),
        };
        let config = chronicle_core::config::Config::load(&self.config_path).ok();
        if let Some(screen) = self.projects_screen.as_mut() {
            screen.status_line = Some(status);
            match &config {
                Some(c) => screen.reload(self.conn.as_ref(), c, &self.tz),
                None => screen.dirty = true,
            }
        }
        if config.is_some() {
            self.config = config;
        }
        self.loaded_at = None;
    }
}

/// Title, the week's totals and the last write's outcome.
fn header_ui(ui: &mut egui::Ui, screen: &ProjectsScreen) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Projects")
                .text_style(egui::TextStyle::Heading)
                .color(palette::TEXT),
        );
        ui.weak(format!(
            "\u{b7} {} \u{b7} {} unfiled this week",
            screen.tree.len(),
            fmt_dur(screen.unfiled_ms)
        ));
    });
    if let Some(status) = &screen.status_line {
        let (text, color) = match status {
            Ok(s) => (s.as_str(), palette::TEXT_DIM),
            Err(e) => (e.as_str(), palette::AMBER),
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .text_style(egui::TextStyle::Small)
                    .color(color),
            )
            .wrap(),
        );
    }
}

/// The tree as a dropdown, for a window too narrow for two columns.
fn picker_ui(ui: &mut egui::Ui, screen: &mut ProjectsScreen) {
    let current = match screen.picked_name() {
        Some(n) if !n.is_empty() => n,
        Some(_) => "(unnamed)".to_owned(),
        None => "unfiled".to_owned(),
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("projects_pick")
            .selected_text(current)
            .width(200.0)
            .show_ui(ui, |ui| {
                for i in 0..screen.tree.len() {
                    let (depth, name, row) = {
                        let t = &screen.tree[i];
                        (t.depth, t.name.clone(), t.row)
                    };
                    let pick = row.map_or_else(|| Pick::Discovered(name.clone()), Pick::Row);
                    let label = format!("{}{name}", "    ".repeat(depth));
                    if ui.selectable_label(screen.pick == pick, label).clicked() {
                        screen.pick = pick;
                        screen.arm_remove = false;
                    }
                }
                if ui
                    .selectable_label(screen.pick == Pick::Unfiled, "unfiled")
                    .clicked()
                {
                    screen.pick = Pick::Unfiled;
                    screen.arm_remove = false;
                }
            });
        if theme::secondary_button(ui, "add project").clicked() {
            add_project(screen);
        }
    });
}

/// One row per project, indented by depth, with the week's minutes; the
/// unfiled row closes the list.
fn tree_ui(ui: &mut egui::Ui, screen: &mut ProjectsScreen) {
    let mut pick: Option<Pick> = None;
    for t in &screen.tree {
        let selected = match (&screen.pick, t.row) {
            (Pick::Row(i), Some(j)) => *i == j,
            (Pick::Discovered(n), None) => n.eq_ignore_ascii_case(&t.name),
            _ => false,
        };
        let name = if t.name.trim().is_empty() {
            "(unnamed)"
        } else {
            t.name.as_str()
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme::SPACE_XS;
            ui.add_space(t.depth as f32 * TREE_INDENT);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(theme::num(fmt_dur(t.ms))).selectable(false));
                if t.discovered {
                    theme::badge(ui, "discovered", palette::TEXT_DIM);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if theme::selectable(ui, selected, name).clicked() {
                        pick = Some(
                            t.row
                                .map_or_else(|| Pick::Discovered(t.name.clone()), Pick::Row),
                        );
                    }
                });
            });
        });
    }
    ui.add_space(theme::SPACE_XS);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::SPACE_XS;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add(egui::Label::new(theme::num(fmt_dur(screen.unfiled_ms))).selectable(false));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if theme::selectable(ui, screen.pick == Pick::Unfiled, "unfiled").clicked() {
                    pick = Some(Pick::Unfiled);
                }
            });
        });
    });
    ui.add_space(theme::SPACE_SM);
    if theme::secondary_button(ui, "add project").clicked() {
        add_project(screen);
    }
    if let Some(p) = pick {
        screen.pick = p;
        screen.arm_remove = false;
    }
}

/// A new empty row, selected so the form opens on it.
fn add_project(screen: &mut ProjectsScreen) {
    screen.rows.push(Row::from_cfg(&ProjectCfg::default()));
    screen.pick = Pick::Row(screen.rows.len() - 1);
    screen.arm_remove = false;
}

/// A discovered project becomes an editable row seeded with its paths.
fn adopt(screen: &mut ProjectsScreen, name: &str) {
    let paths = screen
        .tree
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .map(|t| t.paths.join(", "))
        .unwrap_or_default();
    screen.rows.push(Row {
        name: name.to_owned(),
        repos: paths,
        ..Row::from_cfg(&ProjectCfg::default())
    });
    screen.pick = Pick::Row(screen.rows.len() - 1);
}

fn detail_ui(ui: &mut egui::Ui, width: f32, screen: &mut ProjectsScreen, op: &mut Option<Op>) {
    match screen.pick.clone() {
        Pick::Unfiled => unfiled_ui(ui, width, screen, op),
        Pick::Discovered(name) => discovered_ui(ui, &name, screen, op),
        Pick::Row(i) if i < screen.rows.len() => {
            // A delete removes the row: `i` no longer names it.
            if form_ui(ui, width, screen, i, op) {
                return;
            }
            ui.add_space(theme::SECTION_GAP);
            tasks_ui(ui, width, screen, i);
            ui.add_space(theme::SECTION_GAP);
            test_ui(ui, screen, op);
        }
        Pick::Row(_) => {
            theme::empty_state(ui, "no project", "pick one on the left, or add one");
        }
    }
}

/// A project found under a watched folder: nothing to edit until it is in
/// config.toml.
fn discovered_ui(ui: &mut egui::Ui, name: &str, screen: &ProjectsScreen, op: &mut Option<Op>) {
    theme::section_header_with(ui, name, None, |_| {});
    ui.add_space(theme::SPACE_SM);
    ui.weak("found under a watched folder; nothing in config.toml claims it yet");
    if let Some(t) = screen
        .tree
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
    {
        for p in &t.paths {
            ui.label(
                egui::RichText::new(p)
                    .text_style(egui::TextStyle::Monospace)
                    .color(palette::TEXT_DIM),
            );
        }
    }
    ui.add_space(theme::SPACE_SM);
    if theme::secondary_button(ui, "add to projects").clicked() {
        *op = Some(Op::Adopt(name.to_owned()));
    }
}

/// The selected project's rules, as the settings card edited them.
fn form_ui(
    ui: &mut egui::Ui,
    width: f32,
    screen: &mut ProjectsScreen,
    i: usize,
    op: &mut Option<Op>,
) -> bool {
    let others: Vec<String> = screen
        .rows
        .iter()
        .enumerate()
        .filter(|(j, r)| *j != i && !r.name.trim().is_empty())
        .map(|(_, r)| r.name.trim().to_owned())
        .collect();
    let armed = screen.arm_remove;
    let mut remove = false;
    let mut arm = false;
    {
        let row = &mut screen.rows[i];
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut row.name)
                    .desired_width(180.0)
                    .hint_text("name"),
            );
            ui.checkbox(&mut row.derive, "derive")
                .on_hover_text("mint sub-tasks inside this project");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if armed { "delete?" } else { "delete" };
                if theme::ghost_button(ui, label).clicked() {
                    if armed {
                        remove = true;
                    } else {
                        arm = true;
                    }
                }
            });
        });
        ui.add_space(theme::SPACE_XS);
        ui.weak(
            "every focus span files into the first project a rule matches: repo path or \
             folder, ticket prefix, site, title regex, app; lists are comma-separated, rules \
             on a project with a parent run before the parent's",
        );
        let field_w = (width - 80.0).max(120.0);
        egui::Grid::new(("projects_form", i))
            .num_columns(2)
            .min_col_width(52.0)
            .show(ui, |ui| {
                ui.weak("parent");
                let current = if row.parent.trim().is_empty() {
                    "none".to_owned()
                } else {
                    row.parent.trim().to_owned()
                };
                egui::ComboBox::from_id_salt(("projects_parent", i))
                    .selected_text(current)
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut row.parent, String::new(), "none");
                        for n in &others {
                            ui.selectable_value(&mut row.parent, n.clone(), n.as_str());
                        }
                    });
                ui.end_row();
                let field = |ui: &mut egui::Ui, label: &str, s: &mut String, hint: &str| {
                    ui.weak(label);
                    ui.add(
                        egui::TextEdit::singleline(s)
                            .desired_width(field_w)
                            .font(egui::TextStyle::Monospace)
                            .hint_text(hint),
                    );
                    ui.end_row();
                };
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
    if arm {
        screen.arm_remove = true;
    }
    if remove {
        // Its children move up to its parent: a save with a dangling
        // `parent` is refused and would leave the rows ahead of the file.
        let gone = screen.rows[i].name.trim().to_owned();
        let up = screen.rows[i].parent.clone();
        screen.rows.remove(i);
        if !gone.is_empty() {
            for r in &mut screen.rows {
                if r.parent.trim().eq_ignore_ascii_case(&gone) {
                    r.parent = up.clone();
                }
            }
        }
        screen.arm_remove = false;
        screen.pick = if screen.rows.is_empty() {
            Pick::Unfiled
        } else {
            Pick::Row(i.min(screen.rows.len() - 1))
        };
        *op = Some(Op::Save);
        return true;
    }
    ui.add_space(theme::SPACE_SM);
    ui.horizontal(|ui| {
        if theme::primary_button(ui, "save").clicked() {
            *op = Some(Op::Save);
        }
        ui.label("join glances under");
        ui.add(egui::DragValue::new(&mut screen.join_min).range(0..=30));
        ui.weak("min");
    });
    false
}

/// The selected project's open declared tasks, with what is pinned to them.
fn tasks_ui(ui: &mut egui::Ui, width: f32, screen: &ProjectsScreen, i: usize) {
    let name = screen.rows[i].name.trim().to_ascii_lowercase();
    let empty: Vec<TaskRow> = Vec::new();
    let tasks = screen.tasks.get(&name).unwrap_or(&empty);
    theme::section_header_with(ui, "declared tasks", Some(tasks.len()), |_| {});
    ui.add_space(theme::SPACE_SM);
    if tasks.is_empty() {
        ui.weak("no open task declared in this project");
        return;
    }
    let color = theme::project_hue(&screen.rows[i].name);
    for t in tasks {
        let mut row = theme::ListRow::new(&t.label)
            .dot(color)
            .subtitle(t.scope.join(" \u{b7} "));
        if let Some(k) = &t.ticket {
            row = row.chip(k.as_str(), palette::TEXT_DIM);
        }
        row.show(ui, width, |_| {});
    }
}

/// `project test` inline: how the rows as edited would file the last week.
fn test_ui(ui: &mut egui::Ui, screen: &ProjectsScreen, op: &mut Option<Op>) {
    if theme::secondary_button(ui, &format!("test last {TEST_DAYS} days")).clicked() {
        *op = Some(Op::Test);
    }
    if let Some(text) = &screen.test_out {
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

fn run_test(conn: Option<&Connection>, cfgs: Vec<ProjectCfg>, join_min: u32) -> String {
    let Some(conn) = conn else {
        return "no database".to_owned();
    };
    let config = Config {
        projects: cfgs,
        project_join_min: join_min,
        ..Config::default()
    };
    let hi = Timestamp::now().as_millisecond();
    let lo = hi - i64::from(TEST_DAYS) * 86_400_000;
    let scopes = storage::task_scopes(conn, Some(lo)).unwrap_or_default();
    match storage::anchored_spans(conn, lo, hi) {
        Ok(spans) => crate::project::Report::build(&config, &spans, &scopes, lo, hi, TEST_TOP)
            .render(TEST_DAYS),
        Err(e) => format!("test failed: {e}"),
    }
}

/// The week's unfiled places, sites and titles, each with an attach menu.
fn unfiled_ui(ui: &mut egui::Ui, width: f32, screen: &ProjectsScreen, op: &mut Option<Op>) {
    theme::section_header_with(ui, "unfiled", None, |ui| {
        ui.label(theme::num(fmt_dur(screen.unfiled_ms)));
    });
    ui.add_space(theme::SPACE_SM);
    ui.weak("time this week no rule claims; attach one to a project and the last 30 days re-file");
    let names = screen.names();
    let places: Vec<(String, String, i64, ProjectRule)> = screen
        .unfiled
        .places
        .iter()
        .map(|(place, ms)| {
            (
                place.clone(),
                String::new(),
                *ms,
                ProjectRule::Repo(place.clone()),
            )
        })
        .collect();
    let domains: Vec<(String, String, i64, ProjectRule)> = screen
        .unfiled
        .domains
        .iter()
        .map(|(domain, ms)| {
            (
                domain.clone(),
                String::new(),
                *ms,
                ProjectRule::Domain(domain.clone()),
            )
        })
        .collect();
    let titles: Vec<(String, String, i64, ProjectRule)> = screen
        .unfiled
        .titles
        .iter()
        .map(|(app, title, ms)| {
            (
                theme::display_title(title).to_owned(),
                app.clone(),
                *ms,
                ProjectRule::Title(chronicle_core::config::title_rule(title)),
            )
        })
        .collect();
    for (title, items) in [
        ("places", &places),
        ("sites", &domains),
        ("titles", &titles),
    ] {
        if items.is_empty() {
            continue;
        }
        ui.add_space(theme::SECTION_GAP);
        theme::section_header_with(ui, title, Some(items.len()), |_| {});
        ui.add_space(theme::SPACE_SM);
        for (label, sub, ms, rule) in items {
            theme::ListRow::new(label)
                .ring()
                .subtitle(sub.clone())
                .num(fmt_dur(*ms))
                .show(ui, width, |ui| {
                    ui.menu_button("attach to\u{2026}", |ui| {
                        if names.is_empty() {
                            ui.weak("no project configured");
                        }
                        for n in &names {
                            if ui.button(n).clicked() {
                                *op = Some(Op::Attach {
                                    project: n.clone(),
                                    rule: rule.clone(),
                                });
                                ui.close();
                            }
                        }
                    });
                });
        }
    }
    if places.is_empty() && domains.is_empty() && titles.is_empty() {
        ui.add_space(theme::SPACE_SM);
        ui.weak("nothing unfiled this week");
    }
}
