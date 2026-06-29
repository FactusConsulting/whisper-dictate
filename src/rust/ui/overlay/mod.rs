//! Recording-overlay window: a small always-on-top viewport that mirrors the
//! dictation state with a live audio level meter (Issue #320).
//!
//! The overlay lives in its OWN egui viewport, spawned from the main app's
//! `update` loop via [`render_recording_overlay`]. It is a sibling of the
//! main window (not a child of `CentralPanel`), so it can float above any
//! application without stealing focus and disappears the instant the
//! visibility rule in [`settings::should_show_overlay`] returns `false`.
//!
//! Submodules:
//! - [`level_meter`] — the bar widget + time-smoothed [`level_meter::LevelMeterState`]
//! - [`position`]    — the four-corner / custom anchor model + clamp
//! - [`settings`]    — the worker-event → phase mapping + visibility rule

use eframe::egui;
use std::time::Instant;

#[cfg(test)]
use crate::runtime::RuntimeState;

pub(in crate::ui) mod level_meter;
pub(in crate::ui) mod position;
pub(in crate::ui) mod settings;

pub(in crate::ui) use level_meter::{LevelMeterState, MeterFrame};
pub(in crate::ui) use position::OverlayPosition;
pub(in crate::ui) use settings::{should_show_overlay, OverlayConfig, OverlayPhase};

/// Default size of the overlay window — small enough to live in a screen
/// corner without crowding the active app, large enough to fit the dBFS
/// readout plus the bar at a readable 12 px font.
pub(in crate::ui) const OVERLAY_DEFAULT_SIZE: egui::Vec2 = egui::vec2(220.0, 64.0);

/// Stable viewport id for the overlay. Stable so repeated calls to
/// `show_viewport_immediate` keep targeting the same OS window instead of
/// spawning a new one each frame.
pub(in crate::ui) fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("whisper-dictate.recording-overlay")
}

/// Per-process state for the overlay window. Lives on `WhisperDictateApp` and
/// is stepped each frame from `update_recording_overlay`.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::ui) struct OverlayState {
    /// Time-smoothed audio meter (bar + peak indicator).
    pub meter: LevelMeterState,
    /// Last time `tick` was called, so the meter slew uses real elapsed dt.
    pub last_tick: Option<Instant>,
    /// Whether the overlay was visible on the previous frame, used to decide
    /// whether to reset the meter when it disappears.
    pub was_visible: bool,
}

impl OverlayState {
    /// Step the smoother with the latest audio reading. Pulls dt from the
    /// monotonic clock so the meter looks the same at any repaint cadence.
    pub(in crate::ui) fn tick(&mut self, frame: MeterFrame) {
        let now = Instant::now();
        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t).as_secs_f32())
            // First tick: assume one full repaint interval has elapsed. Any
            // sensible value works because the smoother is monotonic in dt.
            .unwrap_or(0.080);
        self.last_tick = Some(now);
        // Clamp the dt so a frame stutter (paused app, background tab) can't
        // snap the meter to the target on the very next tick.
        let dt = dt.min(0.5);
        self.meter.tick(frame, dt);
    }

    /// Forget the smoother + the dt clock, called when the overlay hides so
    /// the next opening doesn't snap from a stale bar.
    pub(in crate::ui) fn reset(&mut self) {
        self.meter.reset();
        self.last_tick = None;
    }
}

/// The five colours the overlay paints with. Lifted off the main app's
/// palette so the overlay can be unit-tested without a full theme. Defaults
/// (`Color32::TRANSPARENT`) make `OverlayPalette::default()` valid for tests
/// — production callers always pass a real palette via
/// [`OverlayPalette::from_main_palette`].
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::ui) struct OverlayPalette {
    pub bg: egui::Color32,
    pub text: egui::Color32,
    pub bar_trough: egui::Color32,
    pub bar_nominal: egui::Color32,
    pub bar_hot: egui::Color32,
    pub bar_clip: egui::Color32,
    pub peak_tick: egui::Color32,
}

impl OverlayPalette {
    /// Derive the overlay palette from the main app's [`super::UiPalette`] so
    /// the overlay matches the user's theme without needing its own setting.
    pub(in crate::ui) fn from_main_palette(palette: &super::UiPalette) -> Self {
        Self {
            // Solid panel background (no transparency) so the meter stays
            // readable over a busy desktop.
            bg: palette.panel_bg,
            text: palette.text,
            bar_trough: palette.surface_bg,
            bar_nominal: palette.accent_blue,
            bar_hot: palette.warn_text,
            bar_clip: palette.error_text,
            peak_tick: palette.text,
        }
    }
}

/// Optional argument bundle for [`render_recording_overlay`]. Bundled into a
/// struct because Rust closures over `&mut self` get unwieldy past four
/// arguments; nothing here is performance-critical.
pub(in crate::ui) struct OverlayRender<'a> {
    pub config: OverlayConfig,
    pub phase: OverlayPhase,
    pub palette: OverlayPalette,
    pub state: &'a mut OverlayState,
    pub position: OverlayPosition,
    pub active_device: &'a str,
    /// Mutable handle so dragging the window persists into the typed
    /// `AppSettings.overlay_position`. The caller is responsible for the
    /// subsequent dirty-flag bookkeeping.
    pub on_drag: &'a mut dyn FnMut(OverlayPosition),
}

/// Spawn (or refresh) the overlay viewport. A no-op when
/// [`should_show_overlay`] returns `false`.
///
/// Called every frame from `WhisperDictateApp::update`; egui keeps the OS
/// window alive across calls because we re-use the same [`overlay_viewport_id`].
/// When the visibility rule flips to `false` we simply STOP calling this
/// function; egui then tears the viewport down at the end of the next pass.
pub(in crate::ui) fn render_recording_overlay(ctx: &egui::Context, args: OverlayRender<'_>) {
    if !should_show_overlay(args.config, args.phase) {
        if args.state.was_visible {
            args.state.reset();
            args.state.was_visible = false;
        }
        return;
    }
    args.state.was_visible = true;

    let screen = ctx
        .input(|i| i.viewport().monitor_size)
        .map(|size| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size))
        // Reasonable monitor-default fallback when the platform doesn't
        // report a size (some headless / wayland-less compositors).
        .unwrap_or_else(|| {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0))
        });
    let pos = args.position.anchor(screen, OVERLAY_DEFAULT_SIZE);
    let builder = egui::ViewportBuilder::default()
        .with_title("whisper-dictate overlay")
        .with_inner_size(OVERLAY_DEFAULT_SIZE)
        .with_min_inner_size(egui::vec2(160.0, 48.0))
        .with_position(pos)
        .with_resizable(false)
        .with_decorations(false)
        .with_taskbar(false)
        .with_always_on_top();

    let OverlayPalette { bg, .. } = args.palette;
    let phase = args.phase;
    let device = args.active_device.to_owned();
    let position = args.position;
    let palette = args.palette;
    let on_drag = args.on_drag;
    let state = args.state;

    ctx.show_viewport_immediate(overlay_viewport_id(), builder, move |ui, _class| {
        // Solid background so the meter is legible over a busy app behind it.
        let frame = egui::Frame::default()
            .fill(bg)
            .inner_margin(egui::Margin::symmetric(10, 8));
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            draw_overlay_body(ui, phase, &device, state, &palette);
        });
        // Pick up a drag on the viewport so the user can re-position the
        // overlay; report it back to the caller as a Custom position.
        let outer_response = ui.interact(
            ui.max_rect(),
            ui.id().with("overlay_drag"),
            egui::Sense::drag(),
        );
        if outer_response.dragged() {
            let drag = outer_response.drag_delta();
            // Resolve the current position to absolute pixels via the same
            // anchor function so a single drag flips a corner→custom in one
            // shot, with no jump.
            let screen = ui
                .ctx()
                .input(|i| i.viewport().monitor_size)
                .map(|size| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size))
                .unwrap_or_else(|| {
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0))
                });
            let base = position.anchor(screen, OVERLAY_DEFAULT_SIZE);
            let new_pos = OverlayPosition::Custom {
                x: base.x + drag.x,
                y: base.y + drag.y,
            };
            (on_drag)(new_pos);
        }
    });
}

/// Inner painter for the overlay body. Split out from
/// [`render_recording_overlay`] so the viewport callback stays a thin shim
/// and the body's structure can be checked at unit-test time (#320 follow-up).
fn draw_overlay_body(
    ui: &mut egui::Ui,
    phase: OverlayPhase,
    device: &str,
    state: &mut OverlayState,
    palette: &OverlayPalette,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            // Small coloured dot mirroring the phase (matches the tray colour
            // scheme so the overlay reads at a glance).
            let dot_radius = 5.0;
            let (dot_rect, _) = ui.allocate_exact_size(
                egui::vec2(dot_radius * 2.5, dot_radius * 2.5),
                egui::Sense::hover(),
            );
            let dot_color = match phase {
                OverlayPhase::Recording => palette.bar_clip,
                OverlayPhase::Opening => palette.bar_hot,
                OverlayPhase::Transcribing => palette.bar_nominal,
                OverlayPhase::Idle => palette.bar_trough,
            };
            ui.painter()
                .circle_filled(dot_rect.center(), dot_radius, dot_color);
            ui.add(egui::Label::new(
                egui::RichText::new(phase.label())
                    .color(palette.text)
                    .strong(),
            ));
            ui.add_space(6.0);
            if !device.is_empty() {
                ui.add(egui::Label::new(
                    egui::RichText::new(short_device_label(device))
                        .color(palette.text)
                        .weak(),
                ));
            }
        });
        ui.add_space(4.0);
        level_meter::draw_level_meter(ui, &state.meter, palette);
    });
}

/// Truncate the device label so a long mic name (e.g. the full Bluetooth
/// product string) doesn't blow out the overlay's compact width. Pure helper
/// — no egui state, easy to unit-test.
pub(in crate::ui) fn short_device_label(raw: &str) -> String {
    const MAX: usize = 24;
    let trimmed = raw.trim();
    if trimmed.chars().count() <= MAX {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(MAX - 1).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_viewport_id_is_stable_across_calls() {
        // Stability is what lets `show_viewport_immediate` keep reusing the
        // same OS window each frame instead of spawning a new one. Cheap
        // sanity check.
        assert_eq!(overlay_viewport_id(), overlay_viewport_id());
    }

    #[test]
    fn overlay_state_tick_drives_smoother_with_real_dt() {
        let mut state = OverlayState::default();
        state.tick(MeterFrame {
            level: 0.5,
            ..MeterFrame::default()
        });
        // First tick: dt is the bootstrap 80 ms, alpha ≈ 0.798, so display
        // jumps a substantial fraction towards 0.5.
        assert!(state.meter.display_level > 0.2);
        assert!(state.last_tick.is_some());
    }

    #[test]
    fn overlay_state_reset_clears_meter_and_dt_clock() {
        let mut state = OverlayState::default();
        state.tick(MeterFrame {
            level: 0.9,
            ..MeterFrame::default()
        });
        assert!(state.last_tick.is_some());
        state.reset();
        assert_eq!(state.meter.display_level, 0.0);
        assert!(state.last_tick.is_none());
    }

    #[test]
    fn short_device_label_passes_short_names_through_untouched() {
        assert_eq!(short_device_label("Yeti X"), "Yeti X");
        assert_eq!(short_device_label("  Yeti  "), "Yeti");
    }

    #[test]
    fn short_device_label_truncates_long_names_with_ellipsis() {
        let long = "Logitech G PRO X Wireless Lightspeed Gaming Headset";
        let out = short_device_label(long);
        assert!(out.chars().count() <= 24);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn render_skip_resets_meter_when_overlay_hides() {
        // The mid-render `OverlayRender` lifetime makes the actual `render`
        // function awkward to call without an egui ctx, but we can drive the
        // state-reset side effect directly: this is what protects the user
        // from seeing a stale bar the next time the overlay re-opens.
        let mut state = OverlayState::default();
        state.meter.display_level = 0.5;
        state.last_tick = Some(Instant::now());
        state.was_visible = true;
        let config = OverlayConfig {
            enabled: true,
            show_on_idle: false,
        };
        let phase = OverlayPhase::Idle;
        if !should_show_overlay(config, phase) {
            state.reset();
            state.was_visible = false;
        }
        assert_eq!(state.meter.display_level, 0.0);
        assert!(state.last_tick.is_none());
        assert!(!state.was_visible);
    }

    #[test]
    fn overlay_phase_from_runtime_stopped_is_idle_even_with_recording_status() {
        // Mirrors the bug we're guarding against: if the worker exits while a
        // "recording" status is in flight, the overlay must NOT keep showing.
        let phase = OverlayPhase::from_worker_state(RuntimeState::Stopped, "recording", true, true);
        assert_eq!(phase, OverlayPhase::Idle);
    }
}
