//! Task manager takeover (m35 chunk 3): every task in one list — project,
//! label, state, source, minutes today and this week, last activity —
//! with filters (open / closed / all, declared / derived, per project),
//! inline rename with a project picker, and multi-select for merge, close,
//! reopen, move, make current and delete-derived. Home's row menus stay
//! for one-off edits; this is where a week's mess gets cleaned up.

use std::collections::HashSet;

use chronicle_core::storage::{self, ManagedTask};
use chronicle_core::types::ms_to_ts;
use eframe::egui;
use jiff::ToSpan;
use jiff::tz::TimeZone;
use rusqlite::Connection;

use super::timeline::{matches_filter, merge_item, merge_picker};
use super::{Action, EditState, TimelineApp, fmt_dur, theme};

/// One listed task with its minutes.
struct Row {
    task: ManagedTask,
    today_ms: i64,
    week_ms: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum StatusFilter {
    #[default]
    Open,
    Closed,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SourceFilter {
    #[default]
    All,
    Declared,
    Derived,
}

/// Project filter: `None` = every project; `Some(None)` = unfiled only;
/// `Some(Some(p))` = one project.
type ProjectFilter = Option<Option<String>>;

#[derive(Default)]
pub(super) struct TaskManager {
    rows: Vec<Row>,
    /// Every project name seen on a task or in the config, config first.
    projects: Vec<String>,
    selected: HashSet<i64>,
    status: StatusFilter,
    source: SourceFilter,
    project: ProjectFilter,
    query: String,
    query_lc: String,
    edit: Option<EditState>,
    /// Task whose inline "merge into" picker is open.
    merge_pick: Option<i64>,
    /// The bulk delete's second step is armed.
    confirm_delete: bool,
    /// Set after an action: rows reload on the next frame.
    pub(super) dirty: bool,
    /// Last action's outcome, under the header.
    pub(super) status_line: Option<Result<String, String>>,
}

impl TaskManager {
    /// Rebuild the rows; selection and filters survive for tasks still
    /// listed. Minutes are today's and this week's interval time, weighted
    /// by share like every other total.
    pub(super) fn reload(&mut self, conn: &Connection, tz: &TimeZone) {
        self.dirty = false;
        let tasks = match storage::all_tasks(conn) {
            Ok(t) => t,
            Err(e) => {
                self.status_line = Some(Err(format!("tasks: {e}")));
                return;
            }
        };
        let now = jiff::Timestamp::now();
        let today = now.to_zoned(tz.clone()).date();
        let day = today
            .to_zoned(tz.clone())
            .ok()
            .and_then(|s| Some((s.timestamp().as_millisecond(), s.checked_add(1.day()).ok()?)))
            .map(|(lo, e)| (lo, e.timestamp().as_millisecond()));
        let week = chronicle_core::timeref::week_start(today)
            .and_then(|d| d.to_zoned(tz.clone()).ok())
            .map(|s| (s.timestamp().as_millisecond(), now.as_millisecond()));
        let sum = |range: Option<(i64, i64)>| -> std::collections::HashMap<i64, i64> {
            let mut out = std::collections::HashMap::new();
            let Some((lo, hi)) = range else {
                return out;
            };
            if let Ok(rows) = storage::tasks_in_range(conn, lo, hi) {
                for t in rows {
                    let (s, e) = (
                        t.start_ts.as_millisecond().max(lo),
                        t.end_ts.as_millisecond().min(hi),
                    );
                    if e > s {
                        *out.entry(t.id).or_default() += t.weigh(e - s);
                    }
                }
            }
            out
        };
        let today_ms = sum(day);
        let week_ms = sum(week);
        self.rows = tasks
            .into_iter()
            .map(|task| Row {
                today_ms: today_ms.get(&task.id).copied().unwrap_or(0),
                week_ms: week_ms.get(&task.id).copied().unwrap_or(0),
                task,
            })
            .collect();
        let listed: HashSet<i64> = self.rows.iter().map(|r| r.task.id).collect();
        self.selected.retain(|id| listed.contains(id));
        if self
            .edit
            .as_ref()
            .is_some_and(|e| !listed.contains(&e.task_id))
        {
            self.edit = None;
        }
        for r in &self.rows {
            if let Some(p) = r.task.project.as_deref().filter(|p| !p.trim().is_empty())
                && !self.projects.iter().any(|q| q == p)
            {
                self.projects.push(p.to_owned());
            }
        }
    }

    /// Seed the project list with the configured names, in config order.
    pub(super) fn set_projects(&mut self, configured: &[String]) {
        let mut projects: Vec<String> = configured.to_vec();
        for p in &self.projects {
            if !projects.contains(p) {
                projects.push(p.clone());
            }
        }
        self.projects = projects;
    }

    fn visible(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&i| {
                let t = &self.rows[i].task;
                let status = match self.status {
                    StatusFilter::Open => t.open,
                    StatusFilter::Closed => !t.open,
                    StatusFilter::All => true,
                };
                let source = match self.source {
                    SourceFilter::All => true,
                    SourceFilter::Declared => t.declared,
                    SourceFilter::Derived => !t.declared,
                };
                let project = match &self.project {
                    None => true,
                    Some(want) => &t.project.clone().filter(|p| !p.trim().is_empty()) == want,
                };
                status
                    && source
                    && project
                    && matches_filter(&self.query_lc, &t.label, t.project.as_deref())
            })
            .collect()
    }
}

/// Checkbox column width on a manager row.
const CHECK_COL: f32 = 22.0;

impl TimelineApp {
    pub(super) fn tasks_ui(&mut self, ui: &mut egui::Ui) {
        if self.tasks.as_ref().is_some_and(|t| t.dirty)
            && let (Some(conn), Some(panel)) = (self.conn.as_ref(), self.tasks.as_mut())
        {
            panel.reload(conn, &self.tz);
        }
        let configured: Vec<String> = self
            .config
            .as_ref()
            .map(|c| {
                c.projects_effective()
                    .iter()
                    .map(|p| p.name.clone())
                    .filter(|n| !n.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let candidates = self.merge_candidates();
        let tz = self.tz.clone();
        let mut close = false;
        let mut pending: Vec<Action> = Vec::new();
        let Some(panel) = &mut self.tasks else {
            return;
        };
        panel.set_projects(&configured);
        let visible = panel.visible();
        let open_count = panel.rows.iter().filter(|r| r.task.open).count();

        theme::page().show(ui, |ui| {
            let content_w = theme::content_width(ui);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Tasks")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.weak(format!(
                    "\u{b7} {} open \u{b7} {} listed",
                    open_count,
                    visible.len()
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{d7}").clicked() {
                        close = true;
                    }
                });
            });
            if let Some(status) = &panel.status_line {
                let (text, color) = match status {
                    Ok(s) => (s.as_str(), theme::palette::TEXT_DIM),
                    Err(e) => (e.as_str(), theme::palette::AMBER),
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
            ui.add_space(theme::SPACE_XS);
            filters_ui(ui, panel);
            ui.add_space(theme::SPACE_SM);

            // Bulk row: what the selection can have done to it.
            if !panel.selected.is_empty() {
                bulk_ui(ui, panel, &candidates, &mut pending);
                ui.add_space(theme::SPACE_SM);
            }

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    if visible.is_empty() {
                        ui.weak("no tasks match");
                        return;
                    }
                    for &i in &visible {
                        task_row_ui(ui, content_w, &tz, panel, i, &candidates, &mut pending);
                    }
                });
        });
        if close {
            self.tasks = None;
            self.loaded_at = None;
        }
        for action in pending {
            self.apply_action(action);
        }
    }
}

/// Status, source and project filters plus the text query.
fn filters_ui(ui: &mut egui::Ui, panel: &mut TaskManager) {
    ui.horizontal_wrapped(|ui| {
        segmented(ui, |ui| {
            for (v, label) in [
                (StatusFilter::Open, "open"),
                (StatusFilter::Closed, "closed"),
                (StatusFilter::All, "all"),
            ] {
                if theme::selectable(ui, panel.status == v, label).clicked() {
                    panel.status = v;
                }
            }
        });
        segmented(ui, |ui| {
            for (v, label) in [
                (SourceFilter::All, "any source"),
                (SourceFilter::Declared, "declared"),
                (SourceFilter::Derived, "derived"),
            ] {
                if theme::selectable(ui, panel.source == v, label).clicked() {
                    panel.source = v;
                }
            }
        });
    });
    ui.add_space(theme::SPACE_XS);
    ui.horizontal(|ui| {
        let current = match &panel.project {
            None => "every project".to_owned(),
            Some(None) => "unfiled".to_owned(),
            Some(Some(p)) => p.clone(),
        };
        egui::ComboBox::from_id_salt("tasks_project_filter")
            .selected_text(current)
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut panel.project, None, "every project");
                for p in panel.projects.clone() {
                    ui.selectable_value(&mut panel.project, Some(Some(p.clone())), p);
                }
                ui.selectable_value(&mut panel.project, Some(None), "unfiled");
            });
        if ui
            .add(
                egui::TextEdit::singleline(&mut panel.query)
                    .desired_width(ui.available_width())
                    .hint_text("filter\u{2026}"),
            )
            .changed()
        {
            panel.query_lc = panel.query.trim().to_lowercase();
        }
    });
}

/// The segmented-control frame the top bar uses for its view switcher.
fn segmented(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::palette::INPUT_BG)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(add);
        });
}

/// Actions over the selection: every row-menu action, applied to each
/// selected task it fits (a "close" skips the closed ones; "delete" only
/// ever touches derived tasks). Merge and move are menus; delete is
/// two-step.
fn bulk_ui(
    ui: &mut egui::Ui,
    panel: &mut TaskManager,
    candidates: &[(i64, String)],
    pending: &mut Vec<Action>,
) {
    let selected: Vec<&ManagedTask> = panel
        .rows
        .iter()
        .filter(|r| panel.selected.contains(&r.task.id))
        .map(|r| &r.task)
        .collect();
    let n = selected.len();
    let open_ids: Vec<i64> = selected.iter().filter(|t| t.open).map(|t| t.id).collect();
    let closed_ids: Vec<i64> = selected.iter().filter(|t| !t.open).map(|t| t.id).collect();
    let derived_ids: Vec<i64> = selected
        .iter()
        .filter(|t| !t.declared)
        .map(|t| t.id)
        .collect();
    let current_pick: Option<i64> = match selected.as_slice() {
        [t] if t.declared && t.open && !t.current => Some(t.id),
        _ => None,
    };
    let moves: Vec<(i64, String)> = selected.iter().map(|t| (t.id, t.label.clone())).collect();
    let projects = panel.projects.clone();
    let mut clear = false;
    let mut confirm = panel.confirm_delete;
    egui::Frame::new()
        .fill(theme::palette::SURFACE_2)
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_MD))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("{n} selected"))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                if !open_ids.is_empty() && ui.small_button("close").clicked() {
                    pending.extend(open_ids.iter().map(|&id| Action::Close(id)));
                    clear = true;
                }
                if !closed_ids.is_empty() && ui.small_button("reopen").clicked() {
                    pending.extend(closed_ids.iter().map(|&id| Action::Reopen(id)));
                    clear = true;
                }
                if let Some(id) = current_pick
                    && ui
                        .small_button("make current")
                        .on_hover_text("the project's sink: new time on it goes here")
                        .clicked()
                {
                    pending.push(Action::SetCurrent(id));
                    clear = true;
                }
                ui.menu_button("merge into\u{2026}", |ui| {
                    let mut any = false;
                    for (to_task, label) in candidates {
                        if panel.selected.contains(to_task) {
                            continue;
                        }
                        any = true;
                        if ui.button(label).clicked() {
                            pending.extend(selected.iter().map(|t| Action::Merge {
                                from_task: t.id,
                                to_task: *to_task,
                            }));
                            clear = true;
                            ui.close();
                        }
                    }
                    if !any {
                        ui.weak("no other open task");
                    }
                });
                ui.menu_button("move to\u{2026}", |ui| {
                    for p in &projects {
                        if ui.button(p).clicked() {
                            pending.extend(moves.iter().map(|(id, label)| {
                                Action::Rename(EditState {
                                    task_id: *id,
                                    label: label.clone(),
                                    project: p.clone(),
                                    description: None,
                                })
                            }));
                            clear = true;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("no project").clicked() {
                        pending.extend(moves.iter().map(|(id, label)| {
                            Action::Rename(EditState {
                                task_id: *id,
                                label: label.clone(),
                                project: String::new(),
                                description: None,
                            })
                        }));
                        clear = true;
                        ui.close();
                    }
                });
                if !derived_ids.is_empty() {
                    let k = derived_ids.len();
                    if confirm {
                        if theme::primary_button(ui, &format!("delete {k} derived?")).clicked() {
                            pending.extend(derived_ids.iter().map(|&id| Action::DeleteDerived(id)));
                            clear = true;
                        }
                        if theme::ghost_button(ui, "keep").clicked() {
                            confirm = false;
                        }
                    } else if theme::ghost_button(ui, format!("delete {k} derived"))
                        .on_hover_text(
                            "remove the model's task outright; its time goes back to unassigned",
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                }
                if theme::ghost_button(ui, "clear").clicked() {
                    clear = true;
                }
            });
        });
    panel.confirm_delete = confirm;
    if clear {
        panel.selected.clear();
        panel.confirm_delete = false;
    }
}

/// One task: checkbox, then the row grammar (identity dot, label, project
/// and state chips, today's minutes; week, last activity and birth on the
/// second line) and a menu with every single-task action.
fn task_row_ui(
    ui: &mut egui::Ui,
    content_w: f32,
    tz: &TimeZone,
    panel: &mut TaskManager,
    i: usize,
    candidates: &[(i64, String)],
    pending: &mut Vec<Action>,
) {
    let row_w = content_w - CHECK_COL;
    let (id, label, project, open, declared, current) = {
        let t = &panel.rows[i].task;
        (
            t.id,
            t.label.clone(),
            t.project.clone(),
            t.open,
            t.declared,
            t.current,
        )
    };
    let color = theme::task_color(id, project.as_deref());
    if panel.edit.as_ref().is_some_and(|e| e.task_id == id) {
        edit_form_ui(ui, content_w, panel, pending);
        return;
    }
    let (today_ms, week_ms, last_ts, created_ts, external_ref) = {
        let r = &panel.rows[i];
        (
            r.today_ms,
            r.week_ms,
            r.task.last_ts,
            r.task.created_ts,
            r.task.external_ref.clone(),
        )
    };
    let hm = |ms: i64| ms_to_ts(ms).to_zoned(tz.clone());
    let mut parts: Vec<String> = vec![format!("{} this week", fmt_dur(week_ms))];
    if let Some(last) = last_ts {
        parts.push(format!("last {}", hm(last).strftime("%a %H:%M")));
    }
    parts.push(format!("since {}", hm(created_ts).strftime("%-d %b")));
    let subtitle = parts.join(" \u{b7} ");
    ui.horizontal(|ui| {
        let mut on = panel.selected.contains(&id);
        if ui
            .add_sized(
                [CHECK_COL - 6.0, ui.spacing().interact_size.y],
                egui::Checkbox::without_text(&mut on),
            )
            .changed()
        {
            if on {
                panel.selected.insert(id);
            } else {
                panel.selected.remove(&id);
            }
            panel.confirm_delete = false;
        }
        let mut row = theme::ListRow::new(&label)
            .dot(color)
            .padded()
            .subtitle(subtitle)
            .num(fmt_dur(today_ms));
        if let Some(p) = &project {
            row = row.chip(p.as_str(), color);
        }
        if let Some(key) = &external_ref {
            row = row.chip(key.as_str(), theme::palette::TEXT_DIM);
        }
        if current {
            row = row.chip("current", theme::palette::ACCENT);
        }
        if !declared {
            row = row.chip("derived", theme::palette::TEXT_DIM);
        }
        if !open {
            row = row.chip("closed", theme::palette::TEXT_DIM);
        }
        row.show(ui, row_w, |ui| {
            ui.menu_button("\u{2026}", |ui| {
                if ui.button("rename").clicked() {
                    panel.edit = Some(EditState {
                        task_id: id,
                        label: label.clone(),
                        project: project.clone().unwrap_or_default(),
                        description: None,
                    });
                    ui.close();
                }
                if ui.button("open in timeline").clicked() {
                    pending.push(Action::OpenTask(id));
                    ui.close();
                }
                if declared && open && !current && ui.button("make current").clicked() {
                    pending.push(Action::SetCurrent(id));
                    ui.close();
                }
                if open {
                    if ui.button("close").clicked() {
                        pending.push(Action::Close(id));
                        ui.close();
                    }
                } else if ui.button("reopen").clicked() {
                    pending.push(Action::Reopen(id));
                    ui.close();
                }
                merge_item(ui, id, &mut panel.merge_pick);
                if !declared && ui.button("delete").clicked() {
                    // One task, chosen by name: the bulk row keeps the
                    // two-step for the many.
                    pending.push(Action::DeleteDerived(id));
                    ui.close();
                }
            });
        });
    });
    if panel.merge_pick == Some(id) {
        let mut one: Option<Action> = None;
        if !merge_picker(ui, content_w, color, id, candidates, &mut one) {
            panel.merge_pick = None;
        }
        pending.extend(one);
    }
}

/// The inline rename: label alone, then the project (typed, or picked
/// from the configured names) and save / cancel.
fn edit_form_ui(
    ui: &mut egui::Ui,
    content_w: f32,
    panel: &mut TaskManager,
    pending: &mut Vec<Action>,
) {
    let projects = panel.projects.clone();
    let mut save = false;
    let mut cancel = false;
    {
        let e = panel.edit.as_mut().expect("checked by caller");
        ui.add(egui::TextEdit::singleline(&mut e.label).desired_width(content_w - 20.0));
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut e.project)
                    .desired_width(content_w - 190.0)
                    .hint_text("project"),
            );
            ui.menu_button("\u{25bc}", |ui| {
                for p in &projects {
                    if ui.button(p).clicked() {
                        e.project = p.clone();
                        ui.close();
                    }
                }
                ui.separator();
                if ui.button("no project").clicked() {
                    e.project.clear();
                    ui.close();
                }
            });
            if ui.button("save").clicked() {
                save = true;
            }
            if ui.button("cancel").clicked() {
                cancel = true;
            }
        });
    }
    if save && let Some(e) = panel.edit.take() {
        pending.push(Action::Rename(e));
    }
    if cancel {
        panel.edit = None;
    }
}
