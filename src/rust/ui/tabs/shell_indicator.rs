//! Pure helpers for the sidebar recording indicator:
//! the colour-slot enum and the state→label/slot mapping function.
//! Separated from `shell.rs` so the file stays under 500 lines.

use super::super::*;

/// Semantic colour slot for the sidebar recording indicator.
/// Separating the slot from a resolved [`egui::Color32`] keeps
/// [`recording_indicator_style`] unit-testable without a palette instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum RecordingIndicatorColor {
    Error, // active capture  — palette.error_text (red)
    Ok,    // worker ready    — palette.ok_text    (green)
    Warn,  // starting up     — palette.warn_text  (amber)
    Muted, // stopped         — palette.text_muted (grey)
}

impl RecordingIndicatorColor {
    pub(in crate::ui) fn resolve(self, p: UiPalette) -> egui::Color32 {
        match self {
            Self::Error => p.error_text,
            Self::Ok => p.ok_text,
            Self::Warn => p.warn_text,
            Self::Muted => p.text_muted,
        }
    }
}

/// Pure helper: recording indicator label + colour slot for the sidebar header.
/// Recording always takes priority over the worker runtime state.
pub(in crate::ui) fn recording_indicator_style(
    pipeline_stage: Option<&str>,
    runtime: RuntimeState,
) -> (UiTextKey, RecordingIndicatorColor) {
    if pipeline_stage == Some("recording") {
        return (UiTextKey::Recording, RecordingIndicatorColor::Error);
    }
    match runtime {
        RuntimeState::Running => (UiTextKey::Ready, RecordingIndicatorColor::Ok),
        RuntimeState::Starting => (UiTextKey::Starting, RecordingIndicatorColor::Warn),
        RuntimeState::Stopped => (UiTextKey::Stopped, RecordingIndicatorColor::Muted),
    }
}
