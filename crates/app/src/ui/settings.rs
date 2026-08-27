//! Settings window: editable view of config.toml.

use std::path::{Path, PathBuf};

use eframe::egui;

use super::TimelineApp;

/// Editable view of config.toml. Numbers bind directly; list/path fields are
/// edited as text and parsed on save. Saving rewrites the whole file (hand
/// comments are lost); the daemon reads config at startup, so changes apply
/// on its next restart.
pub(super) struct SettingsPanel {
    batch_minutes: u32,
    afk_close_secs: u32,
    derive_idle_secs: u32,
    retention_days: u32,
    task_autoclose_days: u32,
    port: u16,
    model_path: String,
    mcp_config: String,
    excluded_apps: String,
    excluded_titles: String,
    /// Config as loaded; fields without widgets pass through on save.
    base: chronicle_core::config::Config,
    status: Option<Result<String, String>>,
}

impl SettingsPanel {
    fn load(config_path: &Path) -> Result<Self, String> {
        let config =
            chronicle_core::config::Config::load(config_path).map_err(|e| e.to_string())?;
        Ok(Self {
            batch_minutes: config.batch_minutes,
            afk_close_secs: config.afk_close_secs,
            derive_idle_secs: config.derive_idle_secs,
            retention_days: config.retention_days,
            task_autoclose_days: config.task_autoclose_days,
            port: config.port,
            model_path: path_str(&config.model_path),
            mcp_config: path_str(&config.mcp_config),
            excluded_apps: config.excluded_apps.join("\n"),
            excluded_titles: config.excluded_titles.join("\n"),
            base: config,
            status: None,
        })
    }

    fn save(&self, config_path: &Path) -> Result<(), String> {
        let mut config = self.base.clone();
        config.batch_minutes = self.batch_minutes;
        config.afk_close_secs = self.afk_close_secs;
        config.derive_idle_secs = self.derive_idle_secs;
        config.retention_days = self.retention_days;
        config.task_autoclose_days = self.task_autoclose_days;
        config.port = self.port;
        config.model_path = opt_path(&self.model_path);
        config.mcp_config = opt_path(&self.mcp_config);
        config.excluded_apps = regex_lines(&self.excluded_apps)?;
        config.excluded_titles = regex_lines(&self.excluded_titles)?;
        let toml = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
        std::fs::write(config_path, toml).map_err(|e| e.to_string())
    }
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
        match SettingsPanel::load(&self.config_path) {
            Ok(panel) => self.settings = Some(panel),
            Err(e) => self.error = Some(format!("config load failed: {e}")),
        }
    }

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        let config_path = self.config_path.clone();
        let Some(panel) = &mut self.settings else {
            return;
        };
        let mut open = true;
        egui::Window::new("settings")
            .open(&mut open)
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                egui::Grid::new("settings_nums")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("batch minutes");
                        ui.add(egui::DragValue::new(&mut panel.batch_minutes).range(5..=240));
                        ui.end_row();
                        ui.label("afk close secs");
                        ui.add(egui::DragValue::new(&mut panel.afk_close_secs).range(30..=3600));
                        ui.end_row();
                        ui.label("derive idle secs");
                        ui.add(egui::DragValue::new(&mut panel.derive_idle_secs).range(60..=3600));
                        ui.end_row();
                        ui.label("retention days (0 = keep forever)");
                        ui.add(egui::DragValue::new(&mut panel.retention_days).range(0..=3650));
                        ui.end_row();
                        ui.label("task autoclose days (0 = never)");
                        ui.add(egui::DragValue::new(&mut panel.task_autoclose_days).range(0..=365));
                        ui.end_row();
                        ui.label("aw endpoint port");
                        ui.add(egui::DragValue::new(&mut panel.port).range(1024..=65535));
                        ui.end_row();
                    });
                ui.separator();
                ui.label("model path (empty = default preset)");
                ui.text_edit_singleline(&mut panel.model_path);
                ui.label("mcp config path (empty = mcp.toml in data dir)");
                ui.text_edit_singleline(&mut panel.mcp_config);
                ui.label("excluded apps (one regex per line, never stored)");
                ui.add(egui::TextEdit::multiline(&mut panel.excluded_apps).desired_rows(2));
                ui.label("excluded titles (one regex per line)");
                ui.add(egui::TextEdit::multiline(&mut panel.excluded_titles).desired_rows(2));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("save").clicked() {
                        panel.status = Some(match panel.save(&config_path) {
                            Ok(()) => Ok("saved \u{2014} restart daemon to apply".into()),
                            Err(e) => Err(e),
                        });
                    }
                    match &panel.status {
                        Some(Ok(msg)) => {
                            ui.weak(msg.as_str());
                        }
                        Some(Err(msg)) => {
                            ui.colored_label(ui.visuals().error_fg_color, msg);
                        }
                        None => {}
                    }
                });
            });
        if !open {
            self.settings = None;
        }
    }
}
