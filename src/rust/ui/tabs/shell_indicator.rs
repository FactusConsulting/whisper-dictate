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
///
/// Precedence:
/// 1. A `Stopped` worker can never legitimately be recording, so it always
///    resolves to Stopped/muted — even if `pipeline_stage` is a stale
///    `Some("recording")` left over from a worker that exited mid-dictation.
///    This is defense-in-depth: the lifecycle paths (stop/restart/exit/error)
///    already clear `pipeline_stage`, but this guarantees the indicator can
///    never stick red on a stopped worker.
/// 2. Otherwise an active `Some("recording")` stage takes priority over the
///    Running/Starting runtime states — this is the live transitional case
///    where the worker is mid-utterance.
/// 3. Otherwise fall through to the plain runtime state.
pub(in crate::ui) fn recording_indicator_style(
    pipeline_stage: Option<&str>,
    runtime: RuntimeState,
) -> (UiTextKey, RecordingIndicatorColor) {
    match runtime {
        RuntimeState::Stopped => (UiTextKey::Stopped, RecordingIndicatorColor::Muted),
        RuntimeState::Running | RuntimeState::Starting if pipeline_stage == Some("recording") => {
            (UiTextKey::Recording, RecordingIndicatorColor::Error)
        }
        RuntimeState::Running => (UiTextKey::Ready, RecordingIndicatorColor::Ok),
        RuntimeState::Starting => (UiTextKey::Starting, RecordingIndicatorColor::Warn),
    }
}
