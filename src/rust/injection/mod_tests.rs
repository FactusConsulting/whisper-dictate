#[cfg(not(feature = "rust-injection"))]
#[test]
fn reduced_build_explains_why_ui_reinjection_is_unavailable() {
    let error = super::reinject_text_for_ui("hello", "type", "", "", "").unwrap_err();
    assert!(error.to_string().contains("rust-injection"));
}
