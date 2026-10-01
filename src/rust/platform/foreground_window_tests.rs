use super::imp::run_xdotool;
use std::time::{Duration, Instant};

#[test]
fn foreground_probe_does_not_wait_for_descendant_pipe_eof() {
    crate::test_xdotool::with_script("sleep 2 &\necho active", || {
        let started = Instant::now();
        assert_eq!(run_xdotool(&["getactivewindow"]).unwrap().trim(), "active");
        assert!(started.elapsed() < Duration::from_millis(1500));
    });
}

#[test]
fn foreground_probe_rejects_truncated_window_identity() {
    crate::test_xdotool::with_script("head -c 131072 /dev/zero", || {
        assert!(run_xdotool(&["getactivewindow"]).is_none());
    });
}
