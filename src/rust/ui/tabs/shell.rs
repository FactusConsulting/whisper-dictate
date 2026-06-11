use super::super::*;
use super::*;
use egui_material_icons::icons;

impl WhisperDictateApp {
    pub(in crate::ui) fn sidebar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
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
        // Recording indicator: a coloured dot + bold label. Visible even when
        // the window is minimised or only its top-left corner is showing.
        let (label_key, slot) =
            recording_indicator_style(self.pipeline_stage, self.display_runtime_state());
        let color = slot.resolve(palette);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 5.0, color);
            ui.label(
                egui::RichText::new(ui_text(&self.settings.ui_language, label_key))
                    .text_style(egui::TextStyle::Small)
                    .strong()
                    .color(color),
            );
        });
        ui.add_space(18.0);

        // Render the bottom block first inside a bottom_up layout so it always
        // claims its space from the sidebar's bottom edge. The tab ScrollArea
        // that follows fills only the space between the header and this block,
        // preventing overlap when the window is short.
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.label(
                egui::RichText::new(format!("v{}", self.app_version))
                    .text_style(egui::TextStyle::Small)
                    .color(palette.text_muted),
            );
            // Discreet "update available" badge, placed just ABOVE the version
            // label (bottom_up layout renders later items higher). Subtle accent
            // line only — no popup; the hover gives the upgrade command.
            if let Some(version) = self.update_available.clone() {
                ui.add(
                    egui::Label::new(
                        icon_text(
                            icons::ICON_ARROW_UPWARD,
                            format!(
                                "v{} {}",
                                version,
                                ui_text(&self.settings.ui_language, UiTextKey::UpdateAvailable),
                            ),
                        )
                        .text_style(egui::TextStyle::Small)
                        .strong()
                        .color(palette.accent_blue),
                    )
                    .selectable(false),
                )
                .on_hover_text(ui_text(
                    &self.settings.ui_language,
                    UiTextKey::UpdateAvailableHover,
                ));
            }
            ui.add_space(8.0);
            // Reload config / Doctor / Install-Repair moved to the System tab so
            // the sidebar stays a slim navigator. The bottom block now keeps only
            // the PTT chord, Save settings, and the version.
            // Save lives here, next to the saved/unsaved status, rather than
            // repeated on every settings page.
            let is_dirty = self.has_unsaved_settings();
            let save_text = if is_dirty {
                UiTextKey::SaveSettingsDirty
            } else {
                UiTextKey::SaveSettings
            };
            let mut save_button = egui::Button::new(
                icon_text(
                    icons::ICON_SAVE,
                    ui_text(&self.settings.ui_language, save_text),
                )
                .strong(),
            )
            .min_size(egui::vec2(ui.available_width(), 34.0));
            if is_dirty {
                save_button = save_button.fill(palette.accent_dark);
            }
            if ui
                .add_enabled(is_dirty, save_button)
                .on_hover_text("Save changed settings and any edited cloud API key.")
                .clicked()
            {
                self.save_settings();
            }
            ui.add_space(10.0);
            // The active hotkey chord, just above Save settings. Deliberately
            // compact: the chord alone (no "Push-to-talk:" prefix — everyone
            // knows what the key is for); the hover names the mode and where
            // to change it. bottom_up layout, so adding it AFTER the save
            // button places it visually above the button cluster.
            ui.add(
                egui::Label::new(
                    icon_text(
                        icons::ICON_KEYBOARD,
                        format_push_to_talk_keys(&self.settings.key),
                    )
                    .text_style(egui::TextStyle::Small)
                    .strong()
                    .color(palette.text),
                )
                .wrap()
                .selectable(false),
            )
            .on_hover_text(format!(
                "{} — {}",
                push_to_talk_badge_label(
                    &self.settings.key,
                    self.settings.toggle_mode,
                    &self.settings.ui_language,
                ),
                if self.settings.toggle_mode {
                    "Press to start, press again to stop. Change it in Speech → Hotkey."
                } else {
                    "Hold to dictate. Change it in Speech → Hotkey."
                }
            ));

            // The tab list occupies the space remaining above the bottom block.
            // Wrapping it in a ScrollArea means the tabs scroll instead of
            // being painted over the bottom block when the window is short.
            // The ScrollArea content INHERITS the surrounding bottom_up layout,
            // which rendered the tabs in reverse (System on top) — force
            // top_down so Dictation stays first, as in Tab::ALL order.
            egui::ScrollArea::vertical()
                .id_salt("sidebar_tab_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
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
                    });
                });
        });
    }

    /// Thin global status bar (bottom of every tab): a saved/unsaved dot + the
    /// latest message. Replaces the old sidebar badge and per-page Messages card.
    pub(in crate::ui) fn status_message_bar(&self, ui: &mut egui::Ui, palette: UiPalette) {
        // Center the row vertically within the thin bar. `horizontal_centered`
        // only centers items within their own row band, which leaves the content
        // hugging the top of the panel; allocating the full height with a
        // left-to-right Center layout (like the top status bar) centers it.
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.available_height()),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                // Even, compact spacing so the dot, label and message read as one row.
                ui.spacing_mut().item_spacing.x = 6.0;
                let is_dirty = self.has_unsaved_settings();
                let (label_key, color) = if is_dirty {
                    (UiTextKey::UnsavedChanges, palette.warn_text)
                } else {
                    (UiTextKey::SettingsSaved, palette.ok_text)
                };
                // A tight 8px dot whose circle fills its box, vertically centered
                // with the label by the surrounding centered layout.
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.label(
                    egui::RichText::new(ui_text(&self.settings.ui_language, label_key))
                        .color(color)
                        .strong(),
                );
                let message = self.status_bar_message();
                if !message.is_empty() {
                    ui.label(egui::RichText::new("·").color(palette.text_muted));
                    ui.add(
                        egui::Label::new(egui::RichText::new(&message).color(palette.text_muted))
                            .truncate(),
                    )
                    .on_hover_text(&message);
                }
            },
        );
    }

    /// The latest status text for the bottom bar: the settings status plus the
    /// active tab's API-key status, joined compactly (full text on hover).
    fn status_bar_message(&self) -> String {
        let mut parts = Vec::new();
        if !self.settings_status.trim().is_empty() {
            parts.push(self.settings_status.trim());
        }
        match self.selected_tab {
            Tab::Speech if !self.stt_api_key_status.trim().is_empty() => {
                parts.push(self.stt_api_key_status.trim());
            }
            Tab::Post if !self.post_api_key_status.trim().is_empty() => {
                parts.push(self.post_api_key_status.trim());
            }
            _ => {}
        }
        parts.join("   ·   ")
    }

    pub(in crate::ui) fn top_status_bar(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
        // Allocate the right-pinned controls FIRST so they always get their
        // reserved width. The status cards on the left then receive the
        // remaining space and are clipped naturally — no `.max()` floor that
        // would make the two regions overlap at narrow widths.
        // Reserve the inter-region gap too: ui.horizontal inserts item_spacing
        // between the left region and the controls, and that spacing scales
        // with the UI text scale — without it the controls could still be
        // squeezed at narrow widths on scaled-up UIs.
        let controls_width = top_status_controls_width() + ui.spacing().item_spacing.x;
        let total_width = ui.available_width();
        let left_width = top_status_left_width(total_width, controls_width);
        let spacing = ui.spacing().item_spacing.x;
        // Own the scale string in a local: the render closure below needs
        // `&mut self`, so borrowing `self.settings.ui_text_scale` across it
        // would conflict. Bind the clone to a named local (not a borrowed
        // temporary) and pass `&str` slices of it.
        let raw_scale = self.settings.ui_text_scale.clone();
        let raw_scale = raw_scale.as_str();
        // Build the ordered list of card widths (highest priority first). These
        // are the TRUE OUTER widths (inner content min-width + Frame margin +
        // stroke), NOT the inner `set_min_width` values — feeding the bare inner
        // widths here undercounts each card by ~30px and the cards overflow the
        // left budget and get sliced by the clip rect.
        //   0 – Status (always rendered; clip rect is the last resort)
        //   1 – Backend
        //   2 – Model/Compute (wide)
        //   3 – Post indicator pill
        //   4 – Background-task card (only when a task is active)
        let card_outer_widths: &[f32] = if self.background_task_label.is_some() {
            &[
                status_card_outer_width(raw_scale),
                status_card_outer_width(raw_scale),
                status_card_wide_outer_width(raw_scale),
                post_indicator_outer_width(raw_scale),
                status_card_outer_width(raw_scale),
            ]
        } else {
            &[
                status_card_outer_width(raw_scale),
                status_card_outer_width(raw_scale),
                status_card_wide_outer_width(raw_scale),
                post_indicator_outer_width(raw_scale),
            ]
        };
        let fit_count = top_status_cards_fit(left_width, card_outer_widths, spacing);
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(left_width, ui.available_height()),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    // egui does NOT clip child content to the allocated rect —
                    // without an explicit clip the cards/indicator paint right
                    // under the Start/Stop/compact controls at narrow widths.
                    // The clip rect is kept as a backstop for the Status card
                    // (which always renders) and for any rounding edge cases.
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    let display_state = self.display_runtime_state();
                    // Card 0: Status — always rendered (clip rect is backstop).
                    status_card(
                        ui,
                        ui_text(&self.settings.ui_language, UiTextKey::Status),
                        icons::ICON_RADIO_BUTTON_CHECKED,
                        runtime_state_label(display_state, &self.settings.ui_language),
                        runtime_state_color(display_state, palette),
                        palette,
                        raw_scale,
                    );
                    // Card 1: Backend.
                    if fit_count > 1 {
                        status_card(
                            ui,
                            ui_text(&self.settings.ui_language, UiTextKey::Backend),
                            icons::ICON_MODEL_TRAINING,
                            self.backend_summary(),
                            palette.accent_blue,
                            palette,
                            raw_scale,
                        );
                    }
                    // Card 2: Model/Compute (wide).
                    if fit_count > 2 {
                        let (detail_label, detail_icon, detail_value) = self.stt_detail_summary();
                        status_card_wide(
                            ui,
                            detail_label,
                            detail_icon,
                            detail_value,
                            palette.accent_blue,
                            palette,
                            raw_scale,
                        );
                    }
                    // Card 3: Post indicator pill.
                    if fit_count > 3 {
                        self.post_indicator(ui, palette);
                    }
                    // Card 4: Background-task card (only when active).
                    if fit_count > 4 {
                        if let Some(label) = self.background_task_label {
                            status_card(
                                ui,
                                ui_text(&self.settings.ui_language, UiTextKey::Task),
                                icons::ICON_PENDING_ACTIONS,
                                label,
                                palette.warn_text,
                                palette,
                                raw_scale,
                            );
                        }
                    }
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.global_controls(ui, palette);
            });
        });
    }

    /// Compact post-processing indicator that lives WITH the left status cards
    /// (so it shares their clipping budget and never overlaps the right-pinned
    /// runtime controls). Muted "Post off" when inactive, accent-coloured
    /// "Post on" when the worker would actually run a post pass; the hover names
    /// the configured mode + processor.
    fn post_indicator(&self, ui: &mut egui::Ui, palette: UiPalette) {
        let enabled =
            post_processing_enabled(&self.settings.post_processor, &self.settings.post_mode);
        let (icon, color) = if enabled {
            (icons::ICON_AUTO_FIX_HIGH, palette.accent_blue)
        } else {
            (icons::ICON_AUTO_FIX_HIGH, palette.text_muted)
        };
        let label = post_indicator_label(
            &self.settings.post_processor,
            &self.settings.post_mode,
            &self.settings.ui_language,
        );
        // Same flat readout treatment as the status cards (recessed tint, no
        // border, barely-rounded corner). Asymmetric top margin: same
        // STATUS_CARD_V_TOP_REDUCTION optical-centering as the two-line cards.
        egui::Frame::default()
            .fill(palette.readout_bg)
            .stroke(egui::Stroke::new(CARD_STROKE, palette.border_soft))
            .rounding(egui::Rounding::same(READOUT_RADIUS as f32))
            .inner_margin(egui::Margin {
                left: POST_PILL_H_MARGIN,
                right: POST_PILL_H_MARGIN,
                top: STATUS_CARD_V_MARGIN - STATUS_CARD_V_TOP_REDUCTION,
                bottom: STATUS_CARD_V_MARGIN,
            })
            .show(ui, |ui| {
                ui.label(icon_text(icon, label).strong().color(color));
            })
            .response
            .on_hover_text(post_indicator_hover(
                &self.settings.post_processor,
                &self.settings.post_mode,
            ));
    }

    pub(in crate::ui) fn global_controls(&mut self, ui: &mut egui::Ui, palette: UiPalette) {
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
        // Compact-mode toggle: a tiny always-on-top strip the user keeps visible
        // while dictating into another app. Session-only UI state — not persisted.
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(icons::ICON_PICTURE_IN_PICTURE_ALT).color(palette.text),
                )
                .min_size(egui::vec2(34.0, 34.0)),
            )
            .on_hover_text("Compact mode — small always-on-top strip")
            .clicked()
        {
            self.set_compact_mode(ui.ctx(), true);
        }
    }
}

fn status_card(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
    raw_scale: &str,
) {
    status_card_sized(
        ui,
        label,
        icon,
        value,
        accent,
        palette,
        status_card_min_width(raw_scale),
    );
}

fn status_card_wide(
    ui: &mut egui::Ui,
    label: &str,
    icon: &str,
    value: impl AsRef<str>,
    accent: egui::Color32,
    palette: UiPalette,
    raw_scale: &str,
) {
    status_card_sized(
        ui,
        label,
        icon,
        value,
        accent,
        palette,
        status_card_wide_min_width(raw_scale),
    );
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
    // Flat READOUT: recessed tint, no border, barely-rounded corner.
    // Asymmetric top margin (STATUS_CARD_V_TOP_REDUCTION) optically centres the
    // ink: egui adds ~2-4px of leading above the first pixel but not below the
    // last, so symmetric margins read top-heavy. See theme.rs for the derivation.
    egui::Frame::default()
        .fill(palette.readout_bg)
        .stroke(egui::Stroke::new(CARD_STROKE, palette.border_soft))
        .rounding(egui::Rounding::same(READOUT_RADIUS as f32))
        .inner_margin(egui::Margin {
            left: STATUS_CARD_H_MARGIN,
            right: STATUS_CARD_H_MARGIN,
            top: STATUS_CARD_V_MARGIN - STATUS_CARD_V_TOP_REDUCTION,
            bottom: STATUS_CARD_V_MARGIN,
        })
        .show(ui, |ui| {
            ui.set_min_width(min_width);
            ui.label(
                icon_text(icon, label)
                    .text_style(egui::TextStyle::Small)
                    .color(palette.text_muted),
            );
            ui.label(egui::RichText::new(value).strong().color(accent))
                .on_hover_text(value);
        });
}

pub(in crate::ui) fn runtime_state_color(state: RuntimeState, palette: UiPalette) -> egui::Color32 {
    match state {
        RuntimeState::Stopped => palette.text_muted,
        RuntimeState::Starting => palette.warn_text,
        RuntimeState::Running => palette.ok_text,
    }
}
