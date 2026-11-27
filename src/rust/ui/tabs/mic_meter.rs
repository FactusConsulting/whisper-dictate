use super::runtime_format::{audio_device_label, live_audio_level_summary};
use super::*;
use std::time::Duration;

pub(super) const MIC_GAUGE_MIN_WIDTH: f32 = 86.0;

impl WhisperDictateApp {
    pub(super) fn mic_meter(
        &self,
        ui: &mut egui::Ui,
        palette: UiPalette,
        gauge_width: f32,
        label_width: f32,
        label_chars: usize,
        show_status: bool,
    ) {
        let language = &self.settings.ui_language;
        let settings = self.status_settings();
        let running = self.runtime_state == RuntimeState::Running;
        let opening = running && self.audio_capture_opening;
        let device_error = self.device_error.as_deref();
        let active = running && self.audio_capture_active && !opening && device_error.is_none();
        if active {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }

        let system_default = ui_text(language, UiTextKey::MicSystemDefault);
        let configured = match settings.audio_device.trim() {
            "" => system_default,
            name => name,
        };
        // A device observed on the last recording is not an open stream now.
        // Use the applied selection until the worker starts capturing again.
        let device = if active {
            match self.active_audio_device.trim() {
                "" => ui_text(language, UiTextKey::MicInputPending),
                "System default" => system_default,
                name => name,
            }
        } else {
            configured
        };

        let hint = (running
            && self.worker_ready
            && !active
            && !opening
            && device_error.is_none()
            && self.pipeline_stage.is_none()
            && self.hotkey_conflict.is_none())
        .then_some({
            if settings.toggle_mode {
                (UiTextKey::MicPressShortcut, UiTextKey::MicPressForLevel)
            } else {
                (UiTextKey::MicHoldShortcut, UiTextKey::MicHoldForLevel)
            }
        });
        let tooltip = |ui: &mut egui::Ui| {
            let configured_label = ui_text(language, UiTextKey::MicConfiguredInput);
            ui.label(format!("{configured_label}: {configured}"));
            if active {
                let recording_label = ui_text(language, UiTextKey::MicRecordingInput);
                ui.label(format!("{recording_label}: {device}"));
            }
            if let Some(error) = device_error {
                ui.label(error);
            }
            if let Some((_, help)) = hint {
                ui.label(ui_text(language, help));
            } else {
                ui.label(live_audio_level_summary(
                    self.audio_meter_raw_dbfs,
                    self.audio_meter_peak,
                    active,
                    language,
                ));
            }
            if show_status {
                ui.label(format!(
                    "Capture: {}",
                    self.runtime_log_cache.latest_capture()
                ));
                ui.label(format!("Gate: {}", self.runtime_log_cache.latest_gate()));
            }
        };

        level_gauge(
            ui,
            palette,
            audio_meter_level(self.audio_meter_level, self.runtime_state, active),
            gauge_width,
            hint.map(|(short, _)| ui_text(language, short)),
        )
        .on_hover_ui(tooltip);

        let device_label = audio_device_label(device, label_chars);
        let text = if show_status {
            let status = if active {
                UiTextKey::Recording
            } else if device_error.is_some() {
                UiTextKey::CompactError
            } else if opening {
                UiTextKey::MicOpening
            } else {
                match self.display_runtime_state() {
                    RuntimeState::Stopped => UiTextKey::CompactIdle,
                    RuntimeState::Starting => UiTextKey::Starting,
                    RuntimeState::Running => UiTextKey::Ready,
                }
            };
            format!("{} - {device_label}", ui_text(language, status))
        } else {
            device_label
        };
        let color = if device_error.is_some() {
            palette.error_text
        } else if active {
            palette.accent_blue
        } else {
            palette.text_muted
        };
        ui.add_sized(
            egui::vec2(label_width, 18.0),
            egui::Label::new(
                icon_text(icons::ICON_MIC.codepoint, text)
                    .size(12.0)
                    .color(color),
            )
            .truncate(),
        )
        .on_hover_ui(tooltip);
    }
}

fn level_gauge(
    ui: &mut egui::Ui,
    palette: UiPalette,
    level: f32,
    width: f32,
    idle_hint: Option<&str>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        8.0,
        palette.header_bg,
        egui::Stroke::new(0.8, palette.border_soft),
        egui::StrokeKind::Inside,
    );
    // Use the gauge itself for the idle prompt so it needs no additional space.
    if let Some(hint) = idle_hint {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            hint,
            egui::FontId::proportional(10.0),
            palette.text_muted,
        );
        return response;
    }

    let segments = 18;
    let gap = 2.0;
    let segment_width = (rect.width() - gap * (segments - 1) as f32) / segments as f32;
    for index in 0..segments {
        let start_x = rect.left() + index as f32 * (segment_width + gap);
        let segment_rect = egui::Rect::from_min_size(
            egui::pos2(start_x, rect.top() + 3.0),
            egui::vec2(segment_width, rect.height() - 6.0),
        );
        let threshold = (index + 1) as f32 / segments as f32;
        let color = if level >= threshold {
            gauge_color_for_position(index as f32 / (segments - 1) as f32, palette)
        } else {
            palette.border_soft
        };
        painter.rect_filled(segment_rect, 3.0, color);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::test_app;

    fn paint_live_meter(app: &mut WhisperDictateApp) -> (Vec<String>, usize) {
        let palette = ui_palette(&app.settings.ui_theme);
        let mut frame = egui::Context::default().run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(520.0, 420.0),
                )),
                ..Default::default()
            },
            |ui| app.runtime_tab(ui),
        );
        frame.textures_delta.clear();
        let mut labels = Vec::new();
        let mut filled_bars = 0;
        for paint in frame.shapes {
            match paint.shape {
                egui::Shape::Text(text) => labels.push(
                    text.galley
                        .rows
                        .iter()
                        .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
                        .collect(),
                ),
                egui::Shape::Rect(rect)
                    if rect.fill == palette.ok_text && (rect.rect.height() - 12.0).abs() < 0.1 =>
                {
                    filled_bars += 1;
                }
                _ => {}
            }
        }
        (labels, filled_bars)
    }

    fn worker_event(app: &mut WhisperDictateApp, payload: serde_json::Value) {
        app.handle_worker_event(&WorkerEvent {
            event: payload["event"].as_str().unwrap().to_owned(),
            state: payload["state"].as_str().map(str::to_owned),
            payload,
        });
    }

    fn assert_painted(labels: &[String], expected: &str) {
        assert!(
            labels.iter().any(|text| text.contains(expected)),
            "expected {expected:?} in {labels:?}",
        );
    }

    #[test]
    fn ready_live_meter_paints_applied_microphone_and_shortcut_without_a_capture() {
        let mut app = test_app(AppSettings {
            audio_device: "USB Mic".to_owned(),
            ui_language: "en".to_owned(),
            toggle_mode: false,
            ..AppSettings::default()
        });
        app.runtime_state = RuntimeState::Running;
        // A pending settings edit is not used by the running worker yet.
        app.settings.audio_device = "Pending Mic".to_owned();
        worker_event(
            &mut app,
            serde_json::json!({"event":"status", "state":"ready"}),
        );

        let (labels, bars) = paint_live_meter(&mut app);
        assert_painted(&labels, "Ready - USB Mic");
        assert_painted(&labels, "Hold shortcut");
        assert_eq!(bars, 0);

        app.applied_settings.audio_device.clear();
        app.applied_settings.toggle_mode = true;
        let (labels, bars) = paint_live_meter(&mut app);
        assert_painted(&labels, "Ready - System default");
        assert_painted(&labels, "Press shortcut");
        assert_eq!(bars, 0);
    }

    #[test]
    fn live_meter_paints_worker_levels_and_capture_device_only_during_capture() {
        let mut app = test_app(AppSettings {
            audio_device: "USB Mic".to_owned(),
            ui_language: "en".to_owned(),
            ..AppSettings::default()
        });
        app.runtime_state = RuntimeState::Running;
        worker_event(
            &mut app,
            serde_json::json!({"event":"status", "state":"opening"}),
        );
        let (labels, bars) = paint_live_meter(&mut app);
        assert_painted(&labels, "Opening - USB Mic");
        assert_eq!(bars, 0);

        worker_event(
            &mut app,
            serde_json::json!({"event":"status", "state":"audio-fallback", "audio_device":"Desk Mic"}),
        );
        worker_event(
            &mut app,
            serde_json::json!({"event":"status", "state":"recording"}),
        );
        for (level, expected_bars) in [(0.25, 4), (0.6, 10)] {
            worker_event(
                &mut app,
                serde_json::json!({"event":"audio", "state":"recording", "level":level}),
            );
            let (labels, bars) = paint_live_meter(&mut app);
            assert_painted(&labels, "Recording - Desk Mic");
            assert_eq!(bars, expected_bars);
        }

        worker_event(
            &mut app,
            serde_json::json!({"event":"status", "state":"ready"}),
        );
        let (labels, bars) = paint_live_meter(&mut app);
        assert_painted(&labels, "Ready - USB Mic");
        assert_eq!(bars, 0);

        worker_event(
            &mut app,
            serde_json::json!({
                "event":"status", "state":"error", "reason":"device_unusable",
                "error":"Cannot open USB Mic"
            }),
        );
        let (labels, bars) = paint_live_meter(&mut app);
        assert_painted(&labels, "Error - USB Mic");
        assert_painted(&labels, "Microphone unavailable");
        assert_eq!(bars, 0);
    }
}
