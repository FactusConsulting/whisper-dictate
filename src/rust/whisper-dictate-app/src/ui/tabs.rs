use super::*;
use egui_material_icons::icons;

const MIC_INDICATOR_MAX_WIDTH: f32 = 330.0;
const MIC_INDICATOR_MIN_WIDTH: f32 = 150.0;
const MIC_GAUGE_MAX_WIDTH: f32 = 150.0;
const MIC_GAUGE_MIN_WIDTH: f32 = 86.0;
const RUNTIME_LOG_TOP_MARGIN: f32 = 16.0;
const RUNTIME_LOG_CONTENT_TOP_PADDING: f32 = 10.0;
const RUNTIME_LOG_CONTENT_BOTTOM_PADDING: f32 = 14.0;
const RUNTIME_LOG_VERTICAL_CHROME: f32 = 112.0;
const RUNTIME_LOG_MIN_HEIGHT: f32 = 300.0;
const SETTINGS_FOOTER_HEIGHT: f32 = 264.0;
const SETTINGS_FOOTER_CHROME_HEIGHT: f32 = 18.0;
const SETTINGS_MESSAGES_TOP_GAP: f32 = 14.0;
const SETTINGS_MESSAGES_BOTTOM_GAP: f32 = 20.0;
const SETTINGS_MESSAGES_MAX_HEIGHT: f32 = 88.0;

impl WhisperDictateApp {
    pub(super) fn sidebar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        ui.set_min_height(ui.available_height());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(icons::ICON_KEYBOARD_VOICE)
                    .size(25.0)
                    .color(palette.accent_blue),
            );
            ui.label(
                egui::RichText::new("whisper-dictate")
                    .size(20.0)
                    .strong()
                    .color(palette.accent_blue),
            );
        });
        ui.label(
            icon_text(
                icons::ICON_TUNE,
                ui_text(&self.settings.ui_language, UiTextKey::SidebarSubtitle),
            )
            .size(12.0)
            .color(palette.text_muted),
        );
        ui.add_space(18.0);

        for tab in Tab::ALL {
            let selected = self.selected_tab == tab;
            if nav_button(
                ui,
                selected,
                tab.icon(),
                tab.label(&self.settings.ui_language),
                palette,
            )
            .clicked()
            {
                self.selected_tab = tab;
            }
            ui.add_space(5.0);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.label(
                egui::RichText::new(format!("v{}", self.app_version))
                    .size(12.0)
                    .color(palette.text_muted),
            );
            ui.add_space(8.0);
            if ui
                .add_enabled_ui(self.background_task.is_none(), |ui| {
                    ui.add_sized(
                        [ui.available_width(), 34.0],
                        egui::Button::new(icon_text(
                            icons::ICON_BUILD,
                            ui_text(&self.settings.ui_language, UiTextKey::InstallRepair),
                        )),
                    )
                })
                .inner
                .on_hover_text("Install or repair the local runtime environment.")
                .clicked()
            {
                self.run_install();
            }
            ui.add_space(6.0);
            if ui
                .add_sized(
                    [ui.available_width(), 34.0],
                    egui::Button::new(icon_text(
                        icons::ICON_HEALTH_AND_SAFETY,
                        ui_text(&self.settings.ui_language, UiTextKey::Doctor),
                    )),
                )
                .clicked()
            {
                self.run_doctor();
            }
            if ui
                .add_sized(
                    [ui.available_width(), 34.0],
                    egui::Button::new(icon_text(
                        icons::ICON_REFRESH,
                        ui_text(&self.settings.ui_language, UiTextKey::ReloadConfig),
                    )),
                )
                .on_hover_text("Reload the config file from disk.")
                .clicked()
            {
                self.reload_settings();
            }
            ui.add_space(10.0);
            sidebar_save_state(
                ui,
                self.has_unsaved_settings(),
                palette,
                &self.settings.ui_language,
            );
        });
    }

    pub(super) fn top_status_bar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let controls_width = top_status_controls_width();
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(
                    (ui.available_width() - controls_width).max(300.0),
                    ui.available_height(),
                ),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    status_card(
                        ui,
                        ui_text(&self.settings.ui_language, UiTextKey::Status),
                        icons::ICON_RADIO_BUTTON_CHECKED,
                        runtime_state_label(self.runtime_state, &self.settings.ui_language),
                        runtime_state_color(self.runtime_state, palette),
                        palette,
                    );
                    status_card(
                        ui,
                        ui_text(&self.settings.ui_language, UiTextKey::Backend),
                        icons::ICON_MODEL_TRAINING,
                        self.backend_summary(),
                        palette.accent_blue,
                        palette,
                    );
                    let (detail_label, detail_icon, detail_value) = self.stt_detail_summary();
                    status_card_wide(
                        ui,
                        detail_label,
                        detail_icon,
                        detail_value,
                        palette.accent_blue,
                        palette,
                    );
                    if let Some(label) = self.background_task_label {
                        status_card(
                            ui,
                            ui_text(&self.settings.ui_language, UiTextKey::Task),
                            icons::ICON_PENDING_ACTIONS,
                            label,
                            palette.warn_text,
                            palette,
                        );
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.global_controls(ui, palette);
            });
        });
    }

    pub(super) fn global_controls(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        let is_stopped = self.runtime_state == RuntimeState::Stopped;
        let is_active = !is_stopped;

        if ui
            .add_enabled(
                is_active,
                egui::Button::new(
                    icon_text(
                        icons::ICON_STOP,
                        ui_text(&self.settings.ui_language, UiTextKey::Stop),
                    )
                    .strong(),
                )
                .fill(palette.error_text)
                .min_size(egui::vec2(78.0, 34.0)),
            )
            .clicked()
        {
            self.stop_runtime();
        }
        if ui
            .add_enabled(
                is_stopped,
                egui::Button::new(
                    icon_text(
                        icons::ICON_PLAY_ARROW,
                        ui_text(&self.settings.ui_language, UiTextKey::Start),
                    )
                    .strong(),
                )
                .fill(palette.accent_dark)
                .min_size(egui::vec2(88.0, 34.0)),
            )
            .clicked()
        {
            self.start_runtime();
        }
    }

    pub(super) fn runtime_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        let height = ui.available_height();
        self.live_dictation_panel(ui, palette, height);
    }

    fn live_dictation_panel(&mut self, ui: &mut egui::Ui, palette: UiPalette, height: f32) {
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

        let log_height = (height - RUNTIME_LOG_VERTICAL_CHROME).max(RUNTIME_LOG_MIN_HEIGHT);
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

    fn session_panel(&self, ui: &mut egui::Ui, palette: UiPalette) {
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

    fn log_mode_selector(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
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

    fn backend_summary(&self) -> &str {
        match self.settings.stt_backend.as_str() {
            "parakeet" => "Parakeet",
            "openai" => self.current_cloud_provider().label(),
            _ => "Whisper",
        }
    }

    pub(super) fn stt_detail_summary(&self) -> (&'static str, &'static str, String) {
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

    pub(super) fn settings_panel(&mut self, ui: &mut egui::Ui, body: fn(&mut Self, &mut egui::Ui)) {
        let footer_height = SETTINGS_FOOTER_HEIGHT;
        let body_height =
            (ui.available_height() - footer_height - SETTINGS_FOOTER_CHROME_HEIGHT).max(0.0);

        egui::ScrollArea::vertical()
            .id_salt(format!("settings_body_{:?}", self.selected_tab))
            .auto_shrink([false, false])
            .max_height(body_height)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                body(self, ui);
            });

        ui.separator();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), footer_height),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                self.settings_actions(ui);
                ui.add_space(SETTINGS_MESSAGES_TOP_GAP);
                self.settings_messages(ui);
                ui.add_space(SETTINGS_MESSAGES_BOTTOM_GAP);
            },
        );
    }

    fn settings_actions(&mut self, ui: &mut egui::Ui) {
        let is_dirty = self.has_unsaved_settings();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
            let mut save_button = egui::Button::new(if is_dirty {
                icon_text(
                    icons::ICON_SAVE,
                    ui_text(&self.settings.ui_language, UiTextKey::SaveSettingsDirty),
                )
                .strong()
            } else {
                icon_text(
                    icons::ICON_SAVE,
                    ui_text(&self.settings.ui_language, UiTextKey::SaveSettings),
                )
            });
            if is_dirty {
                save_button = save_button.fill(ui.visuals().selection.bg_fill);
            }
            if ui
                .add_enabled(is_dirty, save_button)
                .on_hover_text("Save changed settings and any edited cloud API key.")
                .clicked()
            {
                self.save_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_REFRESH,
                    ui_text(&self.settings.ui_language, UiTextKey::ReloadConfig),
                ))
                .on_hover_text("Reload the config file from disk.")
                .clicked()
            {
                self.reload_settings();
            }
            if ui
                .button(icon_text(
                    icons::ICON_REFRESH,
                    ui_text(&self.settings.ui_language, UiTextKey::ResetPage),
                ))
                .on_hover_text("Reset only the settings shown on this page to the built-in defaults. Save to keep the reset.")
                .clicked()
            {
                self.reset_current_tab_settings();
            }
            if is_dirty {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    ui_text(&self.settings.ui_language, UiTextKey::UnsavedChanges),
                );
            }
        });
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            let palette = ui_palette(&self.settings.ui_theme);
            let config_chars = ((ui.available_width() / 8.0).floor() as usize).clamp(38, 92);
            ui.label(egui::RichText::new("Config:").color(palette.text_muted));
            ui.add(
                egui::Label::new(
                    egui::RichText::new(compact_label(&self.config_path, config_chars))
                        .monospace()
                        .color(palette.text),
                )
                .wrap(),
            )
            .on_hover_text(&self.config_path);
        });
    }

    fn reset_current_tab_settings(&mut self) {
        let tab = self.selected_tab;
        reset_tab_settings(&mut self.settings, tab);
        match tab {
            Tab::Speech => self.reload_stt_api_key(),
            Tab::Post => self.reload_post_api_key(),
            _ => {}
        }
        self.settings_status = format!(
            "Reset {} settings to defaults. Save settings to keep the reset.",
            tab.label(&self.settings.ui_language)
        );
    }

    fn settings_messages(&self, ui: &mut egui::Ui) {
        let mut messages = Vec::new();
        if !self.settings_status.trim().is_empty() {
            messages.push(self.settings_status.as_str());
        }
        match self.selected_tab {
            Tab::Speech if !self.stt_api_key_status.trim().is_empty() => {
                messages.push(self.stt_api_key_status.as_str());
            }
            Tab::Post if !self.post_api_key_status.trim().is_empty() => {
                messages.push(self.post_api_key_status.as_str());
            }
            _ => {}
        }

        let palette = ui_palette(&self.settings.ui_theme);
        panel_frame(palette).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(112.0);
            ui.strong(ui_text(&self.settings.ui_language, UiTextKey::Messages));
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt(format!("settings_messages_{:?}", self.selected_tab))
                .auto_shrink([false, false])
                .max_height(SETTINGS_MESSAGES_MAX_HEIGHT)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    if messages.is_empty() {
                        ui.label(
                            egui::RichText::new(ui_text(
                                &self.settings.ui_language,
                                UiTextKey::NoMessages,
                            ))
                            .color(ui.visuals().weak_text_color()),
                        );
                    }
                    for message in messages {
                        status_label(ui, message, palette);
                    }
                });
        });
    }

    pub(super) fn core_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Speech recognition");
        let backend = SttBackendMode::from_raw(&self.settings.stt_backend);
        let mut provider_id = self.current_cloud_provider().id().to_owned();
        settings_grid("core_settings")
            .show(ui, |ui| {
                combo_help_labeled(
                    ui,
                    "Speech engine",
                    &mut self.settings.stt_backend,
                    STT_BACKEND_OPTIONS,
                    "Choose the transcription engine. Cloud STT can use either Groq or OpenAI; the saved config value is still openai for compatibility with OpenAI-compatible APIs.",
                );
                ui.end_row();
                ui.strong("Local Whisper");
                ui.label("Used only when STT backend is whisper.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Whisper,
                    "Whisper model",
                    &mut self.settings.model,
                    WHISPER_MODELS,
                    "Local faster-whisper model used only with STT backend = whisper.",
                );
                ui.end_row();
                ui.strong("Local NVIDIA Parakeet");
                ui.label("Used only when STT backend is parakeet.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Parakeet,
                    "Parakeet model",
                    &mut self.settings.parakeet_model,
                    PARAKEET_MODELS,
                    "Local NVIDIA NeMo Parakeet model used only with STT backend = parakeet.",
                );
                ui.end_row();
                ui.strong("Cloud STT");
                ui.label("Used only when Speech engine is Cloud STT.");
                ui.end_row();
                combo_enabled_labeled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT provider",
                    &mut provider_id,
                    CLOUD_PROVIDER_OPTIONS,
                    "Cloud transcription provider. Groq and OpenAI both use OpenAI-compatible API shapes, but each has its own URL, API key and model list.",
                );
                if let Some(provider) = CloudProvider::from_raw(&provider_id) {
                    if provider != self.current_cloud_provider() {
                        self.set_cloud_provider(provider);
                    }
                }
                ui.end_row();
                let provider = self.current_cloud_provider();
                combo_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT model",
                    &mut self.settings.stt_model,
                    provider.model_options(),
                    "Remote transcription model for the selected cloud provider. OpenAI options include gpt-4o-mini-transcribe, gpt-4o-transcribe and whisper-1.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API URL",
                    &mut self.settings.stt_base_url,
                    "Base URL for the selected cloud transcription provider.",
                );
                text_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT timeout ms",
                    &mut self.settings.stt_timeout_ms,
                    "Network timeout for cloud transcription requests.",
                );
                password_enabled(
                    ui,
                    backend == SttBackendMode::Cloud,
                    "Cloud STT API key",
                    &mut self.stt_api_key_input,
                    &mut self.stt_api_key_reveal_until,
                    "Stored in the OS credential store and passed to the worker as VOICEPI_STT_API_KEY.",
                );
                if backend == SttBackendMode::Cloud {
                    ui.label("");
                    ui.horizontal(|ui| {
                        if ui
                            .button("Save API key")
                            .on_hover_text(
                                "Stores the current API key in the platform credential store and remembers the selected cloud provider. Clear the field and save to remove it.",
                            )
                            .clicked()
                        {
                            self.save_stt_api_key_now();
                        }
                        if ui
                            .add_enabled(
                                self.background_task.is_none(),
                                egui::Button::new("Test cloud API"),
                            )
                            .on_hover_text("Checks the selected provider key and model from Rust without starting the Python worker.")
                            .clicked()
                        {
                            self.run_cloud_api_check();
                        }
                        if provider == CloudProvider::Groq
                            && ui
                                .link("Open Groq API keys")
                                .on_hover_text("Open the Groq API key page.")
                                .clicked()
                        {
                            match open_url(provider.key_url()) {
                                Ok(()) => {
                                    self.stt_api_key_status =
                                        "Opened Groq API keys page.".to_owned();
                                }
                                Err(err) => {
                                    self.stt_api_key_status =
                                        format!("Could not open Groq API keys page: {err}");
                                }
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    let key_help = if self.saved_stt_api_key_input.trim().is_empty() {
                        "Paste an API key, then save it. Cloud STT sends recorded audio to the configured provider."
                    } else {
                        "Saved key loaded. Edit and save to replace it, or clear the field and save to remove it."
                    };
                    ui.label(key_help).on_hover_text(
                        "API keys are stored in the platform credential store when possible. If that fails, the app reports the fallback location in the runtime log.",
                    );
                    ui.end_row();
                }
                ui.strong("Local runtime");
                ui.label("Used only by local Whisper and Parakeet.");
                ui.end_row();
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Device",
                    &mut self.settings.device,
                    &["auto", "cuda", "cpu"],
                    "Local inference device. auto chooses CUDA when available, otherwise CPU.",
                );
                combo_enabled(
                    ui,
                    backend != SttBackendMode::Cloud,
                    "Compute type",
                    &mut self.settings.compute_type,
                    &["", "int8_float16", "float16", "bfloat16", "float32", "int8"],
                    "Local model precision/performance mode. Leave empty for backend default.",
                );
                ui.strong("Dictation controls");
                ui.label("Applies to local and cloud speech engines.");
                ui.end_row();
                combo_help_labeled(
                    ui,
                    "Language",
                    &mut self.settings.lang,
                    &[
                        ("", "Auto"),
                        ("da", "Danish"),
                        ("en", "English"),
                        ("de", "German"),
                        ("fr", "French"),
                        ("sv", "Swedish"),
                        ("nb", "Norwegian"),
                        ("nl", "Dutch"),
                        ("es", "Spanish"),
                        ("it", "Italian"),
                    ],
                    "Spoken language hint. Auto lets the backend autodetect when supported.",
                );
                if !cfg!(windows) {
                    combo_help_labeled(
                        ui,
                        "Linux keyboard layout",
                        &mut self.settings.xkb_layout,
                        &[
                            ("", "Auto"),
                            ("dk", "Danish"),
                            ("no", "Norwegian"),
                            ("se", "Swedish"),
                            ("de", "German"),
                            ("pt", "Portuguese"),
                            ("br", "Brazilian"),
                            ("us", "US English"),
                        ],
                        "Wayland ydotool/XKB layout used for direct text injection on Linux. Auto detects GNOME layout when possible.",
                    );
                }
                text_help(
                    ui,
                    "Hotkey",
                    &mut self.settings.key,
                    "Hold-to-talk key or chord, for example ctrl_r or shift_l+ctrl_l.",
                );
                text_help(
                    ui,
                    "Quit key",
                    &mut self.settings.quit_key,
                    "Global key used to quit the worker after Quit count presses. Examples: esc, f12, q.",
                );
                text_help(
                    ui,
                    "Quit count",
                    &mut self.settings.quit_count,
                    "Number of consecutive quit-key presses required to stop the worker. 0 disables it.",
                );
                text_help(
                    ui,
                    "Quit window ms",
                    &mut self.settings.quit_window_ms,
                    "Maximum time window for consecutive quit-key presses.",
                );
            });
    }

    pub(super) fn quality_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Quality");
        settings_grid("quality_settings")
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Beam size",
                    &mut self.settings.beam_size,
                    "Whisper beam search width. Higher can improve accuracy but costs more compute.",
                );
                text_help(
                    ui,
                    "Temperature ladder",
                    &mut self.settings.temperature,
                    "Comma-separated Whisper fallback temperatures, for example 0.0,0.2.",
                );
                text_help(
                    ui,
                    "Context min seconds",
                    &mut self.settings.context_min_seconds,
                    "Minimum utterance length before passing previous context/prompt hints to Whisper.",
                );
                text_help(
                    ui,
                    "Parakeet min seconds",
                    &mut self.settings.parakeet_min_seconds,
                    "Minimum captured audio length before Parakeet transcription is attempted.",
                );
                text_help(
                    ui,
                    "Release tail ms",
                    &mut self.settings.release_tail_ms,
                    "Extra audio kept after releasing the hotkey so word endings are not clipped.",
                );
                text_help(
                    ui,
                    "VAD threshold",
                    &mut self.settings.vad_threshold,
                    "Voice activity detection sensitivity. Lower is more sensitive, higher rejects more noise.",
                );
                text_help(
                    ui,
                    "VAD min silence ms",
                    &mut self.settings.vad_min_silence_ms,
                    "Silence duration used by VAD to split or end speech.",
                );
                text_help(
                    ui,
                    "VAD speech pad ms",
                    &mut self.settings.vad_speech_pad_ms,
                    "Audio padding kept around detected speech so soft first and last syllables are not trimmed.",
                );
                text_help(
                    ui,
                    "Target dBFS",
                    &mut self.settings.target_dbfs,
                    "Audio normalization target loudness before transcription.",
                );
                text_help(
                    ui,
                    "Min input dBFS",
                    &mut self.settings.min_input_dbfs,
                    "Minimum raw microphone loudness accepted as speech candidate.",
                );
                text_help(
                    ui,
                    "Min SNR dB",
                    &mut self.settings.min_snr_db,
                    "Minimum signal-to-noise ratio accepted before transcription.",
                );
                checkbox_help(
                    ui,
                    "Audio ducking",
                    &mut self.settings.audio_ducking,
                    "Windows-only: temporarily lowers other app audio while recording, then restores it.",
                );
                text_help(
                    ui,
                    "Audio ducking level",
                    &mut self.settings.audio_ducking_level,
                    "Target volume for other apps while recording. 0.25 means 25%.",
                );
            });
        let show_initial_prompt_help = label_with_help(
            ui,
            "Initial prompt",
            "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.",
        );
        inline_help(ui, show_initial_prompt_help, "Optional prompt sent to Whisper for vocabulary and style hints. Keep it short; dictionary terms are capped separately.");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.initial_prompt)
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }

    pub(super) fn dictionary_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Dictionary");
        ui.horizontal(|ui| {
            if ui.button("Ensure file").clicked() {
                self.ensure_dictionary();
            }
            if ui.button("Open").clicked() {
                self.open_dictionary();
            }
            if ui.button("Preview").clicked() {
                self.preview_dictionary();
            }
        });
        settings_grid("dictionary_settings")
            .show(ui, |ui| {
                text_help(
                    ui,
                    "Dictionary path",
                    &mut self.settings.dictionary,
                    "JSON dictionary used for prompt terms and deterministic replacements.",
                );
                checkbox_help(
                    ui,
                    "Dictionary enabled",
                    &mut self.settings.dictionary_enabled,
                    "Enable prompt-term injection and replacement cleanup from the dictionary.",
                );
                text_help(
                    ui,
                    "Max prompt terms",
                    &mut self.settings.dictionary_max_terms,
                    "Maximum number of dictionary terms included in the model prompt.",
                );
                text_help(
                    ui,
                    "Prompt char cap",
                    &mut self.settings.dictionary_prompt_chars,
                    "Maximum characters used by dictionary prompt terms to avoid over-steering the model.",
                );
            });
        if !self.dictionary_preview.is_empty() {
            ui.label("Prompt preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.dictionary_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    pub(super) fn output_tab(&mut self, ui: &mut egui::Ui) {
        let palette = ui_palette(&self.settings.ui_theme);
        ui.heading("Output");
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            section_label(ui, "Log view", palette);
            self.log_mode_selector(ui, palette);
            ui.add_space(12.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiTheme),
                palette,
            );
            let ui_language = self.settings.ui_language.clone();
            theme_toggle(ui, &mut self.settings.ui_theme, palette, &ui_language);
            ui.add_space(12.0);
            section_label(
                ui,
                ui_text(&self.settings.ui_language, UiTextKey::UiLanguage),
                palette,
            );
            language_toggle(ui, &mut self.settings.ui_language, palette);
        });
        ui.add_space(14.0);
        self.session_panel(ui, palette);
        ui.add_space(14.0);
        ui.separator();
        ui.add_space(8.0);
        settings_grid("output_settings")
            .show(ui, |ui| {
                combo_help(
                    ui,
                    "Inject mode",
                    &mut self.settings.inject_mode,
                    &["auto", "type", "paste", "print"],
                    "How text is inserted into the focused app. auto chooses the safest available strategy.",
                );
                combo_help(
                    ui,
                    "Format commands",
                    &mut self.settings.format_commands,
                    &["off", "en", "da", "both"],
                    "Enable spoken formatting commands such as punctuation and new lines.",
                );
                checkbox_help(
                    ui,
                    "JSON stdout",
                    &mut self.settings.inject_json,
                    "Emit structured JSON events to stdout in addition to normal logs.",
                );
                text_help(
                    ui,
                    "Metrics JSONL",
                    &mut self.settings.metrics_jsonl,
                    "Optional path for appending transcription metrics as JSONL.",
                );
                text_help(
                    ui,
                    "Command hook",
                    &mut self.settings.command_hook,
                    "Optional command run after accepted utterances for advanced automation.",
                );
                text_help(
                    ui,
                    "Command hook timeout ms",
                    &mut self.settings.command_hook_timeout_ms,
                    "Maximum time the command hook may run before it is treated as timed out.",
                );
                checkbox_help(
                    ui,
                    "History enabled",
                    &mut self.settings.history_enabled,
                    "Store local utterance history for review, copying and dictionary suggestions.",
                );
                text_help(
                    ui,
                    "History JSONL",
                    &mut self.settings.history_jsonl,
                    "Optional override path for local utterance history JSONL.",
                );
                checkbox_help(
                    ui,
                    "Local only",
                    &mut self.settings.local_only,
                    "Block network-backed STT/post-processing providers when enabled.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_DEBUG",
                    &mut self.settings.debug,
                    "Print the effective configuration at worker startup.",
                );
                checkbox_help(
                    ui,
                    "VOICEPI_STT_DEBUG",
                    &mut self.settings.stt_debug,
                    "Enable extra backend transcription diagnostics.",
                );
                text_help(
                    ui,
                    "UI text scale",
                    &mut self.settings.ui_text_scale,
                    "Scale all text in this settings UI. Use 1.0 for default, 1.15 for larger text, or 1.3 for high-DPI displays.",
                );
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Preview history").clicked() {
                self.preview_history();
            }
            if ui.button("Open history").clicked() {
                self.open_history();
            }
            if ui.button("Preview metrics").clicked() {
                self.preview_metrics();
            }
            if ui.button("Open metrics").clicked() {
                self.open_metrics();
            }
        });
        if !self.history_preview.is_empty() {
            ui.label("History preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.history_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
        if !self.metrics_preview.is_empty() {
            ui.label("Metrics preview");
            ui.add(
                egui::TextEdit::multiline(&mut self.metrics_preview)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        }
    }

    pub(super) fn post_processing_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Post-processing");
        let previous_post_provider = PostProvider::from_settings(&self.settings);
        settings_grid("post_processing_settings")
            .show(ui, |ui| {
                combo_help_labeled(
                    ui,
                    "Post processor",
                    &mut self.settings.post_processor,
                    POST_PROCESSOR_OPTIONS,
                    "Optional second text pass after speech recognition, dictionary replacements and before final injection. none disables it; ollama uses a local chat model; groq/openai send the dictated text to a cloud chat model for cleanup or rewriting.",
                );
                combo_help(
                    ui,
                    "Post mode",
                    &mut self.settings.post_mode,
                    &[
                        "raw", "clean", "prompt", "terminal", "slack", "email", "bullets",
                    ],
                    "Controls what the post processor is allowed to do. raw bypasses post-processing and does not call the model; clean fixes punctuation/casing and obvious transcription artifacts; prompt rewrites for coding agents; terminal preserves commands and paths; slack/email/bullets format for those destinations.",
                );
                match self.settings.post_processor.as_str() {
                    "groq" => combo_help_labeled(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        GROQ_POST_MODELS,
                        "Groq chat model used for the optional final text cleanup pass. The list labels show the recommended Danish cleanup default, faster alternatives, reasoning models and preview models. STT Whisper models are not listed here because they transcribe audio, not text.",
                    ),
                    "openai" => combo_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        OPENAI_POST_MODELS,
                        "OpenAI chat model used for the optional final text cleanup pass.",
                    ),
                    _ => text_help(
                        ui,
                        "Post model",
                        &mut self.settings.post_model,
                        "Model name for post-processing, for example an Ollama model.",
                    ),
                }
                text_help(
                    ui,
                    "Post base URL",
                    &mut self.settings.post_base_url,
                    "Base URL for the post-processing provider. Ollama normally uses http://localhost:11434; Groq/OpenAI use OpenAI-compatible HTTPS endpoints.",
                );
                text_help(
                    ui,
                    "Post timeout ms",
                    &mut self.settings.post_timeout_ms,
                    "Maximum time allowed for post-processing.",
                );
                text_help(
                    ui,
                    "Post max input chars",
                    &mut self.settings.post_max_input_chars,
                    "Maximum transcript length sent to the post-processor.",
                );
                text_help(
                    ui,
                    "Post max output chars",
                    &mut self.settings.post_max_output_chars,
                    "Maximum accepted length of post-processed output.",
                );
                if let Some(provider) = PostProvider::from_settings(&self.settings) {
                    password_enabled(
                        ui,
                        true,
                        "Post API key",
                        &mut self.post_api_key_input,
                        &mut self.post_api_key_reveal_until,
                        "Optional separate API key for cloud post-processing. Stored in the OS credential store as VOICEPI_POST_API_KEY. If empty, the worker falls back to the Cloud STT API key when available.",
                    );
                    ui.label("");
                    ui.horizontal(|ui| {
                        if ui
                            .button("Save post API key")
                            .on_hover_text("Stores only the post-processing API key in the OS credential store.")
                            .clicked()
                        {
                            self.save_post_api_key_now();
                        }
                        if ui
                            .button("Test post API")
                            .on_hover_text("Sends a tiny chat-completions request to the selected post-processing provider and model.")
                            .clicked()
                        {
                            self.run_post_api_check();
                        }
                        if provider == PostProvider::Groq
                            && ui
                                .link("Open Groq API keys")
                                .on_hover_text("Open the Groq API key page.")
                                .clicked()
                        {
                            match open_url(provider.key_url()) {
                                Ok(()) => {
                                    self.post_api_key_status =
                                        "Opened Groq API keys page.".to_owned();
                                }
                                Err(err) => {
                                    self.post_api_key_status =
                                        format!("Could not open Groq API keys page: {err}");
                                }
                            }
                        }
                    });
                    ui.end_row();
                    ui.label("");
                    let key_help = if self.saved_post_api_key_input.trim().is_empty() {
                        "Optional separate post-processing key. Leave empty to reuse the Cloud STT key when available."
                    } else {
                        "Saved post-processing key loaded. Edit and save to replace it, or clear the field and save to remove it."
                    };
                    ui.label(key_help).on_hover_text(
                        "Post-processing API keys are stored in the platform credential store when possible. If that fails, the app reports the fallback location in the runtime log.",
                    );
                    ui.end_row();
                }
                checkbox_help(
                    ui,
                    "Cloud redaction",
                    &mut self.settings.post_redact,
                    "Before OpenAI-compatible post-processing, replace sensitive local text with placeholders and restore it afterward when possible.",
                );
                text_help(
                    ui,
                    "Redaction terms",
                    &mut self.settings.post_redact_terms,
                    "Comma-separated names or terms to redact before cloud post-processing. Emails, phone numbers and common tokens are detected automatically.",
                );
            });
        if PostProvider::from_settings(&self.settings) != previous_post_provider {
            self.reload_post_api_key();
        }
    }

    pub(super) fn profiles_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        let show_profiles_help = label_with_help(
            ui,
            "Profiles JSON",
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        inline_help(
            ui,
            show_profiles_help,
            "Advanced JSON profile definitions. Save persists valid JSON profiles into the config file.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.profiles_json)
                .font(egui::TextStyle::Monospace)
                .desired_rows(22)
                .desired_width(f32::INFINITY),
        );
    }
}

fn sidebar_save_state(ui: &mut egui::Ui, is_dirty: bool, palette: UiPalette, raw_language: &str) {
    inset_panel_frame(palette).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        if is_dirty {
            ui.label(
                icon_text(
                    icons::ICON_ERROR,
                    ui_text(raw_language, UiTextKey::UnsavedChanges),
                )
                .color(palette.warn_text),
            );
        } else {
            ui.label(
                icon_text(
                    icons::ICON_CHECK_CIRCLE,
                    ui_text(raw_language, UiTextKey::SettingsSaved),
                )
                .color(palette.ok_text),
            );
        }
    });
}

pub(super) fn reset_tab_settings(settings: &mut AppSettings, tab: Tab) {
    let defaults = AppSettings::default();
    match tab {
        Tab::Log => {}
        Tab::Speech => {
            settings.stt_backend = defaults.stt_backend;
            settings.model = defaults.model;
            settings.parakeet_model = defaults.parakeet_model;
            settings.stt_provider = defaults.stt_provider;
            settings.stt_model = defaults.stt_model;
            settings.stt_base_url = defaults.stt_base_url;
            settings.stt_timeout_ms = defaults.stt_timeout_ms;
            settings.device = defaults.device;
            settings.compute_type = defaults.compute_type;
            settings.lang = defaults.lang;
            settings.xkb_layout = defaults.xkb_layout;
            settings.key = defaults.key;
            settings.quit_key = defaults.quit_key;
            settings.quit_count = defaults.quit_count;
            settings.quit_window_ms = defaults.quit_window_ms;
        }
        Tab::Quality => {
            settings.beam_size = defaults.beam_size;
            settings.temperature = defaults.temperature;
            settings.context_min_seconds = defaults.context_min_seconds;
            settings.parakeet_min_seconds = defaults.parakeet_min_seconds;
            settings.release_tail_ms = defaults.release_tail_ms;
            settings.vad_threshold = defaults.vad_threshold;
            settings.vad_min_silence_ms = defaults.vad_min_silence_ms;
            settings.vad_speech_pad_ms = defaults.vad_speech_pad_ms;
            settings.target_dbfs = defaults.target_dbfs;
            settings.min_input_dbfs = defaults.min_input_dbfs;
            settings.min_snr_db = defaults.min_snr_db;
            settings.audio_ducking = defaults.audio_ducking;
            settings.audio_ducking_level = defaults.audio_ducking_level;
            settings.initial_prompt = defaults.initial_prompt;
        }
        Tab::Dictionary => {
            settings.dictionary = defaults.dictionary;
            settings.dictionary_enabled = defaults.dictionary_enabled;
            settings.dictionary_max_terms = defaults.dictionary_max_terms;
            settings.dictionary_prompt_chars = defaults.dictionary_prompt_chars;
        }
        Tab::Output => {
            settings.ui_theme = defaults.ui_theme;
            settings.ui_language = defaults.ui_language;
            settings.ui_log_view = defaults.ui_log_view;
            settings.inject_mode = defaults.inject_mode;
            settings.format_commands = defaults.format_commands;
            settings.inject_json = defaults.inject_json;
            settings.metrics_jsonl = defaults.metrics_jsonl;
            settings.command_hook = defaults.command_hook;
            settings.command_hook_timeout_ms = defaults.command_hook_timeout_ms;
            settings.history_enabled = defaults.history_enabled;
            settings.history_jsonl = defaults.history_jsonl;
            settings.local_only = defaults.local_only;
            settings.debug = defaults.debug;
            settings.stt_debug = defaults.stt_debug;
            settings.ui_text_scale = defaults.ui_text_scale;
        }
        Tab::Post => {
            settings.post_processor = defaults.post_processor;
            settings.post_mode = defaults.post_mode;
            settings.post_model = defaults.post_model;
            settings.post_base_url = defaults.post_base_url;
            settings.post_timeout_ms = defaults.post_timeout_ms;
            settings.post_max_input_chars = defaults.post_max_input_chars;
            settings.post_max_output_chars = defaults.post_max_output_chars;
            settings.post_redact = defaults.post_redact;
            settings.post_redact_terms = defaults.post_redact_terms;
        }
        Tab::Profiles => {
            settings.profiles_json = defaults.profiles_json;
        }
    }
}

fn settings_grid(id: &'static str) -> egui::Grid {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing(egui::vec2(20.0, 10.0))
}

fn section_label(ui: &mut egui::Ui, label: &str, palette: UiPalette) {
    ui.label(
        egui::RichText::new(label)
            .size(12.0)
            .strong()
            .color(palette.text_muted),
    );
}

fn theme_toggle(ui: &mut egui::Ui, value: &mut String, palette: UiPalette, raw_language: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (raw, icon, label) in [
            (
                "dark",
                icons::ICON_DARK_MODE,
                ui_text(raw_language, UiTextKey::Dark),
            ),
            (
                "light",
                icons::ICON_LIGHT_MODE,
                ui_text(raw_language, UiTextKey::Light),
            ),
        ] {
            let selected = value == raw;
            let fill = if selected {
                palette.accent_dark
            } else {
                palette.surface_bg
            };
            let text = if selected {
                icon_text(icon, label).strong().color(palette.text)
            } else {
                icon_text(icon, label).color(palette.text_muted)
            };
            if ui
                .add_sized(
                    egui::vec2(92.0, 30.0),
                    egui::Button::new(text)
                        .fill(fill)
                        .stroke(egui::Stroke::new(0.8, palette.border_soft)),
                )
                .clicked()
            {
                *value = raw.to_owned();
            }
        }
    });
}

fn language_toggle(ui: &mut egui::Ui, value: &mut String, palette: UiPalette) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (raw, label) in [
            ("en", ui_text(value.as_str(), UiTextKey::English)),
            ("da", ui_text(value.as_str(), UiTextKey::Danish)),
        ] {
            let selected = value == raw;
            let fill = if selected {
                palette.accent_dark
            } else {
                palette.surface_bg
            };
            let text = if selected {
                egui::RichText::new(label).strong().color(palette.text)
            } else {
                egui::RichText::new(label).color(palette.text_muted)
            };
            if ui
                .add_sized(
                    egui::vec2(92.0, 30.0),
                    egui::Button::new(text)
                        .fill(fill)
                        .stroke(egui::Stroke::new(0.8, palette.border_soft)),
                )
                .clicked()
            {
                *value = raw.to_owned();
            }
        }
    });
}

fn status_card(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
) {
    status_card_sized(ui, label, icon, value, accent, palette, 134.0);
}

fn status_card_wide(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
) {
    status_card_sized(ui, label, icon, value, accent, palette, 218.0);
}

fn status_card_sized(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
    min_width: f32,
) {
    let value = value.as_ref();
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .rounding(egui::Rounding::same(PANEL_RADIUS as f32))
        .inner_margin(egui::Margin::symmetric(14.0, 9.0))
        .show(ui, |ui| {
            ui.set_min_width(min_width);
            ui.label(icon_text(icon, label).size(12.0).color(palette.text_muted));
            ui.label(egui::RichText::new(value).strong().color(accent))
                .on_hover_text(value);
        });
}

fn top_status_controls_width() -> f32 {
    186.0
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

fn runtime_state_color(state: RuntimeState, palette: UiPalette) -> egui::Color32 {
    match state {
        RuntimeState::Stopped => palette.text_muted,
        RuntimeState::Starting => palette.warn_text,
        RuntimeState::Running => palette.ok_text,
    }
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

pub(super) fn live_audio_level_summary(
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

pub(super) fn mic_label_char_budget(width: f32) -> usize {
    ((width / 7.0).floor() as usize).clamp(8, 34)
}

pub(super) fn audio_device_label(value: &str, max_chars: usize) -> String {
    let value = value.trim();
    if value.is_empty() {
        return "Input pending".to_owned();
    }
    compact_label(value, max_chars.clamp(8, 34))
}

pub(super) fn full_audio_device_label(value: &str) -> &str {
    let value = value.trim();
    if value.is_empty() {
        "Not reported yet"
    } else {
        value
    }
}

fn compact_label(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return value.to_owned();
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn empty_as_auto(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "Auto"
    } else {
        trimmed
    }
}

fn empty_as_disabled(value: &str) -> &str {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "none" {
        "Disabled"
    } else {
        trimmed
    }
}

fn status_label(ui: &mut egui::Ui, text: &str, palette: UiPalette) {
    let rich_text = if text.starts_with("[OK]") {
        egui::RichText::new(text).color(palette.ok_text)
    } else if text.starts_with("[ERROR]") {
        egui::RichText::new(text).color(palette.error_text)
    } else {
        egui::RichText::new(text)
    };
    ui.add(egui::Label::new(rich_text).wrap());
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
