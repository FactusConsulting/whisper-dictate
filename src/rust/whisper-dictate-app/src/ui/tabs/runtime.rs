use super::super::*;
use super::*;
use egui_material_icons::icons;

const MIC_INDICATOR_MAX_WIDTH: f32 = 330.0;
const MIC_INDICATOR_MIN_WIDTH: f32 = 150.0;
const MIC_GAUGE_MAX_WIDTH: f32 = 150.0;
const MIC_GAUGE_MIN_WIDTH: f32 = 86.0;
const RUNTIME_LOG_TOP_MARGIN: f32 = 16.0;
const RUNTIME_LOG_CONTENT_TOP_PADDING: f32 = 10.0;
const RUNTIME_LOG_CONTENT_BOTTOM_PADDING: f32 = 14.0;

impl WhisperDictateApp {
    pub(in crate::ui) fn runtime_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        self.live_dictation_panel(ui, palette);
    }

    fn live_dictation_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.horizontal(|ui| {
            ui.label(
                icon_text(
                    icons::ICON_MIC,
                    ui_text(&self.settings.ui_language, UiTextKey::LiveDictation),
                )
                .size(18.0)
                .strong()
                .color(palette.text),
            );
            runtime_status_badge(ui, self.runtime_state, palette, &self.settings.ui_language);
            let mic_width = (ui.available_width() - 10.0).clamp(0.0, MIC_INDICATOR_MAX_WIDTH);
            if mic_width >= MIC_INDICATOR_MIN_WIDTH {
                ui.add_space(10.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(mic_width, 30.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        self.listening_gauge(ui, palette, mic_width);
                    },
                );
            }
        });
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(ui_text(&self.settings.ui_language, UiTextKey::LogOutput));
            self.log_mode_selector(ui, palette);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(icon_text(
                        icons::ICON_COPY_ALL,
                        ui_text(&self.settings.ui_language, UiTextKey::Copy),
                    ))
                    .clicked()
                {
                    ui.ctx().copy_text(self.visible_runtime_log());
                }
                if ui
                    .button(icon_text(
                        icons::ICON_DELETE,
                        ui_text(&self.settings.ui_language, UiTextKey::Clear),
                    ))
                    .clicked()
                {
                    self.runtime_log.clear();
                    self.runtime_log_scroll_to_bottom = true;
                }
            });
        });
        ui.add_space(10.0);

        // Fill the remaining height so the log card ends at the panel's content
        // bottom (a uniform EDGE_MARGIN gap via the CentralPanel) instead of being
        // forced taller than the window and overflowing below the bottom edge.
        let log_height = (ui.available_height() - (RUNTIME_LOG_TOP_MARGIN + 10.0)).max(0.0);
        let visible_log = self.visible_runtime_log();
        runtime_log_frame(palette).show(ui, |ui| {
            ui.set_min_height(log_height);
            egui::ScrollArea::vertical()
                .id_salt("runtime_log_scroll")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(log_height)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.add_space(RUNTIME_LOG_CONTENT_TOP_PADDING);
                    if self.runtime_log_view == LogViewMode::Debug {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&visible_log)
                                    .monospace()
                                    .color(palette.text),
                            )
                            .selectable(true)
                            .wrap(),
                        );
                    } else {
                        let cards = runtime_log_cards(&self.runtime_log, self.runtime_log_view);
                        if cards.is_empty() {
                            empty_log_state(
                                ui,
                                self.runtime_state,
                                palette,
                                &self.settings.ui_language,
                            );
                        } else {
                            for card in cards {
                                if card.title.trim().is_empty() {
                                    continue;
                                }
                                runtime_log_card(ui, &card, palette);
                                ui.add_space(8.0);
                            }
                        }
                    }
                    let bottom = ui.allocate_response(
                        egui::vec2(ui.available_width(), RUNTIME_LOG_CONTENT_BOTTOM_PADDING),
                        egui::Sense::hover(),
                    );
                    if self.runtime_log_scroll_to_bottom {
                        bottom.scroll_to_me(Some(egui::Align::BOTTOM));
                        self.runtime_log_scroll_to_bottom = false;
                    }
                });
        });
    }

    fn listening_gauge(&self, ui: &mut egui::Ui, palette: UiPalette, max_width: f32) {
        let active = self.audio_capture_active && self.runtime_state == RuntimeState::Running;
        if active {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        let level = audio_meter_level(self.audio_meter_level, self.runtime_state, active);
        let status = if active {
            "Recording"
        } else if self.audio_capture_opening {
            "Opening"
        } else if self.runtime_state == RuntimeState::Running {
            "Ready"
        } else {
            "Idle"
        };
        let gauge_width = (max_width * 0.42).clamp(MIC_GAUGE_MIN_WIDTH, MIC_GAUGE_MAX_WIDTH);
        let label_width = (max_width - gauge_width - 8.0).max(0.0);
        let label_chars = mic_label_char_budget(label_width);
        let audio_device = audio_device_label(&self.active_audio_device, label_chars);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let response = level_gauge(ui, palette, level, active, gauge_width);
            ui.add_sized(
                egui::vec2(label_width, 18.0),
                egui::Label::new(
                    icon_text(icons::ICON_MIC, format!("{status} - {audio_device}"))
                        .size(12.0)
                        .color(if active {
                            palette.accent_blue
                        } else {
                            palette.text_muted
                        }),
                ),
            );
            response.on_hover_text(format!(
                "Audio input: {}\nLive: {}\nCapture: {}\nGate: {}",
                full_audio_device_label(&self.active_audio_device),
                live_audio_level_summary(self.audio_meter_raw_dbfs, self.audio_meter_peak, active,),
                latest_metric_summary(&self.runtime_log, "[cap]"),
                latest_metric_summary(&self.runtime_log, "[gate]")
            ));
        });
    }

    pub(in crate::ui) fn session_panel(&self, ui: &mut egui::Ui, palette: UiPalette) {
        panel_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                icon_text(
                    icons::ICON_TASK_ALT,
                    ui_text(&self.settings.ui_language, UiTextKey::Session),
                )
                .strong(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                metric_box(ui, "Backend", self.backend_summary(), palette);
                metric_box(
                    ui,
                    "Post",
                    empty_as_disabled(&self.settings.post_processor),
                    palette,
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                metric_box(
                    ui,
                    "STT",
                    latest_metric_summary(&self.runtime_log, "[stt]"),
                    palette,
                );
                metric_box(
                    ui,
                    "Inject",
                    latest_log_summary(&self.runtime_log, "[inject] strategy:"),
                    palette,
                );
            });
        });
    }

    pub(in crate::ui) fn log_mode_selector(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for mode in LogViewMode::ALL {
                let selected = self.runtime_log_view == mode;
                let fill = if selected {
                    palette.accent_dark
                } else {
                    palette.surface_bg
                };
                let text = if selected {
                    egui::RichText::new(mode.label(&self.settings.ui_language))
                        .strong()
                        .color(palette.text)
                } else {
                    egui::RichText::new(mode.label(&self.settings.ui_language))
                        .color(palette.text_muted)
                };
                if ui
                    .add_sized(
                        egui::vec2(92.0, 30.0),
                        egui::Button::new(text)
                            .fill(fill)
                            .stroke(egui::Stroke::new(1.0, palette.border_soft)),
                    )
                    .clicked()
                {
                    self.runtime_log_view = mode;
                    self.settings.ui_log_view = mode.id().to_owned();
                    self.runtime_log_scroll_to_bottom = true;
                }
            }
        });
    }

    fn visible_runtime_log(&self) -> String {
        log_view_text(&self.runtime_log, self.runtime_log_view)
    }

    pub(in crate::ui) fn backend_summary(&self) -> &str {
        match self.settings.stt_backend.as_str() {
            "parakeet" => "Parakeet",
            "openai" => self.current_cloud_provider().label(),
            _ => "Whisper",
        }
    }

    pub(in crate::ui) fn stt_detail_summary(&self) -> (&'static str, &'static str, String) {
        match SttBackendMode::from_raw(&self.settings.stt_backend) {
            SttBackendMode::Cloud => (
                ui_text(&self.settings.ui_language, UiTextKey::Model),
                icons::ICON_MODEL_TRAINING,
                compact_label(self.cloud_stt_model_summary(), 28),
            ),
            SttBackendMode::Whisper | SttBackendMode::Parakeet => (
                ui_text(&self.settings.ui_language, UiTextKey::Compute),
                icons::ICON_MEMORY,
                self.compute_summary(),
            ),
        }
    }

    fn cloud_stt_model_summary(&self) -> &str {
        let model = self.settings.stt_model.trim();
        if model.is_empty() {
            self.current_cloud_provider().default_model()
        } else {
            model
        }
    }

    fn compute_summary(&self) -> String {
        format!(
            "{} / {}",
            empty_as_auto(&self.settings.device),
            empty_as_auto(&self.settings.compute_type)
        )
    }
}

fn runtime_log_frame(palette: UiPalette) -> egui::Frame {
    egui::Frame::default()
        .fill(palette.bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin {
            left: 12.0,
            right: 12.0,
            top: RUNTIME_LOG_TOP_MARGIN,
            bottom: 10.0,
        })
}

fn runtime_log_card(ui: &mut egui::Ui, card: &RuntimeLogCard, palette: UiPalette) {
    let (icon, accent) = match card.kind {
        RuntimeLogCardKind::FinalText => (icons::ICON_CHECK_CIRCLE, palette.ok_text),
        RuntimeLogCardKind::Status => (icons::ICON_INFO, palette.accent_blue),
        RuntimeLogCardKind::Diagnostic => (icons::ICON_GRAPHIC_EQ, palette.warn_text),
    };
    let fill = match card.kind {
        RuntimeLogCardKind::FinalText => palette.surface_active_bg,
        RuntimeLogCardKind::Status => palette.surface_bg,
        RuntimeLogCardKind::Diagnostic => palette.header_bg,
    };
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(12.0, 12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                egui::Frame::default()
                    .fill(accent)
                    .rounding(egui::Rounding::same(PILL_RADIUS as f32))
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(4.0, 46.0));
                    });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(icon).size(20.0).color(accent));
                ui.vertical(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&card.title)
                                .size(match card.kind {
                                    RuntimeLogCardKind::FinalText => 17.0,
                                    _ => 14.0,
                                })
                                .strong()
                                .color(palette.text),
                        )
                        .wrap(),
                    );
                    if !card.detail.is_empty() || !card.badge.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            if !card.detail.is_empty() {
                                ui.label(
                                    egui::RichText::new(&card.detail)
                                        .size(12.0)
                                        .color(palette.text_muted),
                                );
                            }
                            if !card.badge.is_empty() {
                                status_pill(ui, &card.badge, accent, palette);
                            }
                        });
                    }
                });
            });
        });
}

fn empty_log_state(ui: &mut egui::Ui, state: RuntimeState, palette: UiPalette, raw_language: &str) {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                icon_text(
                    icons::ICON_MIC,
                    ui_text(raw_language, UiTextKey::NoDictationOutputYet),
                )
                .strong()
                .color(palette.text),
            );
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    ui_text(raw_language, UiTextKey::RuntimeStatus),
                    runtime_state_label(state, raw_language)
                ))
                .size(12.0)
                .color(palette.text_muted),
            );
        });
}

fn status_pill(ui: &mut egui::Ui, label: &str, accent: egui::Color32, palette: UiPalette) {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, accent))
        .rounding(egui::Rounding::same(PILL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(8.0, 3.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(11.0).strong().color(accent));
        });
}

fn level_gauge(
    ui: &mut egui::Ui,
    palette: UiPalette,
    level: f32,
    active: bool,
    width: f32,
) -> egui::Response {
    let size = egui::vec2(width.clamp(MIC_GAUGE_MIN_WIDTH, MIC_GAUGE_MAX_WIDTH), 18.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        8.0,
        palette.header_bg,
        egui::Stroke::new(0.8, palette.border_soft),
    );

    let segments = 18;
    let gap = 2.0;
    let segment_width = (rect.width() - gap * (segments - 1) as f32) / segments as f32;
    let shown_level = if active { level.clamp(0.0, 1.0) } else { 0.0 };

    for index in 0..segments {
        let start_x = rect.left() + index as f32 * (segment_width + gap);
        let segment_rect = egui::Rect::from_min_size(
            egui::pos2(start_x, rect.top() + 3.0),
            egui::vec2(segment_width, rect.height() - 6.0),
        );
        let threshold = (index + 1) as f32 / segments as f32;
        let filled = shown_level >= threshold;
        let color = if filled {
            gauge_color_for_position(index as f32 / (segments - 1) as f32, palette)
        } else {
            palette.border_soft
        };
        painter.rect_filled(segment_rect, 3.0, color);
    }

    response
}

fn gauge_color_for_position(position: f32, palette: UiPalette) -> egui::Color32 {
    if position < 0.68 {
        palette.ok_text
    } else if position < 0.86 {
        palette.warn_text
    } else {
        palette.error_text
    }
}

fn metric_box(ui: &mut egui::Ui, label: &str, value: impl AsRef<str>, palette: UiPalette) {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(CONTROL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(11.0, 8.0))
        .show(ui, |ui| {
            ui.set_min_width(102.0);
            ui.label(
                egui::RichText::new(label)
                    .size(12.0)
                    .color(palette.text_muted),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(value.as_ref())
                        .size(12.0)
                        .color(palette.text),
                )
                .wrap(),
            );
        });
}

fn latest_metric_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(compact_diagnostic_title)
        .unwrap_or_else(|| "No data yet".to_owned())
}

fn latest_log_summary(log: &str, prefix: &str) -> String {
    latest_prefixed_line(log, prefix)
        .map(strip_log_prefix)
        .unwrap_or("No data yet")
        .to_owned()
}

pub(in crate::ui) fn live_audio_level_summary(
    raw_dbfs: Option<f32>,
    peak: Option<f32>,
    active: bool,
) -> String {
    if !active {
        return "Not recording".to_owned();
    }
    match (raw_dbfs, peak) {
        (Some(raw_dbfs), Some(peak)) => format!("raw={raw_dbfs:.1}dBFS  peak={peak:.3}"),
        (Some(raw_dbfs), None) => format!("raw={raw_dbfs:.1}dBFS"),
        _ => "Waiting for audio level".to_owned(),
    }
}

pub(in crate::ui) fn mic_label_char_budget(width: f32) -> usize {
    ((width / 7.0).floor() as usize).clamp(8, 34)
}

pub(in crate::ui) fn audio_device_label(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Input pending".to_owned();
    }
    compact_label(value, max_chars.clamp(8, 34))
}

pub(in crate::ui) fn full_audio_device_label(value: &str) -> &str {
    let value = value.trim();
    if value.is_empty() {
        "Not reported yet"
    } else {
        value
    }
}

pub(in crate::ui) fn empty_as_auto(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Auto"
    } else {
        trimmed
    }
}

pub(in crate::ui) fn empty_as_disabled(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "none" {
        "Disabled"
    } else {
        trimmed
    }
}

fn runtime_status_badge(
    ui: &mut egui::Ui,
    state: RuntimeState,
    palette: UiPalette,
    raw_language: &str,
) {
    let (fill, stroke, text) = match state {
        RuntimeState::Stopped => (palette.surface_bg, palette.border, palette.text_muted),
        RuntimeState::Starting => (
            palette.accent_dark,
            palette.accent_blue,
            palette.accent_blue,
        ),
        RuntimeState::Running => (palette.surface_active_bg, palette.ok_text, palette.ok_text),
    };
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(0.8, stroke))
        .rounding(egui::Rounding::same(PILL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{}: {}",
                    ui_text(raw_language, UiTextKey::Status),
                    runtime_state_label(state, raw_language)
                ))
                .strong()
                .color(text),
            );
        });
}
