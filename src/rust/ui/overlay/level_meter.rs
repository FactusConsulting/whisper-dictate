//! The overlay's audio level meter widget + the smoothing/peak-decay state.
//!
//! The worker streams instantaneous `level` (linearised 0..1), `peak` (the
//! recent maximum) and `raw_dbfs` values (see `vp_capture._emit_audio_level`).
//! Dropping those straight onto a bar produces a jittery "fly-swatter" meter
//! that is hard to read at the overlay's ~200x60 px size. We smooth the bar
//! with classic VU-style attack/decay coefficients and let the peak indicator
//! fall back slowly so a clipping spike stays visible long enough for the eye.
//!
//! The smoothing is intentionally TIME-INDEPENDENT: it takes a `dt` from the
//! frame timer instead of a fixed per-frame factor, so the same overlay reads
//! correctly whether the parent UI repaints at 60 Hz or the 80 ms idle cadence.

use eframe::egui;

/// Time constant (seconds) for the bar's RISE — how quickly the meter slews UP
/// to a louder reading. 50 ms gives the classic VU "the needle jumps but does
/// not snap" feel; slower than this and a real word looks like a slow swell.
const ATTACK_TAU_S: f32 = 0.050;

/// Time constant for the bar's FALL — slower than attack so the eye can track
/// a peak between syllables. 220 ms matches the standard PPM meter half-life.
const DECAY_TAU_S: f32 = 0.220;

/// Half-life of the peak hold indicator. The peak slides down at this rate so
/// a clipping spike stays visible for ~1 s before the bar reclaims it.
const PEAK_DECAY_TAU_S: f32 = 0.900;

/// A single audio-meter frame as it comes off the worker event stream. Held
/// separately from the live smoothing state so the render loop has a clean
/// "what the worker just told us" snapshot to feed into `tick`.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::ui) struct MeterFrame {
    /// Linearised 0..1 instantaneous level (the worker already clamps this).
    pub level: f32,
    /// Optional peak hint reported alongside `level`. When `None` the smoother
    /// falls back to the live level for the peak indicator.
    pub peak: Option<f32>,
    /// Optional raw dBFS for the text label next to the bar. `None` keeps the
    /// previous label so the readout doesn't blink to "--" between frames.
    pub raw_dbfs: Option<f32>,
}

/// Live, time-smoothed state behind the overlay's bar.
///
/// Constructed once and stepped every frame with the latest [`MeterFrame`]
/// + the elapsed `dt`. Kept tiny (three floats + an Option) so cloning into
///   the overlay's per-viewport state map is cheap.
#[derive(Debug, Clone, Copy, Default)]
pub(in crate::ui) struct LevelMeterState {
    /// Smoothed 0..1 bar height. Drives the filled rectangle.
    pub display_level: f32,
    /// Smoothed 0..1 peak indicator. Drives the thin horizontal tick.
    pub display_peak: f32,
    /// Last raw dBFS the worker reported. `None` until the first audio event.
    pub last_dbfs: Option<f32>,
}

impl LevelMeterState {
    /// Slew the bar/peak towards the latest frame using the configured time
    /// constants. `dt` is the elapsed time since the previous call (seconds).
    /// A zero/negative dt is a no-op so a paused viewport can't NaN the state.
    pub(in crate::ui) fn tick(&mut self, frame: MeterFrame, dt: f32) {
        if !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let target_level = frame.level.clamp(0.0, 1.0);
        // Asymmetric one-pole: ATTACK_TAU on the way up, DECAY_TAU on the way
        // down — the textbook VU response.
        let level_tau = if target_level > self.display_level {
            ATTACK_TAU_S
        } else {
            DECAY_TAU_S
        };
        self.display_level = slew(self.display_level, target_level, dt, level_tau);

        // Peak: rise instantly to the worker-reported peak (or live level if
        // none), decay slowly so a clipping spike stays visible.
        let target_peak = frame
            .peak
            .map(|p| p.clamp(0.0, 1.0))
            .unwrap_or(target_level)
            .max(self.display_level);
        if target_peak > self.display_peak {
            self.display_peak = target_peak;
        } else {
            self.display_peak = slew(self.display_peak, target_peak, dt, PEAK_DECAY_TAU_S);
        }

        if frame.raw_dbfs.is_some() {
            self.last_dbfs = frame.raw_dbfs;
        }
    }

    /// Drain the smoothed state back to zero (capture stopped / overlay
    /// hidden). Cheaper than re-constructing because the borrow is `&mut`.
    pub(in crate::ui) fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One-pole exponential slew from `current` to `target` over the given `dt`
/// using time-constant `tau`. Returns a value the same side of `target` as
/// `current` (never overshoots) and is monotonic in `dt`.
fn slew(current: f32, target: f32, dt: f32, tau: f32) -> f32 {
    if tau <= 0.0 {
        return target;
    }
    // `1 - exp(-dt/tau)` is the classic single-pole low-pass coefficient.
    // Clamped to [0,1] so a giant frame stutter can't push the meter PAST
    // its target (which would invert the asymmetry on the next tick).
    let alpha = (1.0 - (-dt / tau).exp()).clamp(0.0, 1.0);
    current + (target - current) * alpha
}

/// Paint the bar + peak indicator + dBFS readout. The drawn rect uses
/// `ui.available_width()` so the caller controls the bar size by sizing the
/// surrounding panel — this keeps the meter widget itself layout-agnostic.
pub(in crate::ui) fn draw_level_meter(
    ui: &mut egui::Ui,
    state: &LevelMeterState,
    palette: &super::OverlayPalette,
) {
    // Two-row layout: numeric readout on the left of the bar, bar fills the
    // rest of the row. Wrapping the bar in `ui.allocate_ui_with_layout` keeps
    // the dBFS label vertically centred no matter how tall the bar is.
    let row_height = 18.0_f32;
    let label = match state.last_dbfs {
        Some(db) if db.is_finite() => format!("{db:>5.1} dBFS"),
        _ => "  --- dBFS".to_owned(),
    };
    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    // dBFS label takes the leftmost 72 px so the bar's left edge stays
    // aligned across frames regardless of the readout's width.
    let label_width = 72.0_f32;
    let label_rect = egui::Rect::from_min_size(rect.min, egui::vec2(label_width, rect.height()));
    let bar_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + label_width + 6.0, rect.min.y + 2.0),
        egui::pos2(rect.max.x, rect.max.y - 2.0),
    );

    painter.text(
        label_rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        palette.text,
    );

    // Trough.
    painter.rect_filled(bar_rect, 2.0, palette.bar_trough);

    // Filled portion. The colour shifts from neutral to warning past the
    // user-noticeable hot zone (~-12 dBFS ≈ 0.79 linear) and to clipping in
    // the top 10 %, matching the worker's `target_dbfs`/`min_input_dbfs`
    // guidance.
    let level = state.display_level.clamp(0.0, 1.0);
    if level > 0.0 {
        let fill_color = bar_color_for_level(level, palette);
        let fill_max_x = bar_rect.min.x + bar_rect.width() * level;
        let fill_rect =
            egui::Rect::from_min_max(bar_rect.min, egui::pos2(fill_max_x, bar_rect.max.y));
        painter.rect_filled(fill_rect, 2.0, fill_color);
    }

    // Peak indicator: a 2-px vertical tick at the smoothed peak position.
    let peak = state.display_peak.clamp(0.0, 1.0);
    if peak > 0.005 {
        let peak_x = bar_rect.min.x + bar_rect.width() * peak;
        let tick = egui::Rect::from_min_max(
            egui::pos2(peak_x - 1.0, bar_rect.min.y),
            egui::pos2(peak_x + 1.0, bar_rect.max.y),
        );
        painter.rect_filled(tick, 0.0, palette.peak_tick);
    }
}

/// Pick the bar fill colour for a linearised 0..1 level. Below ~-12 dBFS
/// (0.79 linear) use the neutral accent; in the hot zone shift to a warning
/// amber; in the top 10 % flip to the error colour so visible clipping is
/// unmistakable. Pure function — easy to unit-test.
pub(in crate::ui) fn bar_color_for_level(
    level: f32,
    palette: &super::OverlayPalette,
) -> egui::Color32 {
    if level >= 0.90 {
        palette.bar_clip
    } else if level >= 0.79 {
        palette.bar_hot
    } else {
        palette.bar_nominal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn tick_rises_quickly_towards_a_louder_target() {
        let mut state = LevelMeterState::default();
        // Single 16 ms frame at the worker's 80 ms idle cadence: the bar
        // should already be a chunky fraction of the way to the target.
        state.tick(
            MeterFrame {
                level: 0.8,
                ..MeterFrame::default()
            },
            0.016,
        );
        // alpha = 1 - exp(-0.016/0.050) ≈ 0.273, so display ≈ 0.218.
        assert!(state.display_level > 0.15 && state.display_level < 0.30);
    }

    #[test]
    fn tick_decays_slowly_when_target_drops() {
        let mut state = LevelMeterState {
            display_level: 0.8,
            display_peak: 0.8,
            last_dbfs: Some(-12.0),
        };
        // 16 ms with target 0: alpha = 1 - exp(-0.016/0.220) ≈ 0.070,
        // so display ≈ 0.8 - 0.056 = 0.744 (i.e. mostly unchanged).
        state.tick(
            MeterFrame {
                level: 0.0,
                ..MeterFrame::default()
            },
            0.016,
        );
        assert!(state.display_level > 0.7);
    }

    #[test]
    fn tick_is_a_noop_on_zero_or_negative_dt() {
        let mut state = LevelMeterState {
            display_level: 0.5,
            display_peak: 0.5,
            last_dbfs: Some(-20.0),
        };
        let original = state;
        state.tick(
            MeterFrame {
                level: 1.0,
                ..MeterFrame::default()
            },
            0.0,
        );
        assert_eq!(state.display_level, original.display_level);
        state.tick(
            MeterFrame {
                level: 1.0,
                ..MeterFrame::default()
            },
            -0.1,
        );
        assert_eq!(state.display_level, original.display_level);
    }

    #[test]
    fn peak_indicator_jumps_up_instantly_and_then_decays() {
        let mut state = LevelMeterState::default();
        state.tick(
            MeterFrame {
                level: 0.4,
                peak: Some(0.9),
                ..MeterFrame::default()
            },
            0.016,
        );
        // Peak rose immediately to the worker-reported value...
        assert_eq!(state.display_peak, 0.9);

        // ...and falls by the slow decay constant on the next quiet frame.
        // alpha = 1 - exp(-0.016/0.900) ≈ 0.0176, so 0.9 → ~0.8842.
        state.tick(
            MeterFrame {
                level: 0.0,
                peak: Some(0.0),
                ..MeterFrame::default()
            },
            0.016,
        );
        assert!(approx_eq(state.display_peak, 0.884, 0.01));
    }

    #[test]
    fn last_dbfs_persists_across_frames_without_a_new_reading() {
        let mut state = LevelMeterState::default();
        state.tick(
            MeterFrame {
                level: 0.5,
                raw_dbfs: Some(-14.5),
                ..MeterFrame::default()
            },
            0.016,
        );
        assert_eq!(state.last_dbfs, Some(-14.5));
        // A frame without a fresh dBFS keeps the last reading so the readout
        // doesn't blink to "--- dBFS" between events.
        state.tick(
            MeterFrame {
                level: 0.6,
                ..MeterFrame::default()
            },
            0.016,
        );
        assert_eq!(state.last_dbfs, Some(-14.5));
    }

    #[test]
    fn reset_returns_state_to_default() {
        let mut state = LevelMeterState {
            display_level: 0.7,
            display_peak: 0.9,
            last_dbfs: Some(-3.0),
        };
        state.reset();
        assert_eq!(state.display_level, 0.0);
        assert_eq!(state.display_peak, 0.0);
        assert_eq!(state.last_dbfs, None);
    }

    #[test]
    fn bar_color_shifts_through_nominal_hot_and_clip_zones() {
        let palette = super::super::OverlayPalette {
            bar_nominal: egui::Color32::GREEN,
            bar_hot: egui::Color32::YELLOW,
            bar_clip: egui::Color32::RED,
            ..super::super::OverlayPalette::default()
        };
        assert_eq!(bar_color_for_level(0.0, &palette), egui::Color32::GREEN);
        assert_eq!(bar_color_for_level(0.5, &palette), egui::Color32::GREEN);
        assert_eq!(bar_color_for_level(0.79, &palette), egui::Color32::YELLOW);
        assert_eq!(bar_color_for_level(0.85, &palette), egui::Color32::YELLOW);
        assert_eq!(bar_color_for_level(0.90, &palette), egui::Color32::RED);
        assert_eq!(bar_color_for_level(1.0, &palette), egui::Color32::RED);
    }
}
