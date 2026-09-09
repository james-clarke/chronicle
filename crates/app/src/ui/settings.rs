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
    quiet_secs: u32,
    away_secs: u32,
    capture_presence: bool,
    derive_idle_secs: u32,
    /// `derive_mode == "segmenter"` (m30 chunk 3).
    segmenter: bool,
    retention_days: u32,
    task_autoclose_days: u32,
    background_minutes: u32,
    port: u16,
    model_path: String,
    excluded_apps: String,
    excluded_titles: String,
    distraction_patterns: String,
    checkpoint_afk_secs: u32,
    /// As written in config.toml (`~` kept); edited by the Connections rows.
    git_repos: Vec<String>,
    /// Projects and their rules (m35 chunk 0).
    projects: super::projects::ProjectsPanel,
    sources: super::connections::LocalSources,
    connections: super::connections::Connections,
    /// Cloud backends + routing (m31 c7); re-read from `models.toml` on
    /// every save so the file stays the truth.
    cloud: super::cloud::CloudPanel,
    /// Config as loaded (or last saved); fields without widgets pass through
    /// on save and the restart hint fires only when the file changes.
    base: chronicle_core::config::Config,
    status: Option<Result<String, String>>,
    /// Inspector views (m27 chunk 7): the tail digest as the worker would
    /// build it, and the last raw model output.
    digest_view: Option<String>,
    output_view: Option<String>,
    /// Today's outbound counts (meta `fetches:<date>` / `posts:<date>`) for
    /// the "what leaves this machine" line; read once, when the panel opens.
    fetches_today: i64,
    posts_today: i64,
    /// Today's cloud job usage by kind/backend (m31 c7), for the same line;
    /// read once, when the panel opens.
    ai_usage_today: Vec<chronicle_core::storage::AiJobUsageRow>,
}

/// What the Pipeline card shows: the daemon's own view plus what the DB
/// holds about the last runs. Loaded on the reload cadence while Settings
/// is open (the socket query has a 1 s timeout).
#[derive(Debug, Clone)]
pub(super) struct PipelineInfo {
    pub daemon: Option<crate::DaemonStatus>,
    pub model: String,
    pub last_derive: Option<chronicle_core::storage::DeriveMetrics>,
    pub pending: i64,
    pub last_live: Option<(i64, String)>,
    /// `(start_ts, end_ts, label, reason)` in the tail.
    pub placements: Vec<(i64, i64, String, String)>,
    pub last_batch: Option<i64>,
    /// The daemon's daily self-score rows, oldest first (m32 chunk 6).
    pub scores: Vec<chronicle_core::storage::SelfScore>,
}

impl PipelineInfo {
    pub(super) fn load(
        conn: &rusqlite::Connection,
        data_dir: &Path,
        sock: &Path,
        config: &chronicle_core::config::Config,
    ) -> Self {
        use chronicle_core::storage;
        // Short timeout: this runs on the UI thread every reload.
        let daemon = match crate::query_daemon_within(sock, std::time::Duration::from_millis(200)) {
            crate::Liveness::Running(s) => Some(s),
            _ => None,
        };
        let model = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
            .map(|p| {
                let file = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                match chronicle_derive::model::PRESETS
                    .iter()
                    .find(|m| m.file == file)
                {
                    Some(m) => format!("{file} ({})", m.name),
                    None => file,
                }
            })
            .unwrap_or_else(|| "not downloaded".to_owned());
        let now = jiff::Timestamp::now().as_millisecond();
        let tail_lo = storage::latest_batch_end(conn)
            .ok()
            .flatten()
            .unwrap_or(now - 12 * 3_600_000);
        Self {
            daemon,
            model,
            last_derive: storage::last_derive(conn).ok().flatten(),
            pending: storage::pending_batch_count(conn).unwrap_or(0),
            last_live: storage::last_live_interval(conn).ok().flatten(),
            placements: storage::tail_placements(conn, tail_lo, now).unwrap_or_default(),
            last_batch: storage::get_meta(conn, "derive_last_batch")
                .ok()
                .flatten()
                .and_then(|v| v.parse().ok()),
            scores: storage::self_scores(conn, chronicle_core::self_score::DAYS)
                .unwrap_or_default(),
        }
    }
}

/// Settings › Derivation › Self-score (m32 chunk 6): one row per day as the
/// daemon scored it, then the week folded into the lines `chronicle status`
/// prints. Rates are computed here; the rows hold counts.
fn self_score_card(ui: &mut egui::Ui, rows: &[chronicle_core::storage::SelfScore]) {
    use chronicle_core::self_score::{Summary, fmt_ms, pct};
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Self-score")
            .strong()
            .color(theme::palette::TEXT),
    );
    ui.weak("per day: placed share of active time, missing (not captured / not derived), tasks minted and merged within a day, ejects over placements, wrong over closed verdicts (the confident pair after); scored once a day");
    if rows.is_empty() {
        ui.weak("no rows yet (the daemon scores the week on its first tick of the day)");
        return;
    }
    // Inside its own horizontal scroll so a wide week cannot widen the
    // sections below it (an over-wide row widens every sibling's column).
    egui::ScrollArea::horizontal()
        .id_salt("settings_self_score_scroll")
        .show(ui, |ui| {
            egui::Grid::new("settings_self_score")
                .num_columns(6)
                .spacing([8.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    for head in [
                        "day",
                        "placed",
                        "missing",
                        "minted/merged",
                        "ejects/placed",
                        "wrong/closed",
                    ] {
                        ui.weak(head);
                    }
                    ui.end_row();
                    for r in rows {
                        ui.label(&r.day[5..]);
                        ui.label(theme::num(format!(
                            "{} of {}",
                            pct(r.placed_ms.min(r.active_ms), r.active_ms),
                            fmt_ms(r.active_ms)
                        )));
                        ui.label(theme::num(format!(
                            "{}/{}",
                            fmt_ms(r.uncaptured_ms),
                            fmt_ms(r.underived_ms)
                        )));
                        ui.label(theme::num(format!("{}/{}", r.minted, r.merged)));
                        ui.label(theme::num(format!("{}/{}", r.ejects, r.placements)));
                        ui.label(theme::num(format!(
                            "{}/{} ({}/{})",
                            r.wrong, r.verdicts, r.confident_wrong, r.confident
                        )));
                        ui.end_row();
                    }
                });
        });
    for line in Summary::of(rows).lines() {
        ui.weak(line);
    }
}

impl SettingsPanel {
    /// The config as loaded or last saved (for the Pipeline card).
    pub(super) fn config(&self) -> &chronicle_core::config::Config {
        &self.base
    }

    fn load(
        config_path: &Path,
        data_dir: &Path,
        conn: Option<&rusqlite::Connection>,
    ) -> Result<Self, String> {
        let config =
            chronicle_core::config::Config::load(config_path).map_err(|e| e.to_string())?;
        let connections = super::connections::Connections::load(
            config.mcp_path(data_dir),
            data_dir,
            conn,
            &config.git_repos,
            &config.ai_session_dirs,
        );
        let counter = |prefix: &str| -> i64 {
            conn.and_then(|c| {
                chronicle_core::storage::get_meta(c, &crate::day_counter_key(prefix))
                    .ok()
                    .flatten()
            })
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
        };
        Ok(Self {
            batch_minutes: config.batch_minutes,
            afk_close_secs: config.afk_close_secs,
            quiet_secs: config.quiet_secs,
            away_secs: config.away_secs,
            capture_presence: config.capture_presence,
            derive_idle_secs: config.derive_idle_secs,
            segmenter: config.derive_mode == "segmenter",
            retention_days: config.retention_days,
            task_autoclose_days: config.task_autoclose_days,
            background_minutes: config.background_minutes,
            port: config.port,
            model_path: path_str(&config.model_path),
            excluded_apps: config.excluded_apps.join("\n"),
            excluded_titles: config.excluded_titles.join("\n"),
            distraction_patterns: config.distraction_patterns.join("\n"),
            checkpoint_afk_secs: config.checkpoint_afk_secs,
            git_repos: config.git_repos.clone(),
            projects: super::projects::ProjectsPanel::from_config(&config),
            digest_view: None,
            output_view: None,
            sources: super::connections::LocalSources::from_config(&config),
            connections,
            cloud: super::cloud::CloudPanel::load(data_dir, conn),
            base: config,
            status: None,
            fetches_today: counter("fetches"),
            posts_today: counter("posts"),
            ai_usage_today: conn
                .and_then(|c| {
                    chronicle_core::storage::ai_jobs_today_by_backend(
                        c,
                        crate::ai_job::day_start_ms(),
                    )
                    .ok()
                })
                .unwrap_or_default(),
        })
    }

    /// Ok(true) = written (daemon restart needed); Ok(false) = nothing
    /// differed from the file as loaded.
    fn save(&mut self, config_path: &Path) -> Result<bool, String> {
        let mut config = self.base.clone();
        config.batch_minutes = self.batch_minutes;
        config.afk_close_secs = self.afk_close_secs;
        config.quiet_secs = self.quiet_secs;
        config.away_secs = self.away_secs;
        config.capture_presence = self.capture_presence;
        config.derive_idle_secs = self.derive_idle_secs;
        config.derive_mode = if self.segmenter { "segmenter" } else { "model" }.to_owned();
        config.retention_days = self.retention_days;
        config.task_autoclose_days = self.task_autoclose_days;
        config.background_minutes = self.background_minutes;
        config.port = self.port;
        config.model_path = opt_path(&self.model_path);
        config.excluded_apps = regex_lines(&self.excluded_apps)?;
        config.excluded_titles = regex_lines(&self.excluded_titles)?;
        config.distraction_patterns = regex_lines(&self.distraction_patterns)?;
        config.checkpoint_afk_secs = self.checkpoint_afk_secs;
        config.git_repos = self.git_repos.clone();
        self.sources.apply(&mut config);
        self.projects.apply(&mut config)?;
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
/// Switch + label on one row; true when the switch flipped this frame.
fn switch_row(ui: &mut egui::Ui, on: &mut bool, label: &str) -> bool {
    ui.horizontal(|ui| {
        let changed = theme::toggle(ui, on).changed();
        ui.label(label);
        changed
    })
    .inner
}

/// Section titles in form order; the wide-window index lists them.
const SECTIONS: &[&str] = &[
    "Connections",
    "Projects",
    "Model",
    "Capture",
    "Derivation",
    "Standup & journal",
    "Storage & server",
    "Window & appearance",
];
const SECTION_INDEX_W: f32 = 132.0;

/// Settings › Derivation › Pipeline (m27 chunk 7): what the worker is doing
/// and what it last saw, without opening a log. Two debugging views on
/// demand: the tail digest as the worker would build it, and the last raw
/// model output.
fn pipeline_card(
    ui: &mut egui::Ui,
    info: Option<&PipelineInfo>,
    tz: &jiff::tz::TimeZone,
    panel: &mut SettingsPanel,
    conn: Option<&rusqlite::Connection>,
    data_dir: &Path,
) {
    let hm = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Pipeline")
            .strong()
            .color(theme::palette::TEXT),
    );
    ui.weak("what the derivation worker is doing and what it last saw");
    let Some(info) = info else {
        ui.weak("loading\u{2026}");
        return;
    };
    egui::Grid::new("settings_pipeline")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.weak("model");
            ui.label(&info.model);
            ui.end_row();
            ui.weak("worker");
            let worker = match &info.daemon {
                None => "daemon not running".to_owned(),
                Some(d) => match (&d.worker, d.worker_secs) {
                    (Some(w), Some(s)) => format!("{w} ({s}s)"),
                    (Some(w), None) => w.clone(),
                    (None, _) if d.model_resident => "idle, model resident".to_owned(),
                    (None, _) => "idle".to_owned(),
                },
            };
            ui.label(worker);
            ui.end_row();
            ui.weak("pending batches");
            ui.label(theme::num(info.pending.to_string()));
            ui.end_row();
            ui.weak("last derive");
            match &info.last_derive {
                Some(d) => ui.label(format!(
                    "batch {} at {}, {:.1}s, {} prompt + {} gen tokens",
                    d.batch_id,
                    hm(d.derived_ts),
                    d.derive_ms as f64 / 1000.0,
                    d.prompt_tokens,
                    d.gen_tokens
                )),
                None => ui.weak("none yet"),
            };
            ui.end_row();
            ui.weak("last live pass");
            match &info.last_live {
                Some((end, label)) => ui.label(format!("{} \u{2192} {label}", hm(*end))),
                None => ui.weak("none yet"),
            };
            ui.end_row();
        });
    if !info.placements.is_empty() {
        ui.weak("pre-pass placements in the tail");
        for (lo, hi, label, reason) in &info.placements {
            ui.label(format!(
                "{}\u{2013}{} \u{2192} {label} ({reason})",
                hm(*lo),
                hm(*hi)
            ));
        }
    }
    ui.horizontal(|ui| {
        if theme::ghost_button(ui, "show digest")
            .on_hover_text("the tail digest as the worker would build it now (no MCP fetch)")
            .clicked()
        {
            panel.digest_view = match panel.digest_view.take() {
                Some(_) => None,
                None => Some(match conn {
                    Some(c) => crate::tail_digest(c, &panel.base, data_dir)
                        .unwrap_or_else(|e| format!("digest failed: {e:#}")),
                    None => "(no database)".to_owned(),
                }),
            };
        }
        if theme::ghost_button(ui, "last output")
            .on_hover_text("the model's last raw batch answer, before linking")
            .clicked()
        {
            panel.output_view = match panel.output_view.take() {
                Some(_) => None,
                None => Some(
                    conn.and_then(|c| {
                        chronicle_core::storage::get_meta(c, "derive_last_output")
                            .ok()
                            .flatten()
                    })
                    .map(|raw| match info.last_batch {
                        Some(b) => format!("batch {b}\n{raw}"),
                        None => raw,
                    })
                    .unwrap_or_else(|| "(no derive since the worker was instrumented)".to_owned()),
                ),
            };
        }
        ui.weak("debugging views");
    });
    for (id, text) in [
        ("digest", &mut panel.digest_view),
        ("output", &mut panel.output_view),
    ] {
        if let Some(text) = text {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .max_height(260.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
        }
    }
}

/// Section heading; scrolls itself to the top when it is the `jump` target.
fn section(ui: &mut egui::Ui, title: &str, first: bool, jump: Option<&str>) {
    if !first {
        ui.add_space(12.0);
    }
    let resp = ui.label(
        egui::RichText::new(title)
            .text_style(egui::TextStyle::Heading)
            .color(theme::palette::TEXT),
    );
    if jump == Some(title) {
        ui.scroll_to_rect(
            resp.rect.expand2(egui::vec2(0.0, 12.0)),
            Some(egui::Align::Min),
        );
    }
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

/// "what leaves this machine today" (m31 c7): the model download mention,
/// then today's cloud job usage grouped by backend ("14 journal + 3 chat to
/// anthropic (claude-opus-5)"), then the existing MCP counts, then "nothing
/// else" when no cloud rows.
fn egress_line(
    usage: &[chronicle_core::storage::AiJobUsageRow],
    cfg: &chronicle_core::models_config::ModelsConfig,
    fetches_today: i64,
    posts_today: i64,
) -> String {
    let mut by_backend: std::collections::BTreeMap<
        &str,
        Vec<&chronicle_core::storage::AiJobUsageRow>,
    > = std::collections::BTreeMap::new();
    for row in usage {
        by_backend
            .entry(row.backend.as_str())
            .or_default()
            .push(row);
    }
    let mut parts = Vec::new();
    for (backend, rows) in &by_backend {
        let kinds = rows
            .iter()
            .map(|r| format!("{} {}", r.count, super::cloud::route_kind_label(&r.kind)))
            .collect::<Vec<_>>()
            .join(" + ");
        let model = cfg
            .backends
            .get(*backend)
            .map(|b| b.model.as_str())
            .unwrap_or("?");
        parts.push(format!("{kinds} to {backend} ({model})"));
    }
    let mut line =
        "what leaves this machine today: the model download (once, on demand)".to_owned();
    for part in &parts {
        line.push_str(" \u{b7} ");
        line.push_str(part);
    }
    line.push_str(&format!(
        " \u{b7} MCP context fetches today {fetches_today} \u{b7} posts today {posts_today}"
    ));
    if parts.is_empty() {
        line.push_str(" \u{b7} nothing else");
    }
    line
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
        let mut density_pick: Option<theme::Density> = None;
        let mut spans_toggle: Option<bool> = None;
        let spans_debug_now = self.spans_debug;
        let autohide_now = self.autohide;
        let mut autohide_toggle: Option<bool> = None;
        let mut cloud_dirty = false;
        let conn = self.conn.as_ref();
        let pipeline = self.pipeline.as_ref();
        let tz = self.tz.clone();
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
            // Center the form; cap width so it stays readable when wide. A
            // wide window also gets a section index beside it (sticky: it
            // sits outside the scroll area) that scrolls the form.
            let wide = theme::wide(ui.ctx());
            let mut jump: Option<&str> = None;
            ui.horizontal_top(|ui| {
                let avail = ui.available_width();
                let max_w = avail.min(520.0);
                let index_w = if wide {
                    SECTION_INDEX_W + theme::SPACE_LG
                } else {
                    0.0
                };
                let pad = ((avail - max_w - index_w) / 2.0).max(0.0);
                ui.add_space(pad);
                if wide {
                    ui.vertical(|ui| {
                        ui.set_width(SECTION_INDEX_W);
                        ui.add_space(2.0);
                        for name in SECTIONS {
                            let text = egui::RichText::new(*name)
                                .text_style(egui::TextStyle::Small)
                                .color(theme::palette::TEXT_DIM);
                            if theme::ghost_button(ui, text).clicked() {
                                jump = Some(name);
                            }
                        }
                    });
                    ui.add_space(theme::SPACE_LG);
                }
                ui.vertical(|ui| {
                    ui.set_width(max_w);
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            ui.set_max_width(max_w);
                            section(ui, "Connections", true, jump);
                            panel.connections.ui(
                                ui,
                                conn,
                                &mut panel.git_repos,
                                &mut panel.sources,
                            );

                            section(ui, "Projects", false, jump);
                            panel.projects.ui(ui, conn);

                            section(ui, "Model", false, jump);
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
                            ui.add_space(theme::SPACE_LG);
                            panel.cloud.ui(ui, conn, &data_dir);
                            cloud_dirty |= panel.cloud.take_changed();

                            section(ui, "Capture", false, jump);
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
                                    ui.label("quiet");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.quiet_secs).range(0..=7200),
                                    );
                                    ui.weak("secs idle, nothing live, closes the span");
                                    ui.end_row();
                                    ui.label("away");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.away_secs).range(0..=14400),
                                    );
                                    ui.weak("secs idle with an agent, call or meeting");
                                    ui.end_row();
                                    ui.label("presence counts");
                                    ui.checkbox(&mut panel.capture_presence, "");
                                    ui.weak("keys, buttons, motion per minute; never which");
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
                            ui.label("distraction patterns (apps or sites, one regex per line)");
                            ui.add(
                                egui::TextEdit::multiline(&mut panel.distraction_patterns)
                                    .desired_rows(2)
                                    .font(egui::TextStyle::Monospace),
                            );

                            section(ui, "Derivation", false, jump);
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
                                    ui.label("placement");
                                    ui.checkbox(&mut panel.segmenter, "segmenter");
                                    ui.weak("evidence scoring, no live model (m30)");
                                    ui.end_row();
                                    ui.label("background under");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.background_minutes)
                                            .range(0..=120),
                                    );
                                    ui.weak("minutes (0 = off)");
                                    ui.end_row();
                                });
                            pipeline_card(ui, pipeline, &tz, panel, conn, &data_dir);
                            if let Some(info) = pipeline {
                                self_score_card(ui, &info.scores);
                            }

                            section(ui, "Standup & journal", false, jump);
                            egui::Grid::new("settings_journal")
                                .num_columns(3)
                                .show(ui, |ui| {
                                    ui.label("checkpoint after idle");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.checkpoint_afk_secs)
                                            .range(0..=14400)
                                            .speed(60),
                                    );
                                    ui.weak("secs (0 = off)");
                                    ui.end_row();
                                    ui.label("task autoclose");
                                    ui.add(
                                        egui::DragValue::new(&mut panel.task_autoclose_days)
                                            .range(0..=365),
                                    );
                                    ui.weak("days (0 = never)");
                                    ui.end_row();
                                });

                            section(ui, "Storage & server", false, jump);
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
                            // Everything that ever leaves the machine, in
                            // one line: the model download, today's cloud
                            // job usage by backend, and the MCP reads.
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(egress_line(
                                        &panel.ai_usage_today,
                                        panel.cloud.config(),
                                        panel.fetches_today,
                                        panel.posts_today,
                                    ))
                                    .text_style(theme::caption())
                                    .weak(),
                                )
                                .wrap(),
                            );

                            // UI-only prefs: applied immediately, stored in
                            // db meta (not config.toml), no daemon restart.
                            section(ui, "Window & appearance", false, jump);
                            ui.horizontal(|ui| {
                                ui.label("text size");
                                for (label, z) in [("S", 0.92f32), ("M", 1.0), ("L", 1.1)] {
                                    let active = (ui.ctx().zoom_factor() - z).abs() < 0.01;
                                    if theme::selectable(ui, active, label).clicked() && !active {
                                        ui.ctx().set_zoom_factor(z);
                                    }
                                }
                            });
                            ui.weak("Ctrl +/\u{2212}/0 also works anywhere");
                            ui.horizontal(|ui| {
                                ui.label("row spacing");
                                let now = theme::density();
                                for d in [theme::Density::Comfortable, theme::Density::Compact] {
                                    let active = now == d;
                                    if theme::selectable(ui, active, d.as_str()).clicked()
                                        && !active
                                    {
                                        density_pick = Some(d);
                                    }
                                }
                            });
                            let mut dbg = spans_debug_now;
                            if switch_row(ui, &mut dbg, "show raw spans on home") {
                                spans_toggle = Some(dbg);
                            }
                            let mut hide = autohide_now;
                            if switch_row(ui, &mut hide, "hide when focus leaves the window") {
                                autohide_toggle = Some(hide);
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
        if cloud_dirty {
            self.reload_cloud_kinds();
        }
        if let Some(d) = density_pick {
            theme::set_density(d);
            if let Some(conn) = self.conn.as_ref() {
                let _ = chronicle_core::storage::set_meta(
                    conn,
                    theme::Density::META_KEY,
                    Some(d.as_str()),
                );
            }
        }
        if let (Some(on), Some(conn)) = (spans_toggle, self.conn.as_ref()) {
            let value = on.then_some("1");
            let _ = chronicle_core::storage::set_meta(conn, "ui_show_spans_debug", value);
            self.spans_debug = on;
        }
        if let Some(on) = autohide_toggle {
            if let Some(conn) = self.conn.as_ref() {
                let _ = chronicle_core::storage::set_meta(conn, "ui_autohide", on.then_some("1"));
            }
            self.autohide = on;
        }
        if start_dl {
            self.start_model_download(ui.ctx(), chronicle_derive::model::default_preset());
        }
    }
}
