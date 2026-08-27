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
        egui::Frame::new()
            .fill(theme::palette::SURFACE)
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| match &self.model_dl {
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
                    ui.horizontal(|ui| {
                        for (i, spec) in model::PRESETS.iter().enumerate() {
                            let hint = if i == 0 {
                                "recommended \u{b7} ~2.4 GiB"
                            } else {
                                "low-RAM \u{b7} ~1.1 GiB"
                            };
                            if ui
                                .selectable_label(
                                    self.preset_pick == i,
                                    format!("{} ({hint})", spec.name),
                                )
                                .clicked()
                            {
                                self.preset_pick = i;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    let dl_btn = egui::Button::new(
                        egui::RichText::new("download model").color(theme::palette::BG),
                    )
                    .fill(theme::palette::ACCENT);
                    if ui.add(dl_btn).clicked() {
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
