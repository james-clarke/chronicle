//! In-UI model provisioning: background `model::pull` with progress, and the
//! onboarding card shown while no model is available.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chronicle_derive::model::{self, ModelSpec};
use eframe::egui;

use super::{TimelineApp, theme};

pub(super) enum PullEvent {
    Progress { done: u64, total: u64 },
    Done,
    Failed(String),
}

/// One in-flight (or finished) model download. The thread detaches; closing
/// the app is safe — the `.part` file resumes on the next attempt. No cancel:
/// `model::pull` has no cancel hook. A concurrent CLI `chronicle model pull`
/// of the same preset would race on the `.part` file; unlikely enough to
/// leave unguarded.
pub(super) struct ModelDownload {
    rx: mpsc::Receiver<PullEvent>,
    done: u64,
    total: u64,
    pub(super) finished: Option<Result<(), String>>,
    pub(super) spec: &'static ModelSpec,
}

impl ModelDownload {
    pub(super) fn start(ctx: egui::Context, data_dir: PathBuf, spec: &'static ModelSpec) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("model-pull".into())
            .spawn(move || {
                // Repaints throttled; progress fires ~1/MiB which can be
                // dozens per second on a fast link.
                let mut last_paint = Instant::now();
                let result = model::pull(&data_dir, spec, &mut |done, total| {
                    let _ = tx.send(PullEvent::Progress { done, total });
                    if last_paint.elapsed() >= Duration::from_millis(250) {
                        last_paint = Instant::now();
                        ctx.request_repaint();
                    }
                });
                let _ = tx.send(match result {
                    Ok(_) => PullEvent::Done,
                    Err(e) => PullEvent::Failed(format!("{e:#}")),
                });
                ctx.request_repaint();
            })
            .expect("spawn model-pull thread");
        Self {
            rx,
            done: 0,
            total: 0,
            finished: None,
            spec,
        }
    }

    pub(super) fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                PullEvent::Progress { done, total } => {
                    self.done = done;
                    self.total = total;
                }
                PullEvent::Done => self.finished = Some(Ok(())),
                PullEvent::Failed(e) => self.finished = Some(Err(e)),
            }
        }
    }

    fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64) as f32
        }
    }
}

fn gib(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

/// Progress bar + byte counts for an in-flight download.
pub(super) fn progress_ui(ui: &mut egui::Ui, dl: &ModelDownload) {
    let text = if dl.total == 0 {
        format!("starting {}\u{2026}", dl.spec.name)
    } else {
        format!(
            "{} \u{b7} {:.1} / {:.1} GiB",
            dl.spec.name,
            gib(dl.done),
            gib(dl.total)
        )
    };
    ui.add(egui::ProgressBar::new(dl.fraction()).text(text));
}

pub(super) fn systemd_available() -> bool {
    std::process::Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn user_unit_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config/systemd/user/chronicle.service"))
}

pub(super) fn service_unit_exists() -> bool {
    user_unit_path().is_some_and(|p| p.exists())
}

/// Write the user unit (ExecStart pointed at this binary) and enable it.
/// Deliberately `enable` without `--now`: the daemon is already running as
/// this UI's parent and holds the single-instance socket; `--now` would start
/// a second instance that just toggles and exits, leaving a confusing
/// stopped unit.
pub(super) fn install_service() -> Result<String, String> {
    let exe = crate::own_exe().map_err(|e| e.to_string())?;
    let unit_path = user_unit_path().ok_or("HOME not set")?;
    let template = include_str!("../../../../packaging/chronicle.service");
    let unit = template.replace(
        "ExecStart=%h/.cargo/bin/chronicle run",
        &format!("ExecStart={} run", exe.display()),
    );
    if let Some(dir) = unit_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&unit_path, unit).map_err(|e| e.to_string())?;
    for args in [["daemon-reload", ""], ["enable", "chronicle"]] {
        let args: Vec<&str> = args.iter().filter(|a| !a.is_empty()).copied().collect();
        let out = std::process::Command::new("systemctl")
            .arg("--user")
            .args(&args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "systemctl --user {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok("installed \u{2014} Chronicle will start at your next login".into())
}

impl TimelineApp {
    /// Dismissible "run at login" card, shown while no user unit exists.
    pub(super) fn service_card_ui(&mut self, ui: &mut egui::Ui) {
        if !self.service_card || self.service_dismissed {
            return;
        }
        theme::hover_card(ui, "service_card", |ui| {
            match &self.service_status {
                Some(Ok(msg)) => {
                    ui.colored_label(theme::palette::GREEN, msg);
                    return;
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::palette::RED, e);
                }
                None => {}
            }
            ui.label(
                egui::RichText::new("Start Chronicle at login")
                    .text_style(egui::TextStyle::Heading)
                    .color(theme::palette::TEXT),
            );
            ui.label(
                "Chronicle is running now; install the background service so it \
                     starts automatically at your next login.",
            );
            if let Ok(exe) = crate::own_exe()
                && exe.components().any(|c| c.as_os_str() == "target")
            {
                ui.colored_label(
                    theme::palette::AMBER,
                    format!(
                        "running a dev build \u{2014} the service will point at {}",
                        exe.display()
                    ),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "install service").clicked() {
                    self.service_status = Some(install_service());
                }
                if theme::ghost_button(
                    ui,
                    egui::RichText::new("dismiss").text_style(egui::TextStyle::Small),
                )
                .clicked()
                {
                    self.service_dismissed = true;
                    if let Some(conn) = self.conn.as_ref() {
                        let _ = chronicle_core::storage::set_meta(
                            conn,
                            "onboard_service_dismissed",
                            Some("1"),
                        );
                    }
                }
            });
        });
        ui.add_space(8.0);
    }

    pub(super) fn start_model_download(&mut self, ctx: &egui::Context, spec: &'static ModelSpec) {
        if self
            .model_dl
            .as_ref()
            .is_some_and(|dl| dl.finished.is_none())
        {
            return; // one at a time
        }
        self.model_dl = Some(ModelDownload::start(
            ctx.clone(),
            self.data_dir.clone(),
            spec,
        ));
    }

    /// Onboarding card at the top of the timeline while no model exists (or a
    /// download is in flight / just finished).
    pub(super) fn model_card_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(dl) = &mut self.model_dl {
            dl.drain();
            // Success card stays until the 5s reload notices the model file.
            if dl.finished.as_ref().is_some_and(|r| r.is_ok()) && !self.model_missing {
                self.model_dl = None;
            }
        }
        if !self.model_missing && self.model_dl.is_none() {
            return;
        }
        let mut start: Option<&'static ModelSpec> = None;
        theme::hover_card(ui, "model_card", |ui| match &self.model_dl {
            Some(dl) => match &dl.finished {
                None => progress_ui(ui, dl),
                Some(Ok(())) => {
                    ui.colored_label(
                        theme::palette::GREEN,
                        "model ready \u{2014} tasks will start appearing after the \
                                 next batch (or press derive now)",
                    );
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::palette::RED, format!("download failed: {e}"));
                    if ui.button("retry").clicked() {
                        start = Some(dl.spec);
                    }
                }
            },
            None => {
                ui.label(
                    egui::RichText::new("Chronicle needs a local model")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.label(
                    "Tasks are derived on-device by a small LLM. Download once; \
                             everything stays local.",
                );
                ui.add_space(4.0);
                // Stacked, not side by side: both labels together are
                // wider than the 400px widget.
                for (i, spec) in model::PRESETS.iter().enumerate() {
                    let hint = if i == 0 {
                        "recommended \u{b7} ~2.4 GiB"
                    } else {
                        "low-RAM \u{b7} ~1.1 GiB"
                    };
                    if theme::selectable(
                        ui,
                        self.preset_pick == i,
                        format!("{} ({hint})", spec.name),
                    )
                    .clicked()
                    {
                        self.preset_pick = i;
                    }
                }
                ui.add_space(4.0);
                if theme::primary_button(ui, "download model").clicked() {
                    start = Some(&model::PRESETS[self.preset_pick]);
                }
            }
        });
        ui.add_space(8.0);
        if let Some(spec) = start {
            self.start_model_download(&ui.ctx().clone(), spec);
        }
    }
}
