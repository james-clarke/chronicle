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

impl TimelineApp {
    /// Dismissible "run at login" card, shown while nothing is installed yet.
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
                    self.service_status = Some(crate::service::install());
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
        // A cloud backend counts as "a model is configured" even before a
        // local model resolves (m31 c7): don't push the download card once
        // BYOK is set up.
        if (!self.model_missing || self.has_cloud_backend) && self.model_dl.is_none() {
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

    /// Card shown on Home while Accessibility isn't trusted yet (m38 chunk
    /// 4): without it the focus provider still emits app names, just no
    /// window titles. Re-checks `ax::trusted` at most every 5s — it shells
    /// out to `AXIsProcessTrustedWithOptions`, not free per frame.
    #[cfg(target_os = "macos")]
    pub(super) fn ax_card_ui(&mut self, ui: &mut egui::Ui) {
        if self
            .ax_last_check
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5))
        {
            self.ax_trusted = chronicle_capture::macos::ax::trusted(false);
            self.ax_last_check = Some(Instant::now());
        }
        if self.ax_trusted {
            return;
        }
        theme::hover_card(ui, "ax_card", |ui| {
            ui.label(
                egui::RichText::new("Allow window titles")
                    .text_style(egui::TextStyle::Heading)
                    .color(theme::palette::TEXT),
            );
            ui.label(
                "Chronicle reads the focused window's title through Accessibility \
                 \u{2014} no screen recording. Grant it under Privacy & Security \
                 \u{203a} Accessibility.",
            );
            ui.add_space(4.0);
            if theme::primary_button(ui, "Open System Settings").clicked() {
                let _ = std::process::Command::new("open")
                    .arg(
                        "x-apple.systempreferences:com.apple.preference.security\
                         ?Privacy_Accessibility",
                    )
                    .spawn();
            }
        });
        ui.add_space(8.0);
    }
}
