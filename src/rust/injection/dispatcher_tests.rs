use super::Injector;

#[test]
fn cancelled_modifier_release_does_not_initialize_a_native_backend() {
    let mut injector = Injector::new();
    assert!(injector
        .release_held_modifiers_cancellable(&[0x11], &|| false)
        .unwrap_err()
        .to_string()
        .contains("injection cancelled"));
    assert!(injector.backend.is_none());
}
