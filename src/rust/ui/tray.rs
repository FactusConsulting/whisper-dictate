//! System-tray (notification-area) icon that mirrors the dictation state so the
//! user can SEE — at a glance, without the main window — when the microphone is
//! actually live. The core feature: the tray dot turns RED the moment capture
//! starts (worker `status=recording`), GREEN while the worker is idle/ready,
//! GREY when nothing is running, and AMBER while transcribing/processing.
//!
//! Layering, by design:
//! - The pure logic — the [`TrayState`] enum, the worker-status → state mapping
//!   ([`tray_state_for`] / [`tray_state_from_app`]), the programmatic icon
//!   pixels ([`tray_icon_rgba`]), and the tooltip-key mapping — is **cfg-free**
//!   so its unit tests run on every platform (incl. the Linux dev container/CI).
//! - The actual OS tray lives behind `#[cfg(windows)]` (see [`TrayManager`]).
//!   Windows is the primary platform and the user's request is Windows-specific;
//!   gating to Windows also keeps tray-icon's gtk/libxdo system deps out of the
//!   Linux build entirely (the crate is a `cfg(windows)` target dependency).
//!   Every other platform gets a zero-cost no-op stub, so the call sites in
//!   `app.rs` stay platform-agnostic and the dictation flow is never affected.

use super::{ui_text, UiTextKey};
use crate::runtime::RuntimeState;

/// What the tray icon should convey. Ordered roughly idle → active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum TrayState {
    /// No worker running — the app is idle. Grey dot.
    NotRunning,
    /// Worker is up and the model is loaded, but the microphone is NOT
    /// capturing. Green dot — "ready, safe to press push-to-talk".
    Ready,
    /// The microphone is actively capturing audio (worker `status=recording`).
    /// Red dot — "I'm listening, you can talk now".
    Recording,
    /// The worker is busy on the rest of the pipeline (transcribing /
    /// post-processing) or still starting up. Amber dot.
    Processing,
}

// `rgb` / `tooltip_key` feed the Windows tray icon + tooltip and the cfg-free
// unit tests. On a non-Windows non-test build (e.g. the Linux dev container/CI)
// the tray is a no-op stub, so they read as dead there — allow it rather than
// cfg-gate the pure logic, which must stay compiled+tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
impl TrayState {
    /// The opaque RGB fill for this state's mic dot. Kept here (not in the
    /// theme palette) so it is cfg-free and unit-testable without an egui
    /// context, and so the tray reads identically in light/dark themes.
    ///
    /// Colours echo the in-app recording indicator: red = recording,
    /// green = ready, amber = busy, grey = stopped.
    pub(in crate::ui) const fn rgb(self) -> [u8; 3] {
        match self {
            // A muted slate grey — clearly "off" against both light and dark trays.
            TrayState::NotRunning => [128, 134, 142],
            // Vivid green — matches the "ready" indicator.
            TrayState::Ready => [46, 184, 92],
            // Strong red — the unmissable "mic is LIVE" signal.
            TrayState::Recording => [224, 49, 49],
            // Amber — transitional / busy.
            TrayState::Processing => [240, 173, 38],
        }
    }

    /// The localized tooltip key for this state. The tooltip is the hover text
    /// on the tray icon, e.g. "whisper-dictate — recording".
    pub(in crate::ui) const fn tooltip_key(self) -> UiTextKey {
        match self {
            TrayState::NotRunning => UiTextKey::TrayTipNotRunning,
            TrayState::Ready => UiTextKey::TrayTipReady,
            TrayState::Recording => UiTextKey::TrayTipRecording,
            TrayState::Processing => UiTextKey::TrayTipProcessing,
        }
    }
}

/// Pure mapping from a raw worker `status` `state` string + whether the worker
/// process is running to a [`TrayState`].
///
/// This is the literal user scenario: while the worker is up, the dot is GREEN
/// (`ready`/idle) and only flips RED once `state == "recording"` — i.e. once
/// capture is actually live — so the user knows the held push-to-talk key has
/// taken effect and they can start talking. `transcribing`/`post-processing`
/// go AMBER; an unknown state while running falls back to GREEN (ready).
///
/// The app itself derives the tray state from its tracked fields via
/// [`tray_state_from_app`]; this string-based variant matches the spec and is
/// exercised by the unit tests (hence the `dead_code` allowance off Windows).
#[cfg_attr(not(windows), allow(dead_code))]
pub(in crate::ui) fn tray_state_for(status_state: &str, worker_running: bool) -> TrayState {
    if !worker_running {
        return TrayState::NotRunning;
    }
    match status_state {
        "recording" => TrayState::Recording,
        "transcribing" | "post-processing" | "loading_model" | "opening" => TrayState::Processing,
        // ready / no_text / preview / capture_lost / listening / unknown → idle-ready.
        _ => TrayState::Ready,
    }
}

/// Derive the tray state from the fields the app already tracks (the same
/// inputs the in-app sidebar recording indicator uses), so the tray and the
/// indicator never disagree.
///
/// Precedence mirrors [`super::tabs::recording_indicator_style`]:
/// 1. `Stopped` → `NotRunning` (can never legitimately be recording, even with
///    a stale `pipeline_stage`).
/// 2. An active `recording` pipeline stage → `Recording` (the live capture
///    moment), regardless of Running/Starting.
/// 3. Other active stages (`transcribing`/`post-processing`) → `Processing`.
/// 4. `Running` → `Ready`; `Starting` → `Processing` (busy loading).
pub(in crate::ui) fn tray_state_from_app(
    runtime: RuntimeState,
    pipeline_stage: Option<&str>,
) -> TrayState {
    match runtime {
        RuntimeState::Stopped => TrayState::NotRunning,
        RuntimeState::Running | RuntimeState::Starting if pipeline_stage == Some("recording") => {
            TrayState::Recording
        }
        RuntimeState::Running | RuntimeState::Starting
            if matches!(
                pipeline_stage,
                Some("transcribing") | Some("post-processing")
            ) =>
        {
            TrayState::Processing
        }
        RuntimeState::Running => TrayState::Ready,
        RuntimeState::Starting => TrayState::Processing,
    }
}

/// The localized tooltip string for a tray state (e.g. "whisper-dictate — recording").
#[cfg_attr(not(windows), allow(dead_code))]
pub(in crate::ui) fn tray_tooltip(state: TrayState, raw_language: &str) -> &'static str {
    ui_text(raw_language, state.tooltip_key())
}

/// Build a square RGBA (8-bit, premultiplied-by-nothing straight alpha) image
/// for the tray icon: a centred filled circle (the "mic dot") in this state's
/// colour on a transparent background, with a soft 1px anti-aliased edge.
///
/// Pure and cfg-free so it is unit-tested everywhere; the Windows tray builds a
/// `tray_icon::Icon` from this buffer via `Icon::from_rgba`.
///
/// The returned buffer is exactly `size * size * 4` bytes in RGBA order.
#[cfg_attr(not(windows), allow(dead_code))]
pub(in crate::ui) fn tray_icon_rgba(state: TrayState, size: u32) -> Vec<u8> {
    let [r, g, b] = state.rgb();
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    if size == 0 {
        return rgba;
    }
    let center = (size as f32 - 1.0) / 2.0;
    // Leave a small margin so the dot doesn't touch the icon edge; ~88% of the
    // half-size keeps a crisp circle at tiny tray sizes (16/24/32 px).
    let radius = center * 0.88;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // Coverage: 1.0 well inside, 0.0 well outside, linear over a 1px band
            // for a soft anti-aliased edge.
            let coverage = (radius + 0.5 - dist).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let idx = ((y * size + x) * 4) as usize;
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = (coverage * 255.0) as u8;
        }
    }
    rgba
}

// ---------------------------------------------------------------------------
// Platform tray manager
// ---------------------------------------------------------------------------

/// Outcome of polling the tray for user interaction, surfaced to the app so it
/// can react (currently: bring the window to front on a left-click). Kept
/// cfg-free so `app.rs` can match on it on every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::ui) struct TrayInteraction {
    /// The user left-clicked the tray icon — focus/raise the main window.
    pub(in crate::ui) activate_window: bool,
}

#[cfg(windows)]
pub(in crate::ui) use self::imp::TrayManager;
#[cfg(not(windows))]
pub(in crate::ui) use self::stub::TrayManager;

/// Tray icon edge length in pixels. 32 px is a safe, crisp source size that
/// Windows downscales for the notification area.
#[cfg(windows)]
const TRAY_ICON_SIZE: u32 = 32;

#[cfg(windows)]
mod imp {
    use super::{tray_icon_rgba, tray_tooltip, TrayInteraction, TrayState, TRAY_ICON_SIZE};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// Owns the live Windows tray icon and the last state we pushed to it, so we
    /// only call `set_icon`/`set_tooltip` when the mapped state actually CHANGES
    /// (not every frame). Created lazily on the first `update()` so the win32
    /// event loop is already running.
    pub(in crate::ui) struct TrayManager {
        tray: Option<TrayIcon>,
        current: Option<TrayState>,
        /// Set once if tray creation failed, so we log the failure a single time
        /// and then quietly run without a tray (never retry-spam, never crash).
        init_failed: bool,
    }

    impl TrayManager {
        pub(in crate::ui) fn new() -> Self {
            Self {
                tray: None,
                current: None,
                init_failed: false,
            }
        }

        fn build_icon(state: TrayState) -> Result<Icon, String> {
            let rgba = tray_icon_rgba(state, TRAY_ICON_SIZE);
            Icon::from_rgba(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE).map_err(|e| e.to_string())
        }

        /// Ensure the tray exists and reflects `state` + a localized tooltip.
        /// Returns `Ok(())` on success or after a benign no-op; returns `Err`
        /// with a one-line reason the FIRST time creation fails (the caller logs
        /// it once). Subsequent calls after a failure are silent no-ops.
        pub(in crate::ui) fn sync(
            &mut self,
            state: TrayState,
            raw_language: &str,
        ) -> Result<(), String> {
            if self.init_failed {
                return Ok(());
            }
            let tooltip = tray_tooltip(state, raw_language);

            // Lazy creation on first call (event loop is running by now).
            if self.tray.is_none() {
                let icon = Self::build_icon(state)?;
                let tray = TrayIconBuilder::new()
                    .with_tooltip(tooltip)
                    .with_icon(icon)
                    .build()
                    .map_err(|e| e.to_string())?;
                self.tray = Some(tray);
                self.current = Some(state);
                return Ok(());
            }

            // Only touch the OS icon when the mapped state actually changed.
            if self.current == Some(state) {
                return Ok(());
            }
            if let Some(tray) = self.tray.as_ref() {
                if let Ok(icon) = Self::build_icon(state) {
                    let _ = tray.set_icon(Some(icon));
                }
                let _ = tray.set_tooltip(Some(tooltip));
            }
            self.current = Some(state);
            Ok(())
        }

        /// Mark the tray as permanently disabled after a failed init so we never
        /// retry (and never spam the log).
        pub(in crate::ui) fn disable(&mut self) {
            self.init_failed = true;
            self.tray = None;
        }

        /// Drain any pending tray-icon events. A left mouse button "up" on the
        /// icon asks the app to bring its window to front. tray-icon delivers
        /// these through a global channel that eframe's win32 message pump
        /// drives, so draining it here (once per frame) is sufficient.
        pub(in crate::ui) fn poll_interaction(&self) -> TrayInteraction {
            let mut interaction = TrayInteraction::default();
            if self.tray.is_none() || self.init_failed {
                return interaction;
            }
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } = event
                {
                    interaction.activate_window = true;
                }
            }
            interaction
        }
    }
}

#[cfg(not(windows))]
mod stub {
    use super::{TrayInteraction, TrayState};

    /// No-op tray for non-Windows targets: keeps the call sites in `app.rs`
    /// identical without dragging gtk/libxdo into the Linux build. Every method
    /// is a cheap no-op, so the dictation flow is wholly unaffected.
    pub(in crate::ui) struct TrayManager;

    impl TrayManager {
        pub(in crate::ui) fn new() -> Self {
            Self
        }

        pub(in crate::ui) fn sync(
            &mut self,
            _state: TrayState,
            _raw_language: &str,
        ) -> Result<(), String> {
            Ok(())
        }

        pub(in crate::ui) fn disable(&mut self) {}

        pub(in crate::ui) fn poll_interaction(&self) -> TrayInteraction {
            TrayInteraction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_running_when_worker_down_regardless_of_state_string() {
        // Even a "recording" string can't show red when the worker isn't running.
        for state in ["recording", "ready", "transcribing", "", "whatever"] {
            assert_eq!(
                tray_state_for(state, false),
                TrayState::NotRunning,
                "state {state:?} with worker down must be NotRunning"
            );
        }
    }

    #[test]
    fn recording_state_maps_to_recording() {
        assert_eq!(tray_state_for("recording", true), TrayState::Recording);
    }

    #[test]
    fn busy_states_map_to_processing() {
        for state in [
            "transcribing",
            "post-processing",
            "loading_model",
            "opening",
        ] {
            assert_eq!(
                tray_state_for(state, true),
                TrayState::Processing,
                "state {state:?} should be Processing"
            );
        }
    }

    #[test]
    fn ready_and_unknown_states_map_to_ready_while_running() {
        for state in [
            "ready",
            "no_text",
            "preview",
            "capture_lost",
            "listening",
            "weird",
        ] {
            assert_eq!(
                tray_state_for(state, true),
                TrayState::Ready,
                "state {state:?} while running should be Ready (green)"
            );
        }
    }

    #[test]
    fn opening_then_recording_flips_green_to_red_only_on_recording() {
        // The exact user scenario: holding push-to-talk opens the device first
        // (mic NOT live yet → stay amber/processing, definitely NOT recording),
        // then capture goes live (`recording`) → RED, telling the user to talk.
        let opening = tray_state_for("opening", true);
        assert_ne!(
            opening,
            TrayState::Recording,
            "while merely opening, the dot must not be red yet"
        );
        let recording = tray_state_for("recording", true);
        assert_eq!(recording, TrayState::Recording);
        assert_ne!(
            recording, opening,
            "recording must visibly differ from opening"
        );
        // And distinct colours back the distinct states.
        assert_ne!(TrayState::Ready.rgb(), TrayState::Recording.rgb());
    }

    #[test]
    fn from_app_matches_indicator_precedence() {
        // Stopped → grey, even with a stale recording stage.
        assert_eq!(
            tray_state_from_app(RuntimeState::Stopped, Some("recording")),
            TrayState::NotRunning
        );
        // Active recording stage wins over Running/Starting → red.
        assert_eq!(
            tray_state_from_app(RuntimeState::Running, Some("recording")),
            TrayState::Recording
        );
        assert_eq!(
            tray_state_from_app(RuntimeState::Starting, Some("recording")),
            TrayState::Recording
        );
        // Other pipeline stages → amber.
        assert_eq!(
            tray_state_from_app(RuntimeState::Running, Some("transcribing")),
            TrayState::Processing
        );
        assert_eq!(
            tray_state_from_app(RuntimeState::Running, Some("post-processing")),
            TrayState::Processing
        );
        // Plain running (idle) → green; plain starting → amber.
        assert_eq!(
            tray_state_from_app(RuntimeState::Running, None),
            TrayState::Ready
        );
        assert_eq!(
            tray_state_from_app(RuntimeState::Starting, None),
            TrayState::Processing
        );
    }

    #[test]
    fn each_state_has_a_distinct_colour() {
        let colours = [
            TrayState::NotRunning.rgb(),
            TrayState::Ready.rgb(),
            TrayState::Recording.rgb(),
            TrayState::Processing.rgb(),
        ];
        for i in 0..colours.len() {
            for j in (i + 1)..colours.len() {
                assert_ne!(
                    colours[i], colours[j],
                    "tray colours {i} and {j} must differ"
                );
            }
        }
        // Sanity: recording is red-dominant, ready is green-dominant.
        let red = TrayState::Recording.rgb();
        assert!(
            red[0] > red[1] && red[0] > red[2],
            "recording must be red-dominant"
        );
        let green = TrayState::Ready.rgb();
        assert!(
            green[1] > green[0] && green[1] > green[2],
            "ready must be green-dominant"
        );
    }

    /// The dominant (modal) opaque pixel colour of a rendered icon, used to
    /// assert the dot is painted in the expected state colour.
    fn dominant_opaque_rgb(rgba: &[u8]) -> [u8; 3] {
        use std::collections::HashMap;
        let mut counts: HashMap<[u8; 3], usize> = HashMap::new();
        for px in rgba.chunks_exact(4) {
            if px[3] == 255 {
                *counts.entry([px[0], px[1], px[2]]).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|&(_, n)| n)
            .map(|(c, _)| c)
            .unwrap_or([0, 0, 0])
    }

    #[test]
    fn icon_rgba_has_correct_buffer_size() {
        for size in [16u32, 24, 32, 64] {
            let buf = tray_icon_rgba(TrayState::Recording, size);
            assert_eq!(buf.len(), (size * size * 4) as usize, "size {size}");
        }
    }

    #[test]
    fn icon_rgba_dominant_colour_matches_state() {
        for state in [
            TrayState::NotRunning,
            TrayState::Ready,
            TrayState::Recording,
            TrayState::Processing,
        ] {
            let buf = tray_icon_rgba(state, 32);
            assert_eq!(
                dominant_opaque_rgb(&buf),
                state.rgb(),
                "the dot's dominant colour must equal the state colour for {state:?}"
            );
        }
    }

    #[test]
    fn icon_rgba_has_transparent_corners_and_opaque_centre() {
        let size = 32u32;
        let buf = tray_icon_rgba(TrayState::Recording, size);
        // Top-left corner pixel is outside the circle → fully transparent.
        assert_eq!(buf[3], 0, "corner must be transparent");
        // Centre pixel is inside the circle → fully opaque and red.
        let center = (size / 2 * size + size / 2) as usize * 4;
        assert_eq!(buf[center + 3], 255, "centre must be opaque");
        assert_eq!(
            [buf[center], buf[center + 1], buf[center + 2]],
            TrayState::Recording.rgb()
        );
    }

    #[test]
    fn icon_rgba_zero_size_is_empty_without_panicking() {
        assert!(tray_icon_rgba(TrayState::Ready, 0).is_empty());
    }

    #[test]
    fn tooltips_localized_en_and_da_present_and_distinct() {
        // English tooltips exist, are non-empty, and carry the app name + state.
        for state in [
            TrayState::NotRunning,
            TrayState::Ready,
            TrayState::Recording,
            TrayState::Processing,
        ] {
            let en = tray_tooltip(state, "en");
            let da = tray_tooltip(state, "da");
            assert!(en.contains("whisper-dictate"), "EN tooltip: {en}");
            assert!(da.contains("whisper-dictate"), "DA tooltip: {da}");
            // EN and DA differ for every state (the suffix is translated).
            assert_ne!(en, da, "EN/DA tooltip must differ for {state:?}");
        }
        // Spot-check the load-bearing recording tooltip wording.
        assert!(tray_tooltip(TrayState::Recording, "en").contains("recording"));
        assert!(tray_tooltip(TrayState::Recording, "da").contains("optager"));
    }
}
