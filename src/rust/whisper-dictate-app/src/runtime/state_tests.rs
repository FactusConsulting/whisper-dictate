use super::*;

#[test]
fn runtime_state_labels_are_stable() {
    assert_eq!(RuntimeState::Stopped.label(), "Stopped");
    assert_eq!(RuntimeState::Starting.label(), "Starting");
    assert_eq!(RuntimeState::Running.label(), "Running");
}
