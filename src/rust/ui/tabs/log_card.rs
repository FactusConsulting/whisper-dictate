//! Frame widgets for the runtime log view: the per-entry log "cards", the empty
//! state, the small status pill, and the session metric boxes.

use super::super::*;
use super::*;
use egui_material_icons::icons;

/// Icon glyph, accent colour and frame fill for a card kind.
fn runtime_log_card_style(
    kind: RuntimeLogCardKind,
    palette: UiPalette,
) -> (&'static str, egui::Color32, egui::Color32) {
    match kind {
        RuntimeLogCardKind::FinalText => (
            icons::ICON_CHECK_CIRCLE.codepoint,
            palette.ok_text,
            palette.surface_active_bg,
        ),
        RuntimeLogCardKind::Status => (
            icons::ICON_INFO.codepoint,
            palette.accent_blue,
            palette.surface_bg,
        ),
        RuntimeLogCardKind::Diagnostic => (
            icons::ICON_GRAPHIC_EQ.codepoint,
            palette.warn_text,
            palette.header_bg,
        ),
        // Graded health, worst signal wins. ICON_MONITOR_HEART reads as "health
        // monitor", visually distinct from FinalText's ICON_CHECK_CIRCLE.
        // perfect: strong/bright green.
        RuntimeLogCardKind::HealthPerfect => (
            icons::ICON_MONITOR_HEART.codepoint,
            palette.ok_text,
            palette.surface_bg,
        ),
        // good: a calmer teal/cyan — distinct from perfect's green but still
        // clearly "fine".
        RuntimeLogCardKind::HealthGood => (
            icons::ICON_MONITOR_HEART.codepoint,
            palette.accent_blue,
            palette.surface_bg,
        ),
        // fair: amber — ICON_WARNING flags that something was off.
        RuntimeLogCardKind::HealthFair => (
            icons::ICON_WARNING.codepoint,
            palette.warn_text,
            palette.header_bg,
        ),
        // poor: red — ICON_DANGEROUS makes an unusable utterance unmissable.
        RuntimeLogCardKind::HealthPoor => (
            icons::ICON_DANGEROUS.codepoint,
            palette.error_text,
            palette.header_bg,
        ),
    }
}

/// Title line plus the optional detail/badge row (the card's right column).
fn runtime_log_card_text(
    ui: &mut egui::Ui,
    card: &RuntimeLogCard,
    accent: egui::Color32,
    palette: UiPalette,
) {
    let title_size = match card.kind {
        RuntimeLogCardKind::FinalText => 17.0,
        _ => 14.0,
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(&card.title)
                .size(title_size)
                .strong()
                .color(palette.text),
        )
        .wrap(),
    );
    // Detail is one group per line ("\n"-separated) so the card reads as a
    // scannable summary; render each as its own muted line instead of a single
    // crammed run-on row.
    for detail_line in card.detail.split('\n') {
        if detail_line.is_empty() {
            continue;
        }
        let response = ui.add(
            egui::Label::new(
                egui::RichText::new(detail_line)
                    .size(12.0)
                    .color(palette.text_muted),
            )
            .wrap(),
        );
        if let Some(hover) = diagnostic_detail_hover(detail_line) {
            let _ = response.on_hover_text(hover);
        }
    }
    if !card.badge.is_empty() {
        ui.add_space(2.0);
        let pill = status_pill(ui, &card.badge, accent, palette);
        // Match against the catalogue strings (both languages) instead of
        // duplicating the translations here — a tweak in text.rs must not
        // silently detach this hover.
        if card.badge == ui_text("en", UiTextKey::Dictation)
            || card.badge == ui_text("da", UiTextKey::Dictation)
        {
            let _ = pill.on_hover_text(
                "One complete dictation: from key down to key up, with its measurements.",
            );
        }
    }
}

/// Return a plain-language hover for a structured-utterance detail line, or
/// `None` when the line does not match any known group.
fn diagnostic_detail_hover(line: &str) -> Option<&'static str> {
    if line.starts_with("recording=") || line.starts_with("raw=") {
        return Some(
            "Audio captured for this dictation: \
             recording = seconds of audio, \
             raw/peak/noise/snr = input levels and signal-to-noise ratio, \
             gain = microphone amplification applied.",
        );
    }
    if line.starts_with("compute=") || line.starts_with("rtf=") {
        return Some(
            "Transcription timing: \
             compute = how long the model took to process the audio, \
             rtf = compute / duration (below 1.0 means faster than real time).",
        );
    }
    if line.starts_with("backend=") {
        return Some(
            "The speech-to-text engine and model used for this dictation, \
             and the compute device (cpu / cuda / api).",
        );
    }
    if line.starts_with("dictionary") {
        return Some(
            "Dictionary hits: \
             terms = words matched by the custom vocabulary list, \
             replacements = substitutions applied.",
        );
    }
    if line.starts_with("post=") || line.starts_with("provider=") {
        return Some(
            "Post-processing: \
             post = mode (clean / fix / raw), \
             provider = the LLM service used, \
             post_model = specific model, \
             changed = whether the text was modified, \
             fallback = whether the primary provider was unavailable.",
        );
    }
    if line.starts_with("inject=") {
        return Some(
            "How the text was inserted: \
             inject = strategy (type = simulated keystrokes, paste = clipboard + Ctrl+V), \
             target = the window that received the text.",
        );
    }
    None
}

pub(in crate::ui) fn runtime_log_card(
    ui: &mut egui::Ui,
    card: &RuntimeLogCard,
    palette: UiPalette,
) {
    let (icon, accent, fill) = runtime_log_card_style(card.kind, palette);
    egui::Frame::default()
        .fill(fill)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
        .inner_margin(egui::Margin::symmetric(12, 12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                egui::Frame::default()
                    .fill(accent)
                    .corner_radius(egui::CornerRadius::same(PILL_RADIUS))
                    .show(ui, |ui| {
                        ui.set_min_size(egui::vec2(4.0, 46.0));
                    });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(icon).size(20.0).color(accent));
                ui.vertical(|ui| runtime_log_card_text(ui, card, accent, palette));
            });
        });
}

pub(in crate::ui) fn empty_log_state(
    ui: &mut egui::Ui,
    state: RuntimeState,
    palette: UiPalette,
    raw_language: &str,
) {
    egui::Frame::default()
        .fill(palette.surface_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .corner_radius(egui::CornerRadius::same(PANEL_RADIUS))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                icon_text(
                    icons::ICON_MIC.codepoint,
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

fn status_pill(
    ui: &mut egui::Ui,
    label: &str,
    accent: egui::Color32,
    palette: UiPalette,
) -> egui::Response {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, accent))
        .corner_radius(egui::CornerRadius::same(PILL_RADIUS))
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(label).size(11.0).strong().color(accent));
        })
        .response
}

pub(in crate::ui) fn metric_box(
    ui: &mut egui::Ui,
    label: &str,
    value: impl AsRef<str>,
    palette: UiPalette,
) -> egui::Response {
    egui::Frame::default()
        .fill(palette.header_bg)
        .stroke(egui::Stroke::new(0.8, palette.border_soft))
        .corner_radius(egui::CornerRadius::same(CONTROL_RADIUS))
        .inner_margin(egui::Margin::symmetric(11, 8))
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
        })
        .response
}

/// Scroll the surrounding ScrollArea while the user drag-selects text past its
/// vertical edges. egui never scrolls on its own during a drag selection, so a
/// selection otherwise stops dead at the viewport edge. Only active while the
/// primary button is down AND the drag STARTED inside this area.
pub(in crate::ui) fn drag_autoscroll(ui: &mut egui::Ui) {
    let clip = ui.clip_rect();
    let (down, origin, pos) = ui.ctx().input(|i| {
        (
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.interact_pos(),
        )
    });
    if !down {
        return;
    }
    let (Some(origin), Some(pos)) = (origin, pos) else {
        return;
    };
    if !clip.contains(origin) {
        return;
    }
    let delta = drag_overshoot_delta(clip.top(), clip.bottom(), pos.y);
    if delta != 0.0 {
        ui.scroll_with_delta(egui::vec2(0.0, delta));
        ui.ctx().request_repaint();
    }
}

/// Per-frame scroll delta for a drag past the [top, bottom] viewport edge.
/// Positive scrolls toward earlier content (drag above the top), negative
/// toward later content (drag below the bottom); speed grows with overshoot,
/// capped so wild drags stay controllable. Pure for unit tests.
pub(in crate::ui) fn drag_overshoot_delta(top: f32, bottom: f32, pointer_y: f32) -> f32 {
    const SPEED: f32 = 0.5;
    const MAX_STEP: f32 = 30.0;
    if pointer_y < top {
        ((top - pointer_y) * SPEED).min(MAX_STEP)
    } else if pointer_y > bottom {
        -(((pointer_y - bottom) * SPEED).min(MAX_STEP))
    } else {
        0.0
    }
}
