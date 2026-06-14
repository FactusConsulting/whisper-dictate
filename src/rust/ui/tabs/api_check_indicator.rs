//! Inline ✓/✗/testing indicator shown next to the "Test post API" / "Test
//! cloud API" buttons (closes #263). The API-test result already lands in the
//! bottom status bar via `post_api_key_status` / `stt_api_key_status` (set by
//! `set_api_check_status` in `tasks.rs`), but that is easy to miss — this puts
//! the verdict right at the button.
//!
//! The state→indicator DECISION is a pure function ([`classify_api_check`]) with
//! no egui dependency so it is unit-tested directly; the egui rendering
//! ([`api_check_indicator_parts`] + the tab call sites) stays a thin shell that
//! only maps the classified state onto a palette colour and a material icon.

use super::*;

/// Which indicator (if any) to show next to a Test-API button, derived purely
/// from the persisted status string and whether this check is in flight.
///
/// The status strings are produced by `set_api_check_status` (tasks.rs): they
/// start with `"[OK] "` on success and `"[ERROR] "` on failure. An empty (or
/// otherwise un-prefixed) status means no check has produced a verdict yet, so
/// nothing is shown. The in-flight flag wins over any prior status so a re-test
/// shows "testing…" instead of the stale previous result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum ApiCheckState {
    /// A check for this button is currently running.
    Testing,
    /// The last check succeeded (`[OK] …`).
    Ok,
    /// The last check failed (`[ERROR] …`).
    Error,
    /// No verdict to show (no check run yet, or an un-prefixed status such as the
    /// transient "Opened … page." message).
    None,
}

/// Classify the inline indicator state from the persisted status string and the
/// in-flight flag. Pure — no egui, no palette — so the OK/ERROR/testing/empty
/// branching is unit-testable without an egui context.
pub(in crate::ui) fn classify_api_check(status: &str, in_flight: bool) -> ApiCheckState {
    if in_flight {
        return ApiCheckState::Testing;
    }
    let status = status.trim_start();
    if status.starts_with("[OK]") {
        ApiCheckState::Ok
    } else if status.starts_with("[ERROR]") {
        ApiCheckState::Error
    } else {
        ApiCheckState::None
    }
}

/// Map the classified state onto the inline (icon, colour, hover-text) the tab
/// renders next to the Test button, or `None` when nothing should be drawn.
///
/// - `Testing` → a neutral muted "testing…" hint (no icon — the spinner is added
///   alongside by the render shell);
/// - `Ok`      → a GREEN `ICON_CHECK_CIRCLE`, hover = the full status message;
/// - `Error`   → a RED `ICON_ERROR`, hover = the full status message;
/// - `None`    → `None`.
///
/// Thin: the only egui types it touches are the palette colour and the icon
/// codepoint string; all the decision logic lives in [`classify_api_check`].
pub(in crate::ui) fn api_check_indicator_parts(
    status: &str,
    in_flight: bool,
    palette: UiPalette,
) -> Option<(&'static str, egui::Color32, String)> {
    use egui_material_icons::icons;
    match classify_api_check(status, in_flight) {
        ApiCheckState::Testing => Some((
            // Hourglass conveys "in progress"; the render shell also shows a
            // spinner next to it. Muted colour so it reads as a transient hint.
            icons::ICON_HOURGLASS_EMPTY.codepoint,
            palette.text_muted,
            "testing…".to_owned(),
        )),
        ApiCheckState::Ok => Some((
            icons::ICON_CHECK_CIRCLE.codepoint,
            palette.ok_text,
            status.trim().to_owned(),
        )),
        ApiCheckState::Error => Some((
            icons::ICON_ERROR.codepoint,
            palette.error_text,
            status.trim().to_owned(),
        )),
        ApiCheckState::None => None,
    }
}

/// Render the inline Test-API indicator into the current `ui.horizontal(..)` row,
/// right after the Test button. Thin shell over [`api_check_indicator_parts`]:
/// nothing is drawn for the `None` state; while in flight a spinner + muted
/// "testing…" label (matching the microphone Test pattern); once a verdict
/// exists, a green ✓ / red ✗ icon whose hover is the full status message.
///
/// Shared by `post.rs` and `speech.rs` so the two Test buttons render an
/// identical indicator from their respective status strings.
pub(in crate::ui) fn render_api_check_indicator(
    ui: &mut egui::Ui,
    status: &str,
    in_flight: bool,
    palette: UiPalette,
) {
    let Some((icon, color, hover)) = api_check_indicator_parts(status, in_flight, palette) else {
        return;
    };
    ui.add_space(4.0);
    if in_flight {
        // The spinner carries the motion, so the hourglass icon is unused here;
        // just show the muted "testing…" label next to the spinner.
        ui.add(egui::Spinner::new().size(14.0));
        ui.label(egui::RichText::new(hover).color(color));
    } else {
        // Render ONLY the ✓/✗ icon inline — the full status message is on hover,
        // not spilled into a long inline label.
        ui.label(egui::RichText::new(icon).color(color))
            .on_hover_text(hover);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::ui_palette;

    #[test]
    fn ok_status_classifies_ok() {
        assert_eq!(
            classify_api_check("[OK] post API check passed: groq", false),
            ApiCheckState::Ok
        );
        // Leading whitespace before the marker is tolerated.
        assert_eq!(
            classify_api_check("  [OK] passed", false),
            ApiCheckState::Ok
        );
    }

    #[test]
    fn error_status_classifies_error() {
        assert_eq!(
            classify_api_check("[ERROR] cloud API check failed: 401", false),
            ApiCheckState::Error
        );
    }

    #[test]
    fn in_flight_wins_over_any_prior_status() {
        // A re-test shows "testing…" even if a previous OK/ERROR is still stored.
        assert_eq!(
            classify_api_check("[OK] passed", true),
            ApiCheckState::Testing
        );
        assert_eq!(
            classify_api_check("[ERROR] boom", true),
            ApiCheckState::Testing
        );
        assert_eq!(classify_api_check("", true), ApiCheckState::Testing);
    }

    #[test]
    fn empty_or_unprefixed_status_classifies_none() {
        assert_eq!(classify_api_check("", false), ApiCheckState::None);
        // The transient "Opened Groq API keys page." message is not a verdict.
        assert_eq!(
            classify_api_check("Opened Groq API keys page.", false),
            ApiCheckState::None
        );
    }

    #[test]
    fn ok_parts_are_green_check_with_message_hover() {
        let palette = ui_palette("dark");
        let (icon, color, hover) =
            api_check_indicator_parts("[OK] post API check passed", false, palette)
                .expect("OK status renders an indicator");
        assert_eq!(
            icon,
            egui_material_icons::icons::ICON_CHECK_CIRCLE.codepoint
        );
        assert_eq!(color, palette.ok_text);
        assert_eq!(hover, "[OK] post API check passed");
    }

    #[test]
    fn error_parts_are_red_error_with_message_hover() {
        let palette = ui_palette("dark");
        let (icon, color, hover) =
            api_check_indicator_parts("[ERROR] cloud API check failed: 401", false, palette)
                .expect("ERROR status renders an indicator");
        assert_eq!(icon, egui_material_icons::icons::ICON_ERROR.codepoint);
        assert_eq!(color, palette.error_text);
        assert_eq!(hover, "[ERROR] cloud API check failed: 401");
    }

    #[test]
    fn testing_parts_are_muted_hint() {
        let palette = ui_palette("dark");
        let (icon, color, hover) = api_check_indicator_parts("", true, palette)
            .expect("in-flight check renders a testing hint");
        assert_eq!(
            icon,
            egui_material_icons::icons::ICON_HOURGLASS_EMPTY.codepoint
        );
        assert_eq!(color, palette.text_muted);
        assert_eq!(hover, "testing…");
    }

    #[test]
    fn none_state_renders_no_indicator() {
        let palette = ui_palette("dark");
        assert!(api_check_indicator_parts("", false, palette).is_none());
        assert!(api_check_indicator_parts("Opened page.", false, palette).is_none());
    }
}
