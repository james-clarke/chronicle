//! Settings › Model › Cloud backends + Routing (m31 chunk 1, commit C7).
//! Cloud backends: named `[backends.<name>]` entries from `models.toml`,
//! each testable with one tiny request; Routing: the job-kind → backend
//! table plus the daily spend cap. `ModelsConfig` is re-read from disk
//! after every save, so the file stays the truth. The API key string
//! lives only in the form's edit buffer and in `ModelsConfig` — never in
//! meta, logs, or `Config`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::mpsc;

use chronicle_core::models_config::{ALL_KINDS, BackendCfg, BackendKind, ModelsConfig, Preset};
use chronicle_core::storage;
use eframe::egui;
use rusqlite::Connection;

use super::theme::{self, palette};

/// Anthropic models offered in the add-backend picker (chunk 1: Anthropic
/// only; OpenAI-compatible arrives in chunk 3).
const ANTHROPIC_MODELS: &[&str] = &["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"];

fn probe_key(name: &str) -> String {
    format!("cloud_probe:{name}")
}

/// Meta flag a job sets when the day's spend crosses `max_usd_per_day`
/// (m31 chunk 1's cost-cap enforcement, not built by this commit); shown
/// here if already present.
fn cap_hit_key() -> String {
    format!("cloud_cap_hit:{}", jiff::Zoned::now().date())
}

/// Probe verdict cached in meta `cloud_probe:<name>`, so a row keeps its
/// status across restarts. Holds no key material.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ProbeRecord {
    ok: bool,
    ms: Option<u64>,
    error: Option<String>,
    ts: i64,
}

enum Probe {
    Running(mpsc::Receiver<Result<u64, String>>),
    Done(ProbeRecord),
}

fn kind_label(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Anthropic => "Anthropic",
        BackendKind::OpenAiCompat => "OpenAI-compatible",
        BackendKind::ClaudeCode => "Claude Code login",
    }
}

/// Human words for a `[routes]` key, in `ALL_KINDS` order — the Routing
/// grid's row labels and the egress line's per-kind counts.
pub(super) fn route_kind_label(kind: &str) -> &'static str {
    match kind {
        "chat" => "chat",
        "narrative" => "narrative",
        "standup" => "standup",
        "journal" => "journal",
        "task_description" => "task description",
        "checkpoint" => "checkpoint",
        "suggest_task" => "suggest task",
        "name_task" => "name new tasks",
        "consolidate" => "tidy day",
        "derive" => "derive batches",
        "live" => "live tier",
        "advise" => "advise placements",
        _ => "unknown",
    }
}

/// Add/edit form. Renaming a backend is not offered: `ModelsConfig` has no
/// rename primitive that also repoints `routes`, so the name field is
/// locked once a backend exists and edits go through remove + re-add.
struct BackendForm {
    original: Option<String>,
    name: String,
    model: String,
    /// Blank while adding (required) or editing (keep the stored key).
    api_key: String,
    /// `masked_key()` of the stored key, shown as a hint while editing.
    key_hint: Option<String>,
    base_url: String,
    error: Option<String>,
}

impl BackendForm {
    fn blank() -> Self {
        Self {
            original: None,
            name: "anthropic".to_owned(),
            model: String::new(),
            api_key: String::new(),
            key_hint: None,
            base_url: String::new(),
            error: None,
        }
    }

    fn from_backend(name: &str, cfg: &BackendCfg) -> Self {
        Self {
            original: Some(name.to_owned()),
            name: name.to_owned(),
            model: cfg.model.clone(),
            api_key: String::new(),
            key_hint: Some(cfg.masked_key()),
            base_url: cfg.base_url.clone().unwrap_or_default(),
            error: None,
        }
    }
}

enum FormAct {
    None,
    Submit,
    Cancel,
}

fn backend_form_ui(ui: &mut egui::Ui, form: &mut BackendForm) -> FormAct {
    let mut act = FormAct::None;
    theme::card().show(ui, |ui| {
        let title = if form.original.is_some() {
            "edit backend"
        } else {
            "add backend"
        };
        ui.label(
            egui::RichText::new(title)
                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                .color(palette::TEXT),
        );
        ui.add_space(theme::SPACE_XS);
        ui.label("name");
        ui.add_enabled(
            form.original.is_none(),
            egui::TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY),
        );
        ui.label("kind");
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Anthropic").color(palette::TEXT));
            ui.add_enabled(false, egui::Button::new("OpenAI-compatible"))
                .on_disabled_hover_text("arrives in m31 chunk 3");
        });
        ui.label("model");
        egui::ComboBox::from_id_salt("cloud_backend_model")
            .selected_text(if form.model.is_empty() {
                "choose a model\u{2026}".to_owned()
            } else {
                form.model.clone()
            })
            .show_ui(ui, |ui| {
                for m in ANTHROPIC_MODELS {
                    let label = match chronicle_derive::cloud::price_per_mtok(m) {
                        Some((inp, out)) => format!("{m}  (${inp:.0} / ${out:.0} per MTok)"),
                        None => (*m).to_owned(),
                    };
                    ui.selectable_value(&mut form.model, (*m).to_owned(), label);
                }
            });
        ui.label("API key");
        let hint_text = match &form.key_hint {
            Some(h) => format!("leave blank to keep {h}"),
            None => "sk-ant-\u{2026}".to_owned(),
        };
        ui.add(
            egui::TextEdit::singleline(&mut form.api_key)
                .password(true)
                .desired_width(f32::INFINITY)
                .hint_text(hint_text),
        );
        ui.label("base URL (optional)");
        ui.add(
            egui::TextEdit::singleline(&mut form.base_url)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace)
                .hint_text(chronicle_derive::cloud::anthropic::DEFAULT_BASE_URL),
        );
        if let Some(e) = &form.error {
            ui.colored_label(palette::RED, e);
        }
        ui.add_space(theme::SPACE_XS);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, "save").clicked() {
                act = FormAct::Submit;
            }
            if theme::ghost_button(ui, "cancel").clicked() {
                act = FormAct::Cancel;
            }
        });
    });
    act
}

enum RowAct {
    Test(String),
    Edit(String),
    Arm(String),
    Remove(String),
}

/// Settings state for the Cloud backends + Routing cards. Owns the loaded
/// `ModelsConfig`; every mutation writes `models.toml` then re-reads it, so
/// `self.cfg` never drifts from the file a concurrently running daemon
/// would load.
pub(super) struct CloudPanel {
    cfg: ModelsConfig,
    /// `models.toml` failed to parse: edits are off until it's fixed by
    /// hand, same as the MCP servers card.
    load_error: Option<String>,
    probes: BTreeMap<String, Probe>,
    form: Option<BackendForm>,
    arm_remove: Option<String>,
    status: Option<Result<String, String>>,
    /// Name of the backend just added, offering the routing preset.
    preset_prompt: Option<String>,
    cost_today: f64,
    cap_hit: bool,
    /// Set on every successful save; the caller polls and clears it with
    /// [`Self::take_changed`] to refresh `TimelineApp::cloud_kinds`.
    changed: bool,
}

impl CloudPanel {
    pub(super) fn load(data_dir: &Path, conn: Option<&Connection>) -> Self {
        let (cfg, load_error) = match ModelsConfig::load(data_dir) {
            Ok(c) => (c, None),
            Err(e) => (ModelsConfig::default(), Some(e.to_string())),
        };
        let mut probes = BTreeMap::new();
        if let Some(conn) = conn {
            for name in cfg.backends.keys() {
                if let Ok(Some(json)) = storage::get_meta(conn, &probe_key(name))
                    && let Ok(rec) = serde_json::from_str::<ProbeRecord>(&json)
                {
                    probes.insert(name.clone(), Probe::Done(rec));
                }
            }
        }
        let cost_today = conn
            .and_then(|c| storage::cost_today(c, crate::ai_job::day_start_ms()).ok())
            .unwrap_or(0.0);
        let cap_hit = conn
            .and_then(|c| storage::get_meta(c, &cap_hit_key()).ok().flatten())
            .is_some();
        Self {
            cfg,
            load_error,
            probes,
            form: None,
            arm_remove: None,
            status: None,
            preset_prompt: None,
            cost_today,
            cap_hit,
            changed: false,
        }
    }

    /// The config as last loaded or saved — settings.rs reads it for the
    /// egress line (backend → model lookup).
    pub(super) fn config(&self) -> &ModelsConfig {
        &self.cfg
    }

    /// `true` once, the first time it's read after a save; clears the flag.
    pub(super) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub(super) fn ui(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>, data_dir: &Path) {
        self.poll_probes(conn);
        self.backends_ui(ui, conn, data_dir);
        ui.add_space(theme::SPACE_SM);
        self.routing_ui(ui, data_dir);
    }

    fn poll_probes(&mut self, conn: Option<&Connection>) {
        for (name, probe) in self.probes.iter_mut() {
            let Probe::Running(rx) = probe else {
                continue;
            };
            let Ok(result) = rx.try_recv() else {
                continue;
            };
            let ts = jiff::Timestamp::now().as_millisecond();
            let rec = match result {
                Ok(ms) => ProbeRecord {
                    ok: true,
                    ms: Some(ms),
                    error: None,
                    ts,
                },
                Err(e) => ProbeRecord {
                    ok: false,
                    ms: None,
                    error: Some(e),
                    ts,
                },
            };
            if let (Some(conn), Ok(json)) = (conn, serde_json::to_string(&rec)) {
                let _ = storage::set_meta(conn, &probe_key(name), Some(&json));
            }
            *probe = Probe::Done(rec);
        }
    }

    /// One tiny request on a `std::thread`, never the UI thread; the panel
    /// polls the channel each frame in [`Self::poll_probes`].
    fn start_probe(&mut self, ctx: &egui::Context, name: String, cfg: BackendCfg) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let key = name.clone();
        std::thread::Builder::new()
            .name("cloud-probe".into())
            .spawn(move || {
                let result = match cfg.kind {
                    BackendKind::Anthropic => {
                        let backend = chronicle_derive::cloud::anthropic::AnthropicBackend::new(
                            &name,
                            &cfg.model,
                            &cfg.api_key,
                            cfg.base_url.as_deref(),
                        );
                        backend
                            .probe()
                            .map(|d| d.as_millis() as u64)
                            .map_err(|e| e.brief())
                    }
                    BackendKind::OpenAiCompat => {
                        Err("openai_compat arrives in m31 chunk 3".to_owned())
                    }
                    BackendKind::ClaudeCode => {
                        chronicle_derive::cloud::claude_code::ClaudeCodeBackend::new(
                            &name,
                            &cfg.model,
                            cfg.command.as_deref(),
                        )
                        .probe()
                        .map(|d| d.as_millis() as u64)
                        .map_err(|e| e.brief())
                    }
                };
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .expect("spawn cloud-probe thread");
        self.probes.insert(key, Probe::Running(rx));
    }

    fn backends_ui(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>, data_dir: &Path) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Cloud backends")
                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                    .color(palette::TEXT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::ghost_button(ui, "add backend").clicked() {
                    self.form = Some(BackendForm::blank());
                    self.arm_remove = None;
                }
            });
        });
        if let Some(err) = &self.load_error {
            ui.colored_label(palette::RED, format!("models.toml: {err}"));
            ui.weak("fix the file by hand; edits here are off until it loads");
            return;
        }
        if self.cfg.backends.is_empty() && self.form.is_none() {
            ui.weak("no cloud backends \u{2014} chat, journals and other writing jobs stay local");
        }
        let width = ui.available_width();
        let mut act: Option<RowAct> = None;
        let names: Vec<String> = self.cfg.backends.keys().cloned().collect();
        for name in &names {
            let Some(cfg) = self.cfg.backends.get(name) else {
                continue;
            };
            let probe = self.probes.get(name);
            let armed = self.arm_remove.as_deref() == Some(name.as_str());
            let running = matches!(probe, Some(Probe::Running(_)));
            let (dot, chip, chip_color) = match probe {
                None => (
                    palette::TEXT_DIM,
                    "not tested".to_owned(),
                    palette::TEXT_DIM,
                ),
                Some(Probe::Running(_)) => (
                    palette::TEXT_DIM,
                    "testing\u{2026}".to_owned(),
                    palette::TEXT_DIM,
                ),
                Some(Probe::Done(r)) if r.ok => (
                    palette::GREEN,
                    r.ms.map(|ms| format!("{ms} ms"))
                        .unwrap_or_else(|| "ok".to_owned()),
                    palette::GREEN,
                ),
                Some(Probe::Done(_)) => (palette::AMBER, "failed".to_owned(), palette::AMBER),
            };
            theme::ListRow::new(name)
                .emphasis()
                .dot(dot)
                .chip(kind_label(cfg.kind).to_owned(), palette::TEXT_DIM)
                .chip(cfg.model.clone(), palette::TEXT_DIM)
                .chip(chip, chip_color)
                .show(ui, width, |ui| {
                    let label = if armed { "delete?" } else { "\u{d7}" };
                    if theme::ghost_button(ui, label).clicked() {
                        act = Some(if armed {
                            RowAct::Remove(name.clone())
                        } else {
                            RowAct::Arm(name.clone())
                        });
                    }
                    if theme::ghost_button(ui, "edit").clicked() {
                        act = Some(RowAct::Edit(name.clone()));
                    }
                    if running {
                        ui.spinner();
                    } else if theme::ghost_button(ui, "test").clicked() {
                        act = Some(RowAct::Test(name.clone()));
                    }
                });
            if let Some(Probe::Done(r)) = probe
                && !r.ok
            {
                let err = r.error.clone().unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("failed: {err}"))
                                .text_style(theme::caption())
                                .color(palette::RED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(err);
                });
            }
        }
        match act {
            Some(RowAct::Arm(n)) => self.arm_remove = Some(n),
            Some(RowAct::Remove(n)) => {
                self.arm_remove = None;
                self.status = Some(
                    self.remove_backend(&n, conn, data_dir)
                        .map(|()| format!("removed {n}")),
                );
            }
            Some(RowAct::Edit(n)) => {
                self.arm_remove = None;
                if let Some(cfg) = self.cfg.backends.get(&n) {
                    self.form = Some(BackendForm::from_backend(&n, cfg));
                }
            }
            Some(RowAct::Test(n)) => {
                if let Some(cfg) = self.cfg.backends.get(&n).cloned() {
                    let ctx = ui.ctx().clone();
                    self.start_probe(&ctx, n, cfg);
                }
            }
            None => {}
        }
        if let Some(name) = self.preset_prompt.clone() {
            ui.horizontal(|ui| {
                ui.weak(format!("route {name} for:"));
                if theme::primary_button(ui, "writing and chat").clicked() {
                    self.status = Some(
                        self.apply_preset(Preset::WritingAndChat, &name, data_dir)
                            .map(|()| format!("{name} routed for writing and chat")),
                    );
                    self.preset_prompt = None;
                }
                if theme::secondary_button(ui, "everything").clicked() {
                    self.status = Some(
                        self.apply_preset(Preset::Everything, &name, data_dir)
                            .map(|()| format!("{name} routed for everything")),
                    );
                    self.preset_prompt = None;
                }
                if theme::ghost_button(ui, "later").clicked() {
                    self.preset_prompt = None;
                }
            });
        }
        match &self.status {
            Some(Ok(msg)) => {
                ui.weak(msg.as_str());
            }
            Some(Err(msg)) => {
                ui.colored_label(palette::RED, msg);
            }
            None => {}
        }
        if self
            .cfg
            .backends
            .values()
            .any(|c| c.kind == BackendKind::Anthropic)
        {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Prompts routed here go to Anthropic under its commercial API terms: \
                         not used for training, retained up to 30 days (checked 2026-09-04).",
                    )
                    .text_style(theme::caption())
                    .weak(),
                )
                .wrap(),
            );
        }
        let form_act = match &mut self.form {
            Some(form) => {
                ui.add_space(theme::SPACE_XS);
                backend_form_ui(ui, form)
            }
            None => FormAct::None,
        };
        match form_act {
            FormAct::Submit => match self.apply_form(data_dir) {
                Ok(name) => self.status = Some(Ok(format!("saved {name}"))),
                Err(e) => {
                    if let Some(form) = &mut self.form {
                        form.error = Some(e);
                    }
                }
            },
            FormAct::Cancel => self.form = None,
            FormAct::None => {}
        }
    }

    fn routing_ui(&mut self, ui: &mut egui::Ui, data_dir: &Path) {
        ui.label(
            egui::RichText::new("Routing")
                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                .color(palette::TEXT),
        );
        let has_backend = !self.cfg.backends.is_empty();
        let target = self.cfg.backends.keys().next().cloned();
        ui.add_enabled_ui(has_backend, |ui| {
            ui.horizontal(|ui| {
                let writing_label = match &target {
                    Some(n) => format!("use {n} for writing and chat"),
                    None => "use for writing and chat".to_owned(),
                };
                if theme::primary_button(ui, &writing_label).clicked()
                    && let Some(name) = &target
                {
                    self.status = Some(
                        self.apply_preset(Preset::WritingAndChat, name, data_dir)
                            .map(|()| format!("{name} routed for writing and chat")),
                    );
                }
                let everything_label = match &target {
                    Some(n) => format!("use {n} for everything"),
                    None => "use for everything".to_owned(),
                };
                if theme::secondary_button(ui, &everything_label).clicked()
                    && let Some(name) = &target
                {
                    self.status = Some(
                        self.apply_preset(Preset::Everything, name, data_dir)
                            .map(|()| format!("{name} routed for everything")),
                    );
                }
            });
        });
        ui.add_space(theme::SPACE_XS);
        // Mutated on a scratch copy so a failed save never leaves `self.cfg`
        // showing an edit that isn't on disk.
        let mut next = self.cfg.clone();
        egui::Grid::new("cloud_routing")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                for kind in ALL_KINDS {
                    ui.label(route_kind_label(kind));
                    let current = next
                        .routes
                        .get(*kind)
                        .cloned()
                        .unwrap_or_else(|| "local".to_owned());
                    let mut picked = current.clone();
                    egui::ComboBox::from_id_salt(("cloud_route", *kind))
                        .selected_text(picked.clone())
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut picked, "local".to_owned(), "local");
                            for name in next.backends.keys() {
                                ui.selectable_value(&mut picked, name.clone(), name.clone());
                            }
                        });
                    if picked != current {
                        if picked == "local" {
                            next.routes.remove(*kind);
                        } else {
                            next.routes.insert((*kind).to_owned(), picked);
                        }
                    }
                    ui.end_row();
                }
            });
        ui.add_space(theme::SPACE_XS);
        ui.horizontal(|ui| {
            ui.label("daily cap");
            ui.add(
                egui::DragValue::new(&mut next.max_usd_per_day)
                    .range(0.0..=1000.0)
                    .speed(0.1)
                    .prefix("$")
                    .max_decimals(2),
            );
        });
        if next != self.cfg {
            self.status = Some(
                self.commit(next, data_dir)
                    .map(|()| "routing saved".to_owned()),
            );
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("today's spend: ${:.2}", self.cost_today))
                    .text_style(theme::caption())
                    .weak(),
            )
            .wrap(),
        );
        if self.cap_hit {
            ui.colored_label(palette::AMBER, "cap reached, running local for today");
        }
        let mut unpriced: Vec<&str> = self
            .cfg
            .backends
            .values()
            .map(|c| c.model.as_str())
            .filter(|m| chronicle_derive::cloud::price_per_mtok(m).is_none())
            .collect();
        unpriced.sort_unstable();
        unpriced.dedup();
        if !unpriced.is_empty() {
            ui.colored_label(
                palette::AMBER,
                format!(
                    "no price known for {}: the cap cannot apply",
                    unpriced.join(", ")
                ),
            );
        }
    }

    /// Write `next` to `models.toml` then re-read it into `self.cfg`, so a
    /// failed save never leaves an unsaved edit showing and a successful
    /// one matches the file even if the daemon wrote it between frames.
    fn commit(&mut self, next: ModelsConfig, data_dir: &Path) -> Result<(), String> {
        next.save(data_dir).map_err(|e| e.to_string())?;
        self.cfg = ModelsConfig::load(data_dir).map_err(|e| e.to_string())?;
        self.changed = true;
        Ok(())
    }

    fn apply_preset(
        &mut self,
        preset: Preset,
        backend: &str,
        data_dir: &Path,
    ) -> Result<(), String> {
        let mut next = self.cfg.clone();
        next.apply_preset(preset, backend);
        self.commit(next, data_dir)
    }

    /// Add or replace the form's backend and write the file; the form
    /// closes on success and keeps the user's edits on failure. On a new
    /// backend, if no route is set anywhere yet, offers the routing preset.
    fn apply_form(&mut self, data_dir: &Path) -> Result<String, String> {
        let Some(form) = &self.form else {
            return Err("no form open".into());
        };
        let name = form.name.trim().to_owned();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let model = form.model.trim().to_owned();
        if model.is_empty() {
            return Err("choose a model".into());
        }
        let is_new = form.original.is_none();
        if is_new && self.cfg.backends.contains_key(&name) {
            return Err(format!("a backend named {name} already exists"));
        }
        let key = if form.api_key.trim().is_empty() {
            if is_new {
                return Err("API key is required".into());
            }
            self.cfg
                .backends
                .get(&name)
                .map(|c| c.api_key.clone())
                .ok_or_else(|| "backend no longer exists".to_owned())?
        } else {
            form.api_key.trim().to_owned()
        };
        let base_url = {
            let t = form.base_url.trim();
            (!t.is_empty()).then(|| t.to_owned())
        };
        let mut next = self.cfg.clone();
        next.backends.insert(
            name.clone(),
            BackendCfg {
                kind: BackendKind::Anthropic,
                model,
                api_key: key,
                base_url,
                command: None,
            },
        );
        self.commit(next, data_dir)?;
        self.probes.remove(&name);
        self.form = None;
        if is_new && self.cfg.routes.is_empty() {
            self.preset_prompt = Some(name.clone());
        }
        Ok(name)
    }

    fn remove_backend(
        &mut self,
        name: &str,
        conn: Option<&Connection>,
        data_dir: &Path,
    ) -> Result<(), String> {
        let mut next = self.cfg.clone();
        next.remove_backend(name);
        self.commit(next, data_dir)?;
        self.probes.remove(name);
        if let Some(conn) = conn {
            let _ = storage::set_meta(conn, &probe_key(name), None);
        }
        if self.preset_prompt.as_deref() == Some(name) {
            self.preset_prompt = None;
        }
        Ok(())
    }
}
