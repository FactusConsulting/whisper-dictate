//! The pure `step_text_scale` helper and its unit tests. The egui widget
//! `text_scale_stepper` (its only consumer) lives in `tabs/system.rs` so it
//! falls under the `src/rust/ui/tabs/**` Sonar coverage exclusion — render
//! code cannot be unit-tested in isolation.

/// The UI text-scale clamp range, kept in lockstep with `theme::layout_scale` /
/// `apply_ui_theme` (which clamp the parsed scale to the same bounds). Stepper
/// buttons clamp to this so they can never push the value outside what the
/// theme parser would accept.
const UI_TEXT_SCALE_MIN: f32 = 0.85;
const UI_TEXT_SCALE_MAX: f32 = 1.6;

/// Apply a `delta` step to a UI-text-scale string and return the formatted,
/// clamped result. Unparseable input falls back to 1.0 before stepping; the
/// result is clamped to the theme's [0.85, 1.6] range and formatted trimmed
/// (e.g. "1.15", "1" instead of "1.000000"). Pure so it is unit-testable.
pub(in crate::ui) fn step_text_scale(raw: &str, delta: f32) -> String {
    let current = raw.trim().parse::<f32>().unwrap_or(1.0);
    let stepped = (current + delta).clamp(UI_TEXT_SCALE_MIN, UI_TEXT_SCALE_MAX);
    // Round to 2 decimals so repeated 0.05 steps don't accumulate float noise,
    // then trim a trailing ".00"/".x0" for a clean display.
    let rounded = (stepped * 100.0).round() / 100.0;
    let mut text = format!("{rounded:.2}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::step_text_scale;

    #[test]
    fn step_text_scale_handles_garbage_input() {
        // Unparseable input falls back to 1.0 before stepping.
        assert_eq!(step_text_scale("nonsense", 0.05), "1.05");
        assert_eq!(step_text_scale("", 0.05), "1.05");
        assert_eq!(step_text_scale("  ", -0.05), "0.95");
    }

    #[test]
    fn step_text_scale_clamps_both_ends() {
        // Stepping up past the max clamps to 1.6.
        assert_eq!(step_text_scale("1.6", 0.05), "1.6");
        assert_eq!(step_text_scale("100", 0.05), "1.6");
        // Stepping down past the min clamps to 0.85.
        assert_eq!(step_text_scale("0.85", -0.05), "0.85");
        assert_eq!(step_text_scale("0", -0.05), "0.85");
    }

    #[test]
    fn step_text_scale_formats_trimmed() {
        // Trailing zeros are trimmed; whitespace is tolerated.
        assert_eq!(step_text_scale(" 1.10 ", 0.05), "1.15");
        assert_eq!(step_text_scale("1.15", -0.05), "1.1");
        assert_eq!(step_text_scale("0.95", 0.05), "1");
    }
}
