use super::*;
use crate::ui::tabs::top_status_left_width;

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
    let controls = 232.0_f32;

    // Normal window: left side gets the surplus.
    assert!((top_status_left_width(1000.0, controls) - 768.0).abs() < 0.001);

    // Exactly fitting: left side gets zero, no overlap.
    assert!((top_status_left_width(controls, controls)).abs() < 0.001);

    // Narrower than controls (edge case): left side is floored at zero,
    // never negative — the cards simply clip rather than forcing an overlap.
    assert_eq!(top_status_left_width(100.0, controls), 0.0);
    assert_eq!(top_status_left_width(0.0, controls), 0.0);
}
