use super::*;
use crate::ui::tabs::{top_status_cards_fit, top_status_controls_width, top_status_left_width};

#[test]
fn shell_chrome_dimensions_scale_with_ui_text() {
    assert!(top_status_bar_height("1.15") >= 73.0);
    assert_eq!(top_status_bar_height("bad"), 64.0);
    assert_eq!(top_status_bar_height("3.0"), 102.4);
    assert!(sidebar_width("1.15") >= 188.0);
    assert_eq!(sidebar_width("bad"), 164.0);
    assert!((sidebar_width("3.0") - 262.4).abs() < 0.001);
}

#[test]
fn top_status_left_width_gives_controls_priority_and_never_goes_negative() {
    // Pin the REAL production budget so a future change to the controls
    // width is caught here, not by a stale hardcoded copy.
    let controls = top_status_controls_width();

    // Normal window: left side gets the surplus.
    assert!((top_status_left_width(1000.0, controls) - 768.0).abs() < 0.001);

    // Exactly fitting: left side gets zero, no overlap.
    assert!((top_status_left_width(controls, controls)).abs() < 0.001);

    // Narrower than controls (edge case): left side is floored at zero,
    // never negative — the cards simply clip rather than forcing an overlap.
    assert_eq!(top_status_left_width(100.0, controls), 0.0);
    assert_eq!(top_status_left_width(0.0, controls), 0.0);
}

#[test]
fn top_status_cards_fit_all_cards_fit_when_budget_is_large() {
    // 4 cards: 100, 100, 200, 120.  With spacing 10, total = 100+10+100+10+200+10+120 = 550.
    let widths = [100.0_f32, 100.0, 200.0, 120.0];
    assert_eq!(top_status_cards_fit(600.0, &widths, 10.0), 4);
    assert_eq!(top_status_cards_fit(550.0, &widths, 10.0), 4);
}

#[test]
fn top_status_cards_fit_partial_fit_exact_boundary() {
    // Budget exactly covers first two cards: 100 + 10 + 100 = 210 — fits 2.
    // Third card would need 210 + 10 + 200 = 420 — doesn't fit with budget 210.
    let widths = [100.0_f32, 100.0, 200.0, 120.0];
    assert_eq!(top_status_cards_fit(210.0, &widths, 10.0), 2);
    // One pixel under: only the first card fits (but at least 1 is always returned).
    assert_eq!(top_status_cards_fit(209.0, &widths, 10.0), 1);
}

#[test]
fn top_status_cards_fit_status_card_always_renders() {
    // Even a zero-width budget must return 1 (Status card always shown, clip as
    // backstop).
    let widths = [100.0_f32, 100.0, 200.0];
    assert_eq!(top_status_cards_fit(0.0, &widths, 10.0), 1);
    assert_eq!(top_status_cards_fit(50.0, &widths, 10.0), 1);
}

#[test]
fn top_status_cards_fit_empty_slice_returns_zero() {
    assert_eq!(top_status_cards_fit(1000.0, &[], 10.0), 0);
}

#[test]
fn top_status_cards_fit_spacing_is_accounted() {
    // Two cards of width 100 each. With spacing 50 they need 250 total to both fit.
    let widths = [100.0_f32, 100.0];
    // Budget = 249: only first card fits (100 ≤ 249, but 100+50+100=250 > 249).
    assert_eq!(top_status_cards_fit(249.0, &widths, 50.0), 1);
    // Budget = 250: both fit exactly.
    assert_eq!(top_status_cards_fit(250.0, &widths, 50.0), 2);
}

#[test]
fn top_status_cards_fit_scaled_widths_used_at_higher_scale() {
    // At scale 1.5, min widths grow, so fewer cards fit in the same left_width.
    let scale = "1.5";
    // item_spacing.x scales with layout_scale; source the base from the shared
    // const so this never drifts from `apply_ui_theme`.
    let spacing = ITEM_SPACING_X * 1.5;
    let card = status_card_min_width(scale);
    let wide = status_card_wide_min_width(scale);
    let post = post_indicator_min_width(scale);
    let widths = [card, card, wide, post];
    // A very wide window fits all four.
    assert_eq!(top_status_cards_fit(2000.0, &widths, spacing), 4);
    // A budget that fits only the first two narrow cards.
    let two_budget = card + spacing + card;
    assert_eq!(top_status_cards_fit(two_budget, &widths, spacing), 2);
    // A budget between card and card+spacing+card fits only the first.
    assert_eq!(
        top_status_cards_fit(card + spacing - 1.0, &widths, spacing),
        1
    );
}

#[test]
fn budget_outer_widths_match_real_card_geometry() {
    // The fit budget must use each card's TRUE OUTER width — the inner
    // `set_min_width` value PLUS the Frame's symmetric horizontal margin and
    // both stroke edges — or cards overflow the left region and clip mid-card.
    // Pin the exact relationship the render uses (margin/stroke from the same
    // consts the Frame builders reference) so changing a Frame margin without
    // updating the budget fails here. Checked at scale 1.0 and a scaled value.
    for scale in ["1.0", "1.5"] {
        let card_margin = 2.0 * STATUS_CARD_H_MARGIN + 2.0 * CARD_STROKE;
        assert!(
            (status_card_outer_width(scale) - (status_card_min_width(scale) + card_margin)).abs()
                < 0.001,
            "narrow card outer width drifted from inner+margin+stroke at scale {scale}"
        );
        assert!(
            (status_card_wide_outer_width(scale)
                - (status_card_wide_min_width(scale) + card_margin))
                .abs()
                < 0.001,
            "wide card outer width drifted from inner+margin+stroke at scale {scale}"
        );
        // The pill uses a tighter horizontal margin than the cards.
        let pill_margin = 2.0 * POST_PILL_H_MARGIN + 2.0 * CARD_STROKE;
        assert!(
            (post_indicator_outer_width(scale) - (post_indicator_min_width(scale) + pill_margin))
                .abs()
                < 0.001,
            "post pill outer width drifted from inner+margin+stroke at scale {scale}"
        );
    }
}
