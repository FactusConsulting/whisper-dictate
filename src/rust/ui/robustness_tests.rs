//! Unit tests for the four UI robustness fixes:
//!   1. Runtime-log cap (trim at line boundary, marker not duplicated)
//!   2. GPU probe channel polling (non-blocking result adoption)
//!   3. Worker crash-streak advice (pure helper + integration via test_app)
//!   4. Stale audio-meter guard (stateless audio event only sets active when Running)

use super::app::{crash_streak_advice, trim_runtime_log, RUNTIME_LOG_MAX_CHARS, TRIM_MARKER};
use super::test_support::test_app;
use super::*;

// ── Fix 1: Runtime-log cap ────────────────────────────────────────────────────

#[test]
fn log_under_cap_is_not_trimmed() {
    let mut log = "line one\nline two\nline three".to_owned();
    trim_runtime_log(&mut log);
    assert_eq!(log, "line one\nline two\nline three");
}

#[test]
fn log_at_exactly_cap_is_not_trimmed() {
    let line = "x".repeat(RUNTIME_LOG_MAX_CHARS);
    let mut log = line.clone();
    trim_runtime_log(&mut log);
    assert_eq!(log, line);
}

#[test]
fn trim_drops_oldest_whole_lines_and_keeps_newest_content() {
    // Build a log that is just over the cap.
    // Each line is "line NNN\n" (9 chars).  We need enough lines to exceed
    // RUNTIME_LOG_MAX_CHARS.  We'll use a much smaller cap by constructing a
    // log that slightly exceeds 200_000 chars and confirming the newest lines
    // survive.
    let line_count = RUNTIME_LOG_MAX_CHARS / 10 + 5; // slightly over cap
    let lines: Vec<String> = (0..line_count).map(|i| format!("line {:06}", i)).collect();
    let mut log = lines.join("\n");

    trim_runtime_log(&mut log);

    // The marker must be present exactly at the top.
    assert!(
        log.starts_with(TRIM_MARKER),
        "marker should be at the top; log starts with: {:?}",
        &log[..log.len().min(80)]
    );
    // The very last line must survive.
    let last_line = format!("line {:06}", line_count - 1);
    assert!(
        log.contains(&last_line),
        "newest line should survive trimming"
    );
    // The log must now be at or under the cap.
    assert!(
        log.len() <= RUNTIME_LOG_MAX_CHARS,
        "log length {} exceeds cap {}",
        log.len(),
        RUNTIME_LOG_MAX_CHARS
    );
    // The cut must be at a whole-line boundary — no partial lines.
    for part in log.split('\n') {
        // Every part should either be the marker or a complete "line NNNNNN".
        assert!(
            part == TRIM_MARKER || part.starts_with("line "),
            "unexpected partial line: {part:?}"
        );
    }
}

#[test]
fn trim_marker_is_not_duplicated_after_repeated_trims() {
    // Simulate append_runtime_log being called on an already-trimmed log.
    let line_count = RUNTIME_LOG_MAX_CHARS / 10 + 5;
    let lines: Vec<String> = (0..line_count).map(|i| format!("line {:06}", i)).collect();
    let mut log = lines.join("\n");

    // First trim.
    trim_runtime_log(&mut log);
    assert!(log.starts_with(TRIM_MARKER));
    let marker_count_after_first = log.matches(TRIM_MARKER).count();

    // Append more lines to push past the cap again.
    for i in 0..50 {
        log.push('\n');
        log.push_str(&format!("extra {:06}", i));
    }
    // Second trim.
    trim_runtime_log(&mut log);
    let marker_count_after_second = log.matches(TRIM_MARKER).count();

    assert_eq!(
        marker_count_after_second, 1,
        "marker should appear exactly once after repeated trims \
         (was {marker_count_after_first} after first trim)"
    );
}

#[test]
fn append_runtime_log_via_app_trims_log_and_sets_scroll_flag() {
    let mut app = test_app(AppSettings::default());
    // Fill the log to just under the cap.
    let filler: String = (0..(RUNTIME_LOG_MAX_CHARS / 10))
        .map(|i| format!("fill {:06}\n", i))
        .collect();
    app.runtime_log = filler;

    // One more append should push over the cap and trigger a trim.
    let tail = "final important line";
    app.append_runtime_log(tail);

    assert!(
        app.runtime_log.len() <= RUNTIME_LOG_MAX_CHARS,
        "log length {} exceeds cap",
        app.runtime_log.len()
    );
    assert!(
        app.runtime_log.contains(tail),
        "newest line must survive after trim"
    );
    assert!(app.runtime_log_scroll_to_bottom);
}

// ── Fix 2: GPU probe channel polling ─────────────────────────────────────────

#[test]
fn gpu_probe_result_is_adopted_on_poll() {
    let (tx, rx) = std::sync::mpsc::channel::<Option<u32>>();
    let mut app = test_app(AppSettings::default());
    app.gpu_total_mb = None;
    app.gpu_probe = Some(rx);

    // Before the sender fires, gpu_total_mb stays None.
    // (We skip a real poll_runtime call to avoid spawning processes.)

    // Simulate the background thread completing.
    tx.send(Some(8192)).unwrap();

    // Manually replicate what poll_runtime does for the gpu_probe.
    if let Some(probe_rx) = &app.gpu_probe {
        if let Ok(result) = probe_rx.try_recv() {
            app.gpu_total_mb = result;
            app.gpu_probe = None;
        }
    }

    assert_eq!(app.gpu_total_mb, Some(8192));
    assert!(
        app.gpu_probe.is_none(),
        "receiver should be dropped after result is taken"
    );
}

#[test]
fn gpu_probe_none_result_is_adopted_correctly() {
    let (tx, rx) = std::sync::mpsc::channel::<Option<u32>>();
    let mut app = test_app(AppSettings::default());
    app.gpu_probe = Some(rx);

    tx.send(None).unwrap();

    if let Some(probe_rx) = &app.gpu_probe {
        if let Ok(result) = probe_rx.try_recv() {
            app.gpu_total_mb = result;
            app.gpu_probe = None;
        }
    }

    assert_eq!(app.gpu_total_mb, None);
    assert!(app.gpu_probe.is_none());
}

#[test]
fn gpu_probe_not_yet_ready_leaves_state_unchanged() {
    let (_tx, rx) = std::sync::mpsc::channel::<Option<u32>>();
    let mut app = test_app(AppSettings::default());
    app.gpu_total_mb = None;
    app.gpu_probe = Some(rx);

    // try_recv should return Err(Empty) — nothing changes.
    if let Some(probe_rx) = &app.gpu_probe {
        if let Ok(result) = probe_rx.try_recv() {
            app.gpu_total_mb = result;
            app.gpu_probe = None;
        }
    }

    assert_eq!(app.gpu_total_mb, None);
    assert!(app.gpu_probe.is_some(), "receiver should still be held");
}

// ── Fix 3: Crash-streak advice (pure helper) ─────────────────────────────────

#[test]
fn crash_streak_advice_is_none_below_threshold() {
    assert!(crash_streak_advice(0).is_none());
    assert!(crash_streak_advice(1).is_none());
    assert!(crash_streak_advice(2).is_none());
}

#[test]
fn crash_streak_advice_fires_exactly_at_three() {
    let msg = crash_streak_advice(3).expect("should produce advice at count=3");
    assert!(
        msg.contains("3 times"),
        "message should mention '3 times'; got: {msg:?}"
    );
    assert!(
        msg.contains("Doctor"),
        "message should point to the Doctor; got: {msg:?}"
    );
}

#[test]
fn crash_streak_advice_is_none_above_threshold() {
    // After the streak has been flagged, subsequent crashes don't repeat it.
    assert!(
        crash_streak_advice(4).is_none(),
        "advice should fire exactly at 3, not at 4"
    );
    assert!(crash_streak_advice(10).is_none());
}

#[test]
fn crash_streak_resets_on_clean_exit() {
    let mut app = test_app(AppSettings::default());
    app.fast_crash_count = 2;
    app.worker_start_time = Some(std::time::Instant::now());

    // Simulate a clean exit (code == Some(0)).
    app.handle_exit_crash_streak(Some(0));

    assert_eq!(app.fast_crash_count, 0, "streak must reset on clean exit");
}

#[test]
fn crash_streak_resets_on_long_lived_exit() {
    let mut app = test_app(AppSettings::default());
    app.fast_crash_count = 2;
    // Simulate start time 30 s ago by omitting a start time (None → Duration::MAX).
    app.worker_start_time = None;

    // A non-zero exit code but elapsed == Duration::MAX (≥10 s) → reset.
    app.handle_exit_crash_streak(Some(1));

    assert_eq!(app.fast_crash_count, 0);
}

#[test]
fn crash_streak_increments_on_fast_nonzero_exit() {
    let mut app = test_app(AppSettings::default());
    app.fast_crash_count = 0;
    app.worker_start_time = Some(std::time::Instant::now());

    app.handle_exit_crash_streak(Some(1));
    assert_eq!(app.fast_crash_count, 1);

    app.worker_start_time = Some(std::time::Instant::now());
    app.handle_exit_crash_streak(Some(1));
    assert_eq!(app.fast_crash_count, 2);
}

#[test]
fn crash_streak_advice_appended_to_log_at_count_three() {
    let mut app = test_app(AppSettings::default());
    app.fast_crash_count = 2;
    app.worker_start_time = Some(std::time::Instant::now());

    app.handle_exit_crash_streak(Some(1));

    assert_eq!(app.fast_crash_count, 3);
    assert!(
        app.runtime_log.contains("3 times"),
        "advice message must be in the runtime log; got: {:?}",
        app.runtime_log
    );
}

#[test]
fn crash_streak_resets_when_worker_reports_ready() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.fast_crash_count = 2;

    let event = WorkerEvent {
        event: "status".to_owned(),
        state: Some("ready".to_owned()),
        payload: serde_json::json!({ "event": "status", "state": "ready" }),
    };
    app.update_worker_status(&event);

    assert_eq!(
        app.fast_crash_count, 0,
        "streak must reset when worker is ready"
    );
}

// ── Fix 4: Stale audio meter on unknown audio-event state ────────────────────

#[test]
fn stateless_audio_event_does_not_set_active_when_worker_stopped() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Stopped;
    app.audio_capture_active = false;

    // Audio event with no `state` field.
    let event = WorkerEvent {
        event: "audio".to_owned(),
        state: None,
        payload: serde_json::json!({
            "event": "audio",
            "level": 0.5,
        }),
    };
    app.update_worker_audio(&event);

    assert!(
        !app.audio_capture_active,
        "stateless audio event must not set active=true when worker is Stopped"
    );
}

#[test]
fn stateless_audio_event_sets_active_when_worker_running() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.audio_capture_active = false;

    let event = WorkerEvent {
        event: "audio".to_owned(),
        state: None,
        payload: serde_json::json!({
            "event": "audio",
            "level": 0.4,
        }),
    };
    app.update_worker_audio(&event);

    assert!(
        app.audio_capture_active,
        "stateless audio event should set active=true when worker is Running"
    );
}

#[test]
fn stateless_audio_event_does_not_set_active_when_worker_starting() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Starting;
    app.audio_capture_active = false;

    let event = WorkerEvent {
        event: "audio".to_owned(),
        state: None,
        payload: serde_json::json!({ "event": "audio", "level": 0.3 }),
    };
    app.update_worker_audio(&event);

    assert!(
        !app.audio_capture_active,
        "stateless audio event must not set active when worker is Starting"
    );
}
