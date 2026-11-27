//! Read-only local GPU preflight for the visible, stopped Start controls.

use std::borrow::Cow;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_material_icons::icons;

use super::{icon_text, ui_text, RuntimeState, UiPalette, UiTextKey, WhisperDictateApp};
use crate::cloud_api::hviske_local::{self, LocalModel};
use crate::model_capacity::start::{LocalGpuCheck, LocalGpuTarget};
use crate::runtime::settings_snapshot::RuntimeSettingsSnapshot;

const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Default, PartialEq, Eq)]
enum Selection {
    #[default]
    Hosted,
    UnknownLocal,
    Target(LocalGpuTarget),
}

fn effective_selection(runtime: &RuntimeSettingsSnapshot) -> Selection {
    let settings = runtime.settings();
    let provider = runtime.stt_provider();
    if settings.stt_backend.trim().eq_ignore_ascii_case("openai") {
        if let Some(model) = LocalModel::from_alias(&settings.stt_model) {
            return if hviske_local::require_custom_provider(provider).is_ok() {
                Selection::Target(LocalGpuTarget::Hviske(model))
            } else {
                Selection::UnknownLocal
            };
        }
        if crate::cloud_api::is_nemotron_provider(provider)
            && crate::cloud_api::is_nemotron_in_process_endpoint(&settings.stt_base_url)
        {
            return Selection::Target(LocalGpuTarget::Nemotron {
                model: settings.stt_model.clone(),
                device: settings.device.clone(),
            });
        }
        return Selection::Hosted;
    }

    #[cfg(native_session)]
    if !matches!(
        crate::whisper::gpu::parse_gpu_policy(
            runtime.value(crate::whisper::GPU_ENV),
            Some(&settings.device),
        ),
        Ok(crate::whisper::GpuPolicy::Auto | crate::whisper::GpuPolicy::Vulkan)
    ) {
        return Selection::UnknownLocal;
    }

    if let Some(path) = runtime.value(super::app::WHISPER_MODEL_PATH_ENV) {
        let path = path.trim();
        if !path.is_empty() {
            return Selection::Target(LocalGpuTarget::WhisperFile(path.into()));
        }
        return Selection::UnknownLocal;
    }
    crate::whisper::model_manager::find(settings.model.trim())
        .map_or(Selection::UnknownLocal, |entry| {
            Selection::Target(LocalGpuTarget::Whisper(entry.required_gpu_mib()))
        })
}

struct PendingSample {
    receiver: Receiver<(LocalGpuCheck, Instant)>,
    obsolete: bool,
}

#[derive(Default)]
pub(super) struct GpuStartState {
    selection: Selection,
    result: Option<LocalGpuCheck>,
    pending: Option<PendingSample>,
    refresh_at: Option<Instant>,
    sample_at: Option<Instant>,
    #[cfg(test)]
    manual: bool,
}

impl GpuStartState {
    #[cfg(test)]
    pub(super) fn for_render_test() -> Self {
        Self {
            manual: true,
            ..Self::default()
        }
    }

    pub(super) fn refresh_selection(&mut self) {
        self.refresh_at = None;
    }

    pub(super) fn suspend(&mut self) {
        self.set_selection(Selection::Hosted);
        self.refresh_selection();
    }

    fn set_selection(&mut self, selection: Selection) {
        if selection == self.selection {
            return;
        }
        self.selection = selection;
        self.result = None;
        self.sample_at = None;
        // Keep the receiver until it resolves: a selection change must not
        // allow a second hardware probe to run concurrently with the first.
        if let Some(pending) = &mut self.pending {
            pending.obsolete = true;
        }
    }

    fn complete_sample(&mut self, now: Instant) {
        let Some(pending) = &self.pending else {
            return;
        };
        let (result, finished_at) = match pending.receiver.try_recv() {
            Ok(sample) => sample,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => (LocalGpuCheck::Unavailable, now),
        };
        if !pending.obsolete {
            self.result = Some(result);
            self.sample_at = Some(finished_at + REFRESH_INTERVAL);
        }
        self.pending = None;
    }

    fn sample(&mut self, ctx: &egui::Context, now: Instant) {
        self.complete_sample(now);
        // Do not reuse a readout from before a long idle/hidden interval.
        // During a routine refresh the last result remains visible.
        if self
            .sample_at
            .is_some_and(|at| now >= at + REFRESH_INTERVAL)
        {
            self.result = None;
        }
        if let Selection::Target(target) = &self.selection {
            if self.pending.is_none() && self.sample_at.is_none_or(|at| now >= at) {
                let (sender, receiver) = mpsc::channel();
                let ctx = ctx.clone();
                let target = target.clone();
                let started = std::thread::Builder::new()
                    .name("gpu-start-check".into())
                    .spawn(move || {
                        let result = target.check();
                        let _ = sender.send((result, Instant::now()));
                        ctx.request_repaint();
                    });
                if started.is_ok() {
                    self.pending = Some(PendingSample {
                        receiver,
                        obsolete: false,
                    });
                } else {
                    self.result = Some(LocalGpuCheck::Unavailable);
                    self.sample_at = Some(now + REFRESH_INTERVAL);
                }
            }
            if self.pending.is_none() {
                if let Some(at) = self.sample_at {
                    ctx.request_repaint_after(at.saturating_duration_since(now));
                }
            }
        }
    }

    fn status(&self) -> Option<GpuStartStatus> {
        match self.selection {
            Selection::Hosted => return None,
            Selection::UnknownLocal => return Some(GpuStartStatus::Unknown),
            Selection::Target(_) => {}
        }
        Some(match self.result {
            None => GpuStartStatus::Checking,
            Some(LocalGpuCheck::Unavailable) => GpuStartStatus::Unknown,
            Some(LocalGpuCheck::Capacity {
                required_mib,
                free_mib,
            }) => GpuStartStatus::Capacity {
                required_mib,
                free_mib,
            },
            Some(LocalGpuCheck::HviskeServiceReady) => GpuStartStatus::HviskeReady,
            Some(LocalGpuCheck::HviskeServiceNotReady {
                required_mib,
                free_mib,
            }) => GpuStartStatus::HviskeNotReady {
                required_mib,
                free_mib,
            },
        })
    }
}

#[derive(Clone, Copy)]
pub(super) enum GpuStartStatus {
    Checking,
    Unknown,
    Capacity {
        required_mib: u32,
        free_mib: Option<u32>,
    },
    HviskeReady,
    HviskeNotReady {
        required_mib: u32,
        free_mib: Option<u32>,
    },
}

impl GpuStartStatus {
    fn blocks_start(self) -> bool {
        match self {
            Self::Checking | Self::HviskeNotReady { .. } => true,
            Self::Capacity {
                required_mib,
                free_mib,
            } => free_mib.is_none_or(|free| free < required_mib),
            Self::Unknown | Self::HviskeReady => false,
        }
    }

    fn label(self, da: bool) -> Cow<'static, str> {
        match self {
            Self::Checking if da => "Kontrollerer ledig GPU-hukommelse…".into(),
            Self::Checking => "Checking free GPU memory…".into(),
            Self::Unknown if da => "Ukendt GPU-hukommelse – kontrolleres ved start".into(),
            Self::Unknown => "GPU memory unknown — checked at Start".into(),
            Self::Capacity {
                required_mib,
                free_mib: Some(free),
            } => {
                let state = match (free >= required_mib, da) {
                    (true, true) => "klar",
                    (true, false) => "ready",
                    (false, true) => "for lidt",
                    (false, false) => "too low",
                };
                let usage = if da { "ledig/krævet" } else { "free/needed" };
                format!("VRAM: {free}/{required_mib} MiB ({usage}) · {state}").into()
            }
            Self::Capacity {
                required_mib,
                free_mib: None,
            } if da => format!("Ingen egnet GPU · kræver {required_mib} MiB VRAM").into(),
            Self::Capacity {
                required_mib,
                free_mib: None,
            } => format!("No suitable GPU · needs {required_mib} MiB VRAM").into(),
            Self::HviskeReady if da => "Hviske-tjenesten er klar".into(),
            Self::HviskeReady => "Hviske service ready".into(),
            Self::HviskeNotReady {
                required_mib,
                free_mib,
            } => match (free_mib, da) {
                (Some(free), true) => {
                    format!("Hviske er ikke klar · {free}/{required_mib} MiB ledig/krævet")
                }
                (Some(free), false) => {
                    format!("Hviske service not ready · {free}/{required_mib} MiB free/needed")
                }
                (None, true) => format!("Hviske er ikke klar · kræver {required_mib} MiB VRAM"),
                (None, false) => {
                    format!("Hviske service not ready · needs {required_mib} MiB VRAM")
                }
            }
            .into(),
        }
    }

    fn help(self, da: bool) -> &'static str {
        match (self, da) {
            (Self::Checking, true) => "Vent på målingen af ledig GPU-hukommelse.",
            (Self::Checking, false) => "Wait for the free GPU memory check.",
            (Self::Unknown, true) => {
                "Ledig GPU-hukommelse kunne ikke måles. Programmet kontrollerer GPU'en ved start."
            }
            (Self::Unknown, false) => {
                "Free GPU memory could not be measured. The runtime will check the GPU at Start."
            }
            (Self::HviskeNotReady { .. }, true) => {
                "Start den lokale Hviske-tjeneste med den valgte model, og vent til den er klar."
            }
            (Self::HviskeNotReady { .. }, false) => {
                "Start the local Hviske service with the selected model and wait until it is ready."
            }
            (Self::HviskeReady, true) => "Den lokale Hviske-tjeneste har indlæst den valgte model.",
            (Self::HviskeReady, false) => "The local Hviske service has loaded the selected model.",
            (status, true) if status.blocks_start() => {
                "Frigør GPU-hukommelse, eller vælg en mindre lokal model."
            }
            (status, false) if status.blocks_start() => {
                "Free GPU memory or choose a smaller local model."
            }
            (_, true) => {
                "Ledig GPU-hukommelse kan ændre sig. Programmet kontrollerer igen ved start."
            }
            (_, false) => "Free GPU memory can change. The runtime will check again at Start.",
        }
    }
}

impl WhisperDictateApp {
    pub(super) fn poll_gpu_start(&mut self, ctx: &egui::Context) {
        #[cfg(test)]
        if self.gpu_start.manual {
            return;
        }
        if self.runtime_state != RuntimeState::Stopped {
            self.gpu_start.suspend();
            return;
        }

        let now = Instant::now();
        if self.gpu_start.refresh_at.is_none_or(|at| now >= at) {
            let command = self.saved_worker_base_command();
            self.gpu_start
                .set_selection(effective_selection(&command.runtime));
            self.gpu_start.refresh_at = Some(now + REFRESH_INTERVAL);
        }
        if let Some(at) = self.gpu_start.refresh_at {
            ctx.request_repaint_after(at.saturating_duration_since(now));
        }
        self.gpu_start.sample(ctx, now);
    }

    pub(super) fn gpu_start_status(&self) -> Option<GpuStartStatus> {
        if self.runtime_state == RuntimeState::Stopped {
            self.gpu_start.status()
        } else {
            None
        }
    }

    pub(super) fn gpu_start_status_label(&self, ui: &mut egui::Ui, palette: UiPalette) {
        if let Some(status) = self.gpu_start_status() {
            let da = self.settings.ui_language == "da";
            let color = if status.blocks_start() {
                palette.warn_text
            } else {
                palette.text_muted
            };
            ui.add(
                egui::Label::new(egui::RichText::new(status.label(da)).small().color(color)).wrap(),
            )
            .on_hover_text(status.help(da));
        }
    }

    pub(super) fn manual_start_button(
        &self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        min_size: egui::Vec2,
    ) -> egui::Response {
        let enabled = self.runtime_state == RuntimeState::Stopped
            && self.nemotron_probe_active.is_none()
            && !self
                .gpu_start_status()
                .is_some_and(GpuStartStatus::blocks_start);
        ui.add_enabled(
            enabled,
            egui::Button::new(
                icon_text(
                    icons::ICON_PLAY_ARROW.codepoint,
                    ui_text(&self.settings.ui_language, UiTextKey::Start),
                )
                .strong(),
            )
            .fill(palette.accent_dark)
            .min_size(min_size),
        )
    }
}

#[cfg(test)]
#[path = "gpu_start_tests.rs"]
mod tests;
