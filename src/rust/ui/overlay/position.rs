//! Persisted overlay position model.
//!
//! The overlay can dock to one of the four screen corners or float at a custom
//! `(x, y)` pixel coordinate (the user dragged it). The stored value round-trips
//! through a tiny token grammar so it lives comfortably in `config.json` as a
//! plain string:
//!
//! ```text
//! top-left | top-right | bottom-left | bottom-right | custom:<x>,<y>
//! ```
//!
//! Anything else (including an empty string) decodes to the default,
//! `OverlayPosition::BottomRight`, so a hand-edited config can't crash the UI.

use eframe::egui;

/// Margin between the overlay window and the nearest screen edge when docked
/// to a corner. Picked to match the default Windows tray margin so the docked
/// overlay never overlaps the system tray or notification badges.
pub(in crate::ui) const CORNER_MARGIN: f32 = 16.0;

/// The set of overlay placements we persist + the explicit "user dragged it"
/// custom case. Compared by value so the dirty-dot in the settings UI fires on
/// any change. BottomRight matches where most chat/voice overlays live on
/// Windows (next to the tray) and keeps the meter out of the way of the
/// active application's title bar — hence the `#[default]` marker.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(in crate::ui) enum OverlayPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
    /// Free-form drag position. The two floats are screen-space pixels (the
    /// overlay window's top-left). Clamped on render so a multi-monitor unplug
    /// can't strand it off-screen.
    Custom {
        x: f32,
        y: f32,
    },
}

impl OverlayPosition {
    /// Parse a persisted overlay-position string. Unknown / malformed tokens
    /// fall back to the default so a hand-edited config never crashes the UI.
    /// The custom case accepts integer or float `x,y` (NaN/infinity rejected).
    pub(in crate::ui) fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "top-left" | "top_left" | "topleft" => Self::TopLeft,
            "top-right" | "top_right" | "topright" => Self::TopRight,
            "bottom-left" | "bottom_left" | "bottomleft" => Self::BottomLeft,
            "bottom-right" | "bottom_right" | "bottomright" => Self::BottomRight,
            other if other.starts_with("custom:") => {
                let body = &other["custom:".len()..];
                let mut parts = body.split(',');
                let x = parts.next().and_then(|s| s.trim().parse::<f32>().ok());
                let y = parts.next().and_then(|s| s.trim().parse::<f32>().ok());
                match (x, y) {
                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() => Self::Custom { x, y },
                    _ => Self::default(),
                }
            }
            _ => Self::default(),
        }
    }

    /// Serialize as the canonical token form. Round-trips with [`Self::parse`].
    pub(in crate::ui) fn to_storage_string(self) -> String {
        match self {
            Self::TopLeft => "top-left".to_owned(),
            Self::TopRight => "top-right".to_owned(),
            Self::BottomLeft => "bottom-left".to_owned(),
            Self::BottomRight => "bottom-right".to_owned(),
            // Format with no trailing zeros for readable storage; integer
            // positions become "120,80" instead of "120.0,80.0".
            Self::Custom { x, y } => format!("custom:{},{}", trim_float(x), trim_float(y)),
        }
    }

    /// Compute the overlay window's top-left position for a given screen rect
    /// and overlay size. The screen rect is the usable monitor area
    /// (`MonitorSize` minus taskbar) — corners are inset by [`CORNER_MARGIN`]
    /// so the overlay doesn't kiss the edge. Custom positions are CLAMPED to
    /// fit so a saved-then-disconnected-monitor coord still lands on-screen.
    pub(in crate::ui) fn anchor(self, screen: egui::Rect, overlay_size: egui::Vec2) -> egui::Pos2 {
        // Avoid clamping into negative space when the overlay is somehow
        // bigger than the screen rect (tiny monitor, ridiculous DPI).
        let max_x = (screen.max.x - overlay_size.x - CORNER_MARGIN).max(screen.min.x);
        let max_y = (screen.max.y - overlay_size.y - CORNER_MARGIN).max(screen.min.y);
        let min_x = screen.min.x + CORNER_MARGIN;
        let min_y = screen.min.y + CORNER_MARGIN;
        match self {
            Self::TopLeft => egui::pos2(min_x, min_y),
            Self::TopRight => egui::pos2(max_x, min_y),
            Self::BottomLeft => egui::pos2(min_x, max_y),
            Self::BottomRight => egui::pos2(max_x, max_y),
            Self::Custom { x, y } => egui::pos2(
                x.clamp(screen.min.x, max_x.max(screen.min.x)),
                y.clamp(screen.min.y, max_y.max(screen.min.y)),
            ),
        }
    }
}

/// Format a float without trailing zeros or unnecessary `.0`, so the storage
/// string for `Custom { x: 120.0, y: 80.0 }` is "120,80" not "120.0,80.0". A
/// non-finite value (NaN/inf) falls back to `0` so the storage roundtrip stays
/// total — we already gate construction on `is_finite` in `parse`, so this is
/// belt-and-braces.
fn trim_float(value: f32) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        // {:.3} keeps the storage compact while preserving enough precision for
        // physical pixel placement on typical 100 %–200 % scaled displays.
        let s = format!("{value:.3}");
        // Strip trailing zeros (and a dangling dot) so "120.500" → "120.5".
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_known_corner_tokens_case_and_separator_insensitive() {
        assert_eq!(OverlayPosition::parse("top-left"), OverlayPosition::TopLeft);
        assert_eq!(OverlayPosition::parse("TOP_LEFT"), OverlayPosition::TopLeft);
        assert_eq!(OverlayPosition::parse("TopLeft"), OverlayPosition::TopLeft);
        assert_eq!(
            OverlayPosition::parse("bottom-right"),
            OverlayPosition::BottomRight,
        );
    }

    #[test]
    fn parse_unknown_or_malformed_falls_back_to_default() {
        assert_eq!(OverlayPosition::parse(""), OverlayPosition::default());
        assert_eq!(OverlayPosition::parse("center"), OverlayPosition::default());
        assert_eq!(
            OverlayPosition::parse("custom:not,a,coord"),
            OverlayPosition::default(),
        );
        assert_eq!(
            OverlayPosition::parse("custom:NaN,12"),
            OverlayPosition::default(),
        );
    }

    #[test]
    fn parse_accepts_custom_positions_with_whitespace() {
        assert_eq!(
            OverlayPosition::parse("custom:120,80"),
            OverlayPosition::Custom { x: 120.0, y: 80.0 },
        );
        // Whitespace inside the body is tolerated.
        assert_eq!(
            OverlayPosition::parse("custom: 12.5 , -4 "),
            OverlayPosition::Custom { x: 12.5, y: -4.0 },
        );
    }

    #[test]
    fn storage_round_trips_through_parse() {
        for position in [
            OverlayPosition::TopLeft,
            OverlayPosition::TopRight,
            OverlayPosition::BottomLeft,
            OverlayPosition::BottomRight,
            OverlayPosition::Custom { x: 120.0, y: 80.0 },
            OverlayPosition::Custom { x: 12.5, y: -4.0 },
        ] {
            let stored = position.to_storage_string();
            assert_eq!(
                OverlayPosition::parse(&stored),
                position,
                "round-trip failed for {position:?} (stored as {stored:?})",
            );
        }
    }

    #[test]
    fn anchor_docks_overlay_inside_each_corner_with_margin() {
        // 1920x1080 screen at origin, overlay 200x60.
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let size = egui::vec2(200.0, 60.0);
        let m = CORNER_MARGIN;
        assert_eq!(
            OverlayPosition::TopLeft.anchor(screen, size),
            egui::pos2(m, m),
        );
        assert_eq!(
            OverlayPosition::TopRight.anchor(screen, size),
            egui::pos2(1920.0 - 200.0 - m, m),
        );
        assert_eq!(
            OverlayPosition::BottomLeft.anchor(screen, size),
            egui::pos2(m, 1080.0 - 60.0 - m),
        );
        assert_eq!(
            OverlayPosition::BottomRight.anchor(screen, size),
            egui::pos2(1920.0 - 200.0 - m, 1080.0 - 60.0 - m),
        );
    }

    #[test]
    fn anchor_clamps_custom_position_back_into_screen() {
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let size = egui::vec2(200.0, 60.0);
        // Off-screen far right + below: clamped to the bottom-right corner.
        let stranded = OverlayPosition::Custom {
            x: 10_000.0,
            y: 10_000.0,
        };
        let anchored = stranded.anchor(screen, size);
        assert_eq!(anchored.x, 1920.0 - 200.0 - CORNER_MARGIN);
        assert_eq!(anchored.y, 1080.0 - 60.0 - CORNER_MARGIN);

        // Negative coords: clamped to the screen origin.
        let negative = OverlayPosition::Custom {
            x: -500.0,
            y: -500.0,
        };
        let anchored = negative.anchor(screen, size);
        assert_eq!(anchored.x, 0.0);
        assert_eq!(anchored.y, 0.0);
    }

    #[test]
    fn anchor_clamp_survives_overlay_bigger_than_screen() {
        // Tiny screen, oversize overlay: max_{x,y} would go negative without
        // the `.max(screen.min)` guard. Anchor must still land somewhere
        // sensible (here, at the origin) instead of NaN-ing the layout.
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 60.0));
        let size = egui::vec2(400.0, 200.0);
        for position in [
            OverlayPosition::TopLeft,
            OverlayPosition::TopRight,
            OverlayPosition::BottomLeft,
            OverlayPosition::BottomRight,
        ] {
            let pos = position.anchor(screen, size);
            assert!(
                pos.x.is_finite() && pos.y.is_finite(),
                "anchor produced non-finite coords for {position:?}",
            );
            assert!(pos.x >= screen.min.x && pos.y >= screen.min.y);
        }
    }
}
