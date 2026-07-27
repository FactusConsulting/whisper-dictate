//! Sibling regression tests for [`super::profile_match`].
//!
//! See `feedback_tests.rs` for the sibling-test rationale.

use super::profile_match::{run_profile_match_self_test, ProfileMatchOptions, ProfileMatchReport};

#[test]
fn crate_public_api_surface_is_reachable_through_module() {
    // Explicit override so the test doesn't touch the operator's real
    // config. Wildcard profile always matches.
    let opts = ProfileMatchOptions {
        title: "Cursor".to_owned(),
        process: "cursor.exe".to_owned(),
        profiles_json_override: r#"[
            {"name":"cursor","match":{"process":"cursor"},"settings":{"lang":"en"}}
        ]"#
        .to_owned(),
    };
    let report: ProfileMatchReport = run_profile_match_self_test(opts);
    assert!(report.matched());
    assert_eq!(report.applied.name.as_deref(), Some("cursor"));
    assert!(report.exit_ok());
}

#[test]
fn no_match_is_not_a_failure() {
    let opts = ProfileMatchOptions {
        title: "Notepad".to_owned(),
        process: "notepad.exe".to_owned(),
        profiles_json_override: "[]".to_owned(),
    };
    let report = run_profile_match_self_test(opts);
    assert!(!report.matched());
    // "no match" is a valid diagnostic answer — exit 0 so the operator
    // can eyeball the JSON.
    assert!(report.exit_ok());
}

#[test]
fn json_envelope_kind_is_stable() {
    let opts = ProfileMatchOptions {
        title: String::new(),
        process: String::new(),
        profiles_json_override: "[]".to_owned(),
    };
    let report = run_profile_match_self_test(opts);
    assert!(report.to_json().contains("\"profile_match_self_test\""));
}
