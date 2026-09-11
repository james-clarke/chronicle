//! The Setup view (m41 chunk 2): the first five minutes. One pass over the
//! registry sorts every supported connector into *already working*, *one
//! step each* and *needs an account*; the steps render inline — a switch
//! that writes config.toml, a command with a copy button, a field with a
//! scan behind it — and "check" probes the row again on the spot. Shown
//! once on a fresh profile, again from Settings › Connections or
//! `chronicle setup`. Skipping is one click and is remembered in `meta`.

use std::collections::HashMap;

use eframe::egui;
use jiff::Timestamp;

use chronicle_core::config::Config;
use chronicle_core::connectors::SetupStep;
use chronicle_core::health::{self, Env, Health};
use chronicle_core::project::DiscoveredRepo;
use chronicle_core::setup::{self, Group, Item};

use super::connections::subhead;
use super::theme::{self, palette};
use super::timeline::ago;
use super::{TimelineApp, View};

/// `meta` key: "done" or "skipped" once the view has been left on purpose.
pub(super) const SEEN_KEY: &str = "setup_seen";

pub(super) struct SetupState {
    /// config.toml as loaded, then as edited here (each change is written).
    config: Config,
    env: Env,
    plan: Vec<Item>,
    shell: &'static str,
    /// Text typed into a list field's entry box, by field.
    drafts: HashMap<&'static str, String>,
    /// "find my repos" results with their ticks; None until asked.
    scan: Option<Vec<(DiscoveredRepo, bool)>>,
    status: Option<Result<String, String>>,
}

impl SetupState {
    fn replan(&mut self, conn: Option<&rusqlite::Connection>) {
        let now_ms = Timestamp::now().as_millisecond();
        self.plan = setup::plan(conn, &self.config, &self.env, now_ms);
    }
}

impl TimelineApp {
    /// Open the view fresh: Settings' "run setup again" and the first run.
    pub(super) fn open_setup(&mut self) {
        self.settings = None;
        self.setup = None;
        self.view = View::Setup;
    }

    fn load_setup(&self) -> SetupState {
        let config = Config::load(&self.config_path).unwrap_or_default();
        // `daemon_up` false for the same reason Settings passes it: whether
        // the daemon answers is one fact about the product, not a row.
        let env = Env::host(false);
        let mut st = SetupState {
            config,
            env,
            plan: Vec::new(),
            shell: setup::shell(),
            drafts: HashMap::new(),
            scan: None,
            status: None,
        };
        st.replan(self.conn.as_ref());
        st
    }

    pub(super) fn setup_ui(&mut self, ui: &mut egui::Ui) {
        let mut st = self.setup.take().unwrap_or_else(|| self.load_setup());
        let mut leave: Option<&'static str> = None;
        theme::page().show(ui, |ui| {
            let width = theme::content_width(ui);
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.set_width(width);
                    self.model_card_ui(ui);
                    let conn = self.conn.as_ref();
                    ui.label(
                        egui::RichText::new("Set up Chronicle")
                            .text_style(egui::TextStyle::Heading)
                            .color(palette::TEXT),
                    );
                    dim(
                        ui,
                        "Chronicle reads your tools where they already are, on this \
                         machine. Switch on what you use; the daemon reads config.toml \
                         when it starts.",
                    );
                    ui.add_space(theme::SPACE_SM);

                    let mut save = false;
                    let mut recheck = false;
                    // Steps first: what already works is the reassurance,
                    // not the reason the view is open.
                    for group in [Group::OneStep, Group::Account, Group::Working] {
                        let ids: Vec<usize> = (0..st.plan.len())
                            .filter(|&i| st.plan[i].group == group)
                            .collect();
                        if ids.is_empty() {
                            continue;
                        }
                        ui.add_space(theme::SPACE_SM);
                        let n = ids.len().to_string();
                        subhead(ui, group.label(), |ui| dim(ui, &n));
                        for i in ids {
                            let (c, h) = (st.plan[i].connector, st.plan[i].health.clone());
                            let (dot, chip, color) = match &h {
                                Health::Broken { reason } => {
                                    (palette::RED, reason.clone(), palette::RED)
                                }
                                Health::Working { last_seen_ms, .. } => {
                                    (palette::GREEN, ago(*last_seen_ms), palette::TEXT_DIM)
                                }
                                Health::Connected => (
                                    palette::AMBER,
                                    "nothing seen yet".to_owned(),
                                    palette::AMBER,
                                ),
                                Health::Found | Health::Absent => (
                                    palette::TEXT_DIM,
                                    health::label(c, &h).to_owned(),
                                    palette::TEXT_DIM,
                                ),
                            };
                            let mut row = theme::ListRow::new(c.name)
                                .emphasis()
                                .dot(dot)
                                .chip(chip, color);
                            if group != Group::Working {
                                row = row.subtitle(c.blurb);
                            }
                            row.show(ui, width, |ui| {
                                if group != Group::Working
                                    && theme::ghost_button(ui, "check")
                                        .on_hover_text("probe this row again")
                                        .clicked()
                                {
                                    recheck = true;
                                }
                            });
                            if group == Group::Working {
                                continue;
                            }
                            for step in c.setup {
                                step_ui(ui, width, step, &mut st, &mut save);
                            }
                            ui.add_space(theme::SPACE_XS);
                        }
                    }

                    ui.add_space(theme::SPACE_LG);
                    ui.horizontal(|ui| {
                        if theme::primary_button(ui, "done").clicked() {
                            leave = Some("done");
                        }
                        if theme::ghost_button(ui, "skip for now").clicked() {
                            leave = Some("skipped");
                        }
                        match &st.status {
                            Some(Ok(msg)) => {
                                ui.weak(msg.as_str());
                            }
                            Some(Err(msg)) => {
                                ui.colored_label(palette::RED, msg.as_str());
                            }
                            None => {}
                        }
                    });
                    ui.add_space(theme::SPACE_SM);

                    if save {
                        st.status = Some(match st.config.save(&self.config_path) {
                            Ok(()) => {
                                Ok("saved \u{2014} the daemon reads it on its next start".into())
                            }
                            Err(e) => Err(e.to_string()),
                        });
                        recheck = true;
                    }
                    if recheck {
                        st.replan(conn);
                    }
                });
        });
        match leave {
            Some(how) => {
                if let Some(conn) = self.conn.as_ref() {
                    let _ = chronicle_core::storage::set_meta(conn, SEEN_KEY, Some(how));
                }
                self.view = View::Home;
                self.loaded_at = None;
            }
            None => self.setup = Some(st),
        }
    }
}

/// One step, indented under its row. `save` is set when config changed.
fn step_ui(ui: &mut egui::Ui, width: f32, step: &SetupStep, st: &mut SetupState, save: &mut bool) {
    const INDENT: f32 = 16.0;
    match *step {
        SetupStep::Toggle { field } => {
            let Some(mut on) = flag(&st.config, field) else {
                return;
            };
            ui.horizontal(|ui| {
                ui.add_space(INDENT);
                if theme::toggle(ui, &mut on).changed() {
                    set_flag(&mut st.config, field, on);
                    *save = true;
                }
                dim(ui, &format!("{field} in config.toml"));
            });
        }
        SetupStep::Command { run } => {
            let line = setup::paste_line(run, st.shell);
            ui.horizontal(|ui| {
                ui.add_space(INDENT);
                ui.label(egui::RichText::new(&line).monospace().color(palette::TEXT));
                if theme::ghost_button(ui, theme::glyph(theme::icon::COPY))
                    .on_hover_text("copy")
                    .clicked()
                {
                    ui.ctx().copy_text(line.clone());
                }
            });
            if run.starts_with("chronicle shell-init") {
                ui.horizontal(|ui| {
                    ui.add_space(INDENT);
                    dim(ui, &format!("add it to {}", setup::rc_file(st.shell)));
                });
            }
        }
        SetupStep::Field { field, hint } => {
            let Some(count) = list(&st.config, field).map(|v| v.len()) else {
                return;
            };
            if count > 0 {
                ui.horizontal(|ui| {
                    ui.add_space(INDENT);
                    dim(ui, &format!("{count} configured"));
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(INDENT);
                let draft = st.drafts.entry(field).or_default();
                let edit = egui::TextEdit::singleline(draft)
                    .hint_text(hint)
                    .desired_width(width - INDENT - 120.0);
                let submitted =
                    ui.add(edit).lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let add = theme::secondary_button(ui, "add").clicked() || submitted;
                if add && !draft.trim().is_empty() {
                    let value = draft.trim().to_owned();
                    draft.clear();
                    if let Some(v) = list_mut(&mut st.config, field)
                        && !v.contains(&value)
                    {
                        v.push(value);
                        *save = true;
                    }
                }
            });
            if field == "git_repos" {
                repo_scan_ui(ui, st, save);
            }
        }
        SetupStep::Install { url } => {
            ui.horizontal(|ui| {
                ui.add_space(INDENT);
                ui.hyperlink_to("install it", url);
                dim(ui, url);
            });
        }
        SetupStep::Account { flow } => {
            let line = format!("chronicle {flow}");
            ui.horizontal(|ui| {
                ui.add_space(INDENT);
                ui.label(egui::RichText::new(&line).monospace().color(palette::TEXT));
                if theme::ghost_button(ui, theme::glyph(theme::icon::COPY))
                    .on_hover_text("copy")
                    .clicked()
                {
                    ui.ctx().copy_text(line.clone());
                }
            });
        }
    }
}

/// "find my repos": the usual parent folders, each hit a tick, one button
/// to add the ticked ones — the fresh-profile route to `git_repos` with no
/// editor open.
fn repo_scan_ui(ui: &mut egui::Ui, st: &mut SetupState, save: &mut bool) {
    const INDENT: f32 = 16.0;
    let home = st.env.home.clone();
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        if theme::secondary_button(ui, "find my repos")
            .on_hover_text(format!(
                "git repos under ~/{}",
                setup::REPO_PARENTS.join(", ~/")
            ))
            .clicked()
        {
            let found = setup::scan_repos(&home, &st.config.git_repos);
            st.scan = Some(found.into_iter().map(|r| (r, true)).collect());
        }
    });
    let Some(scan) = st.scan.as_mut() else {
        return;
    };
    if scan.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(INDENT);
            dim(ui, "no new repos under the usual folders; add a path above");
        });
        return;
    }
    for (repo, tick) in scan.iter_mut() {
        ui.horizontal(|ui| {
            ui.add_space(INDENT);
            ui.checkbox(tick, &repo.name);
            dim(ui, &tilde(&repo.path, &home));
        });
    }
    let picked: Vec<String> = scan
        .iter()
        .filter(|(_, tick)| *tick)
        .map(|(r, _)| tilde(&r.path, &home))
        .collect();
    ui.horizontal(|ui| {
        ui.add_space(INDENT);
        let label = match picked.len() {
            1 => "add 1 repo".to_owned(),
            n => format!("add {n} repos"),
        };
        if theme::primary_button_enabled(ui, !picked.is_empty(), &label).clicked() {
            for p in picked {
                if !st.config.git_repos.contains(&p) {
                    st.config.git_repos.push(p);
                }
            }
            st.scan = None;
            *save = true;
        }
    });
}

/// `~/…` for a path under home, the way config.toml keeps them.
fn tilde(path: &std::path::Path, home: &std::path::Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn dim(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .text_style(theme::caption())
            .color(palette::TEXT_DIM),
    );
}

/// The bool behind a `Toggle` step. `ai_session_dirs` is a list in the
/// config; on means the default directories, as Settings does it.
fn flag(cfg: &Config, field: &str) -> Option<bool> {
    Some(match field {
        "ai_session_dirs" => !cfg.ai_session_dirs.is_empty(),
        "github_prs" => cfg.github_prs,
        "gitlab_mrs" => cfg.gitlab_mrs,
        "mic_capture" => cfg.mic_capture,
        "google_calendar" => cfg.google_calendar,
        "shell_history" => cfg.shell_history,
        "editor_heartbeats" => cfg.editor_heartbeats,
        "shell_hook" => cfg.shell_hook,
        "discover_repos" => cfg.discover_repos,
        "browser_history" => cfg.browser_history,
        _ => return None,
    })
}

fn set_flag(cfg: &mut Config, field: &str, on: bool) {
    match field {
        "ai_session_dirs" => {
            cfg.ai_session_dirs = if on {
                Config::default().ai_session_dirs
            } else {
                Vec::new()
            }
        }
        "github_prs" => cfg.github_prs = on,
        "gitlab_mrs" => cfg.gitlab_mrs = on,
        "mic_capture" => cfg.mic_capture = on,
        "google_calendar" => cfg.google_calendar = on,
        "shell_history" => cfg.shell_history = on,
        "editor_heartbeats" => cfg.editor_heartbeats = on,
        "shell_hook" => cfg.shell_hook = on,
        "discover_repos" => cfg.discover_repos = on,
        "browser_history" => cfg.browser_history = on,
        _ => {}
    }
}

/// The list behind a `Field` step.
fn list<'a>(cfg: &'a Config, field: &str) -> Option<&'a Vec<String>> {
    Some(match field {
        "git_repos" => &cfg.git_repos,
        "calendars" => &cfg.calendars,
        "ai_session_dirs" => &cfg.ai_session_dirs,
        _ => return None,
    })
}

fn list_mut<'a>(cfg: &'a mut Config, field: &str) -> Option<&'a mut Vec<String>> {
    Some(match field {
        "git_repos" => &mut cfg.git_repos,
        "calendars" => &mut cfg.calendars,
        "ai_session_dirs" => &mut cfg.ai_session_dirs,
        _ => return None,
    })
}
