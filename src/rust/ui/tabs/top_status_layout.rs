//! Pure (egui-free) helpers for the top status bar: the priority-drop fit
//! budget, the left/right width split, and the post-indicator on/off + label/
//! hover logic. Kept out of `shell.rs` so the render code stays small and these
//! functions remain directly unit-testable without an egui context.

use super::*;

/// Returns how many leading cards from `card_widths` fit within `left_width`
/// when each card (after the first) is preceded by `spacing` pixels.
///
/// The first card always counts as fitting (it is rendered with the clip rect
/// as backstop), so the return value is at least 1 when `card_widths` is
/// non-empty. An empty slice returns 0.
///
/// `card_widths` must be the cards' TRUE OUTER widths (inner content min-width
/// plus the Frame's horizontal margin and stroke) — see `status_card_outer_width`
/// in `theme.rs`. Feeding the bare inner `set_min_width` values undercounts each
/// card by the margin+stroke and the cards overflow and clip mid-card.
///
/// Pure function — no egui context required — so it is directly unit-testable.
pub(in crate::ui) fn top_status_cards_fit(
    left_width: f32,
    card_widths: &[f32],
    spacing: f32,
) -> usize {
    if card_widths.is_empty() {
        return 0;
    }
    // Walk the cards in priority order, subtracting each one's cost from the
    // remaining budget. The Status card (index 0) is always rendered with the
    // clip rect as a backstop, so when nothing has been counted yet we force it
    // in (`count == 0`) regardless of budget; every later card must actually fit.
    let mut budget = left_width;
    let mut count = 0usize;
    for (i, &w) in card_widths.iter().enumerate() {
        let cost = if i == 0 { w } else { spacing + w };
        if count == 0 || budget >= cost {
            budget -= cost;
            count += 1;
        } else {
            // Once a card doesn't fit, stop — avoid weird gaps mid-bar.
            break;
        }
    }
    // Guarantee at least 1 (the Status card always renders).
    count.max(1)
}

pub(in crate::ui) fn top_status_controls_width() -> f32 {
    // Start (88) + Stop (78) + compact toggle (34) + inter-button spacing.
    // The caller adds the live item_spacing gap between the two regions.
    232.0
}

/// Width budget for the left (status-cards) portion of the top bar.
///
/// The right-pinned controls are allocated first and always get
/// `controls_width` pixels. The status cards take whatever remains,
/// with a floor of zero so the bar never forces an overflow/overlap —
/// cards simply clip when the window is very narrow.
///
/// Pure function: easy to unit-test without an egui context.
pub(in crate::ui) fn top_status_left_width(total_width: f32, controls_width: f32) -> f32 {
    (total_width - controls_width).max(0.0)
}

/// Whether post-processing is actually active, given the configured processor
/// and mode. Mirrors the worker's gate (`vp_dictate`/`vp_postprocess`): the pass
/// is skipped when the processor is `none`/empty OR the mode is `raw` — and the
/// worker normalizes an EMPTY mode to `raw`, so an unset mode reads as off here
/// too. Case-insensitive like the worker's normalization. Pure so the top-bar
/// indicator's on/off decision is unit-testable.
pub(in crate::ui) fn post_processing_enabled(processor: &str, mode: &str) -> bool {
    let processor = processor.trim().to_ascii_lowercase();
    let mode = mode.trim().to_ascii_lowercase();
    !processor.is_empty() && processor != "none" && !mode.is_empty() && mode != "raw"
}

/// The compact label for the top-bar post indicator: "Post on" when active,
/// "Post off" otherwise. Pure helper over `post_processing_enabled`.
pub(in crate::ui) fn post_indicator_label(
    processor: &str,
    mode: &str,
    raw_language: &str,
) -> &'static str {
    if post_processing_enabled(processor, mode) {
        ui_text(raw_language, UiTextKey::PostOn)
    } else {
        ui_text(raw_language, UiTextKey::PostOff)
    }
}

/// Hover text for the post indicator, naming the configured mode + processor so
/// the user can see the details without leaving the current tab. Pure so it can
/// be unit-tested without an egui context.
pub(in crate::ui) fn post_indicator_hover(processor: &str, mode: &str) -> String {
    let processor = processor.trim();
    let processor = if processor.is_empty() {
        "none"
    } else {
        processor
    };
    let mode = mode.trim();
    let mode = if mode.is_empty() { "raw" } else { mode };
    if post_processing_enabled(processor, mode) {
        format!("Post-processing on — mode: {mode}, processor: {processor}")
    } else {
        format!("Post-processing off — mode: {mode}, processor: {processor}")
    }
}
