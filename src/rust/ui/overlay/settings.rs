//! Mapping between worker `status`/`audio` events and the overlay's visibility
//! decision, plus the small typed wrappers around the persisted
//! `overlay_enabled`/`overlay_show_on_idle` settings.
//!
//! Kept in its own module so the visibility rule (the single source of truth
//! for "should the overlay be on screen right now?") can be unit-tested in
//! isolation, without spinning up egui.

use crate::runtime::RuntimeState;

/// The dictation phase the overlay reacts to, derived from the worker's
/// status events. Maps onto a separate, explicit enum so the visibility
/// decision in [`should_show_overlay`] doesn't depend on string compares
/// scattered across the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum OverlayPhase {
    /// No worker / idle. The "show on idle" toggle decides visibility.
    Idle,
    /// Worker process spawned but model still loading — the user has signalled
    /// intent (Start), so the overlay should appear with a "starting…" hint.
    Opening,
    /// PTT held / mic open / capture active.
    Recording,
    /// Audio uploaded, awaiting transcription.
    Transcribing,
}

impl OverlayPhase {
    /// Map the worker's raw status state + the live audio-capture flag onto
    /// the overlay phase. Mirrors `pipeline_stage_for_worker_state` plus the
    /// `opening` and `ready` short-circuits the overlay needs.
    pub(in crate::ui) fn from_worker_state(
        runtime: RuntimeState,
        worker_status: &str,
        audio_opening: bool,
        audio_active: bool,
    ) -> Self {
        // A stopped worker is always Idle, regardless of stale status strings.
        if matches!(runtime, RuntimeState::Stopped) {
            return Self::Idle;
        }
        if audio_opening || worker_status == "opening" {
            return Self::Opening;
        }
        // Mid-recording (mic open) — the live audio flag takes precedence over
        // the last status string, since "preview" / "audio" events arrive
        // BETWEEN status updates and would otherwise leave the overlay flashing.
        if audio_active || matches!(worker_status, "recording" | "listening" | "preview") {
            return Self::Recording;
        }
        match worker_status {
            "transcribing" | "post-processing" => Self::Transcribing,
            _ => Self::Idle,
        }
    }

    /// Short human-readable label for the overlay's header text.
    pub(in crate::ui) fn label(self) -> &'static str {
        match self {
            Self::Idle => "Idle",
            Self::Opening => "Starting\u{2026}",
            Self::Recording => "Recording",
            Self::Transcribing => "Transcribing\u{2026}",
        }
    }
}

/// The overlay's three persisted flags, packaged together so the render path
/// can be unit-tested without constructing the full `AppSettings`.
#[derive(Debug, Clone, Copy)]
pub(in crate::ui) struct OverlayConfig {
    pub enabled: bool,
    pub show_on_idle: bool,
}

/// Decide whether the overlay window should be on-screen right now, given the
/// user's settings and the current phase. Centralised so the same rule drives
/// the viewport spawn AND tests; the render path must never branch on the
/// `OverlayPhase`/`enabled` pair directly.
pub(in crate::ui) fn should_show_overlay(config: OverlayConfig, phase: OverlayPhase) -> bool {
    if !config.enabled {
        return false;
    }
    match phase {
        OverlayPhase::Idle => config.show_on_idle,
        OverlayPhase::Opening | OverlayPhase::Recording | OverlayPhase::Transcribing => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_event_recording_maps_to_recording_phase() {
        let phase =
            OverlayPhase::from_worker_state(RuntimeState::Running, "recording", false, true);
        assert_eq!(phase, OverlayPhase::Recording);
    }

    #[test]
    fn worker_event_opening_maps_to_opening_phase_even_if_status_is_blank() {
        let phase = OverlayPhase::from_worker_state(RuntimeState::Running, "", true, false);
        assert_eq!(phase, OverlayPhase::Opening);
        let phase = OverlayPhase::from_worker_state(RuntimeState::Running, "opening", false, false);
        assert_eq!(phase, OverlayPhase::Opening);
    }

    #[test]
    fn worker_event_preview_keeps_overlay_in_recording() {
        // The "preview" status carries the growing partial text and arrives
        // between recording ticks — the overlay must not flicker through Idle.
        let phase = OverlayPhase::from_worker_state(RuntimeState::Running, "preview", false, true);
        assert_eq!(phase, OverlayPhase::Recording);
    }

    #[test]
    fn worker_event_transcribing_and_post_processing_share_a_phase() {
        let phase =
            OverlayPhase::from_worker_state(RuntimeState::Running, "transcribing", false, false);
        assert_eq!(phase, OverlayPhase::Transcribing);
        let phase =
            OverlayPhase::from_worker_state(RuntimeState::Running, "post-processing", false, false);
        assert_eq!(phase, OverlayPhase::Transcribing);
    }

    #[test]
    fn stopped_worker_is_always_idle() {
        // Even with a stale "recording" status hanging around, a Stopped
        // runtime collapses straight to Idle so the overlay doesn't linger
        // after the user hits Stop.
        let phase = OverlayPhase::from_worker_state(RuntimeState::Stopped, "recording", true, true);
        assert_eq!(phase, OverlayPhase::Idle);
    }

    #[test]
    fn idle_phase_visibility_follows_show_on_idle_toggle() {
        let config = OverlayConfig {
            enabled: true,
            show_on_idle: false,
        };
        assert!(!should_show_overlay(config, OverlayPhase::Idle));
        let config = OverlayConfig {
            enabled: true,
            show_on_idle: true,
        };
        assert!(should_show_overlay(config, OverlayPhase::Idle));
    }

    #[test]
    fn disabled_overlay_never_shows_regardless_of_phase() {
        let config = OverlayConfig {
            enabled: false,
            show_on_idle: true,
        };
        for phase in [
            OverlayPhase::Idle,
            OverlayPhase::Opening,
            OverlayPhase::Recording,
            OverlayPhase::Transcribing,
        ] {
            assert!(!should_show_overlay(config, phase));
        }
    }

    #[test]
    fn active_phases_always_show_when_overlay_enabled() {
        let config = OverlayConfig {
            enabled: true,
            show_on_idle: false,
        };
        for phase in [
            OverlayPhase::Opening,
            OverlayPhase::Recording,
            OverlayPhase::Transcribing,
        ] {
            assert!(should_show_overlay(config, phase));
        }
    }

    #[test]
    fn phase_label_strings_are_localisable_constants() {
        // Spot check that each variant returns a non-empty label; widget tests
        // rely on these to size the header row.
        assert!(!OverlayPhase::Idle.label().is_empty());
        assert!(!OverlayPhase::Opening.label().is_empty());
        assert!(!OverlayPhase::Recording.label().is_empty());
        assert!(!OverlayPhase::Transcribing.label().is_empty());
    }
}
