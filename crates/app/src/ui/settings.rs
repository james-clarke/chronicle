//! Settings window: editable view of config.toml.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::{TimelineApp, theme};

/// Editable view of config.toml. Numbers bind directly; list/path fields are
/// edited as text and parsed on save. Saving rewrites the whole file (hand
/// comments are lost); the daemon reads config at startup, so changes apply
/// on its next restart. MCP servers live in mcp.toml and are written by the
/// Connections section as they are edited (see `connections.rs`).
pub(super) struct SettingsPanel {
    batch_minutes: u32,
    afk_close_secs: u32,
    derive_idle_secs: u32,
    retention_days: u32,
    task_autoclose_days: u32,
    background_minutes: u32,
    port: u16,
    model_path: String,
    excluded_apps: String,
    excluded_titles: String,
    /// As written in config.toml (`~` kept); edited by the Connections rows.
    git_repos: Vec<String>,
    connections: super::connections::Connections,
    /// Config as loaded (or last saved); fields without widgets pass through
    /// on save and the restart hint fires only when the file changes.
    base: chronicle_core::config::Config,
    status: Option<Result<String, String>>,
}

impl SettingsPanel {
    fn load(
        config_path: &Path,
        data_dir: &Path,
        conn: Option<&rusqlite::Connection>,
    ) -> Result<Self, String> {
        let config =
            chronicle_core::config::Config::load(config_path).map_err(|e| e.to_string())?;
        let connections = super::connections::Connections::load(config.mcp_path(data_dir), conn);
        Ok(Self {
            batch_minutes: config.batch_minutes,
            afk_close_secs: config.afk_close_secs,
            derive_idle_secs: config.derive_idle_secs,
            retention_days: config.retention_days,
            task_autoclose_days: config.task_autoclose_days,
            background_minutes: config.background_minutes,
            port: config.port,
            model_path: path_str(&config.model_path),
            excluded_apps: config.excluded_apps.join("\n"),
            excluded_titles: config.excluded_titles.join("\n"),
            git_repos: config.git_repos.clone(),
            connections,
            base: config,
            status: None,
        })
    }

    /// Ok(true) = written (daemon restart needed); Ok(false) = nothing
    /// differed from the file as loaded.
    fn save(&mut self, config_path: &Path) -> Result<bool, String> {
        let mut config = self.base.clone();
        config.batch_minutes = self.batch_minutes;
        config.afk_close_secs = self.afk_close_secs;
        config.derive_idle_secs = self.derive_idle_secs;
        config.retention_days = self.retention_days;
        config.task_autoclose_days = self.task_autoclose_days;
        config.background_minutes = self.background_minutes;
        config.port = self.port;
        config.model_path = opt_path(&self.model_path);
        config.excluded_apps = regex_lines(&self.excluded_apps)?;
        config.excluded_titles = regex_lines(&self.excluded_titles)?;
        config.git_repos = self.git_repos.clone();
        let toml = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
        let before = toml::to_string_pretty(&self.base).map_err(|e| e.to_string())?;
        if toml == before {
            return Ok(false);
        }
        std::fs::write(config_path, toml).map_err(|e| e.to_string())?;
        self.base = config;
        Ok(true)
    }
}

/// Section heading inside the settings window.
fn section(ui: &mut egui::Ui, title: &str, first: bool) {
    if !first {
        ui.add_space(12.0);
    }
    ui.label(
        egui::RichText::new(title)
            .text_style(egui::TextStyle::Heading)
            .color(theme::palette::TEXT),
    );
    ui.add_space(2.0);
}

fn path_str(p: &Option<PathBuf>) -> String {
    p.as_deref()
        .map_or_else(String::new, |p| p.display().to_string())
}

fn opt_path(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// One regex per line; each must compile so a typo can't silently disable
/// capture filtering after the next daemon restart.
fn regex_lines(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        regex::Regex::new(line).map_err(|e| format!("bad regex \"{line}\": {e}"))?;
        out.push(line.to_owned());
    }
    Ok(out)
}

impl TimelineApp {
    pub(super) fn toggle_settings(&mut self) {
        if self.settings.is_some() {
            self.settings = None;
            return;
        }
        match SettingsPanel::load(&self.config_path, &self.data_dir, self.conn.as_ref()) {
            Ok(panel) => self.settings = Some(panel),
            Err(e) => self.error = Some(format!("config load failed: {e}")),
        }
    }

    /// Full-window takeover: replaces every panel (top bar included) while
    /// `self.settings` is Some — a floating window can't fit the widget-sized
    /// default viewport.
    pub(super) fn settings_ui(&mut self, ui: &mut egui::Ui) {
        let config_path = self.config_path.clone();
        let data_dir = self.data_dir.clone();
        let model_dl = &self.model_dl;
        let mut start_dl = false;
        let mut close = false;
        let mut zoom_pick: Option<f32> = None;
        let mut spans_toggle: Option<bool> = None;
        let spans_debug_now = self.spans_debug;
        let conn = self.conn.as_ref();
        let Some(panel) = &mut self.settings else {
            return;
        };
        theme::page().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Settings")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{d7}").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    // Center the form; cap width so it stays readable when wide.
                    let max_w = ui.available_width().min(520.0);
                    let pad = ((ui.available_width() - max_w) / 2.0).max(0.0);
                    ui.horizontal(|ui| {
                        ui.add_space(pad);
                        ui.vertical(|ui| {
                            ui.set_max_width(max_w);
                            section(ui, "Connections", true);
                            panel.connections.ui(ui, conn, &mut panel.git_repos);

                            section(ui, "Capture", false);
                            egui::Grid::new("settings_capture")
                                .num_columns(3)
                                .show(ui, |ui| {
                                    ui.label("afk close");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.afk_close_secs)
                                            .range(30..=3600),
                                    );
                                    ui.weak("secs");
                                    ui.end_row();
                                });
                            ui.label("excluded apps (one regex per line, never stored)");
                            ui.add(
                                egui::TextEdit::multiline(&mut panel.excluded_apps)
                                    .desired_rows(2)
                                    .font(egui::TextStyle::Monospace),
                            );
                            ui.label("excluded titles (one regex per line)");
                            ui.add(
                                egui::TextEdit::multiline(&mut panel.excluded_titles)
                                    .desired_rows(2)
                                    .font(egui::TextStyle::Monospace),
                            );

                            section(ui, "Derivation", false);
                            egui::Grid::new("settings_derive")
                                .num_columns(3)
                                .show(ui, |ui| {
                                    ui.label("batch every");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.batch_minutes)
                                            .range(5..=240),
                                    );
                                    ui.weak("minutes");
                                    ui.end_row();
                                    ui.label("derive after idle");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.derive_idle_secs)
                                            .range(60..=3600),
                                    );
                                    ui.weak("secs");
                                    ui.end_row();
                                    ui.label("task autoclose");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.task_autoclose_days)
                                            .range(0..=365),
                                    );
                                    ui.weak("days (0 = never)");
                                    ui.end_row();
                                    ui.label("background under");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.background_minutes)
                                            .range(0..=120),
                                    );
                                    ui.weak("minutes (0 = off)");
                                    ui.end_row();
                                });

                            section(ui, "Model", false);
                            ui.label("model path (empty = default preset)");
                            ui.text_edit_singleline(&mut panel.model_path);
                            match chronicle_derive::model::resolve(
                                opt_path(&panel.model_path).as_deref(),
                                &data_dir,
                            ) {
                                Some(p) => {
                                    ui.weak(format!("using {}", p.display()));
                                }
                                None => match model_dl {
                                    Some(dl) if dl.finished.is_none() => {
                                        super::onboarding::progress_ui(ui, dl);
                                    }
                                    _ => {
                                        ui.horizontal(|ui| {
                                            ui.colored_label(
                                                theme::palette::AMBER,
                                                "no model downloaded",
                                            );
                                            if ui.small_button("download").clicked() {
                                                start_dl = true;
                                            }
                                        });
                                    }
                                },
                            }

                            section(ui, "Storage & server", false);
                            egui::Grid::new("settings_storage")
                                .num_columns(3)
                                .show(ui, |ui| {
                                    ui.label("retention");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.retention_days)
                                            .range(0..=3650),
                                    );
                                    ui.weak("days (0 = keep forever)");
                                    ui.end_row();
                                    ui.label("aw endpoint port");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.port).range(1024..=65535),
                                    );
                                    ui.label("");
                                    ui.end_row();
                                });

                            // UI-only prefs: applied immediately, stored in
                            // db meta (not config.toml), no daemon restart.
                            section(ui, "Appearance", false);
                            ui.horizontal(|ui| {
                                ui.label("ui scale");
                                for (label, z) in
                                    [("compact", 0.9f32), ("default", 1.0), ("comfortable", 1.15)]
                                {
                                    let active = (ui.ctx().zoom_factor() - z).abs() < 0.01;
                                    if theme::selectable(ui, active, label).clicked() && !active {
                                        ui.ctx().set_zoom_factor(z);
                                        zoom_pick = Some(z);
                                    }
                                }
                            });
                            ui.weak("Ctrl +/\u{2212}/0 also works anywhere");
                            let mut dbg = spans_debug_now;
                            if ui.checkbox(&mut dbg, "show raw spans on home").changed() {
                                spans_toggle = Some(dbg);
                            }

                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                if theme::primary_button(ui, "save").clicked() {
                                    panel.status = Some(match panel.save(&config_path) {
                                        Ok(true) => {
                                            Ok("saved \u{2014} restart daemon to apply".into())
                                        }
                                        Ok(false) => Ok("no changes".into()),
                                        Err(e) => Err(e),
                                    });
                                }
                                match &panel.status {
                                    Some(Ok(msg)) => {
                                        ui.weak(msg.as_str());
                                    }
                                    Some(Err(msg)) => {
                                        egui::Frame::new()
                                            .fill(theme::palette::RED.gamma_multiply(0.15))
                                            .corner_radius(egui::CornerRadius::same(6))
                                            .inner_margin(egui::Margin::same(6))
                                            .show(ui, |ui| {
                                                ui.colored_label(theme::palette::RED, msg);
                                            });
                                    }
                                    None => {}
                                }
                            });
                        });
                    });
                });
        });
        if close {
            self.settings = None;
        }
        if let (Some(z), Some(conn)) = (zoom_pick, self.conn.as_ref()) {
            let _ =
                chronicle_core::storage::set_meta(conn, "ui_zoom_factor", Some(&format!("{z:.2}")));
        }
        if let (Some(on), Some(conn)) = (spans_toggle, self.conn.as_ref()) {
            let value = on.then_some("1");
            let _ = chronicle_core::storage::set_meta(conn, "ui_show_spans_debug", value);
            self.spans_debug = on;
        }
        if start_dl {
            self.start_model_download(ui.ctx(), chronicle_derive::model::default_preset());
        }
    }
}
