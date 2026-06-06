use super::*;

#[test]
fn runtime_controls_header_is_tall_enough_for_scaled_topbar() {
    assert!(runtime_controls_header_height("1.15") >= 110.0);
    assert_eq!(runtime_controls_header_height("bad"), 96.0);
    assert_eq!(runtime_controls_header_height("3.0"), 153.6);
}
