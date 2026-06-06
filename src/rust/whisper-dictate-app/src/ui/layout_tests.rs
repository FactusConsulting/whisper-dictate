use super::*;

#[test]
fn shell_chrome_dimensions_scale_with_ui_text() {
    assert!(top_status_bar_height("1.15") >= 73.0);
    assert_eq!(top_status_bar_height("bad"), 64.0);
    assert_eq!(top_status_bar_height("3.0"), 102.4);
    assert!(sidebar_width("1.15") >= 188.0);
    assert_eq!(sidebar_width("bad"), 164.0);
    assert!((sidebar_width("3.0") - 262.4).abs() < 0.001);
}
