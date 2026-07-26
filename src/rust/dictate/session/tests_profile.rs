//! Wire-up tests for the per-utterance target-profile matcher (Python
//! parity port of `_profiled_config` in `vp_dictate._start`). See
//! `src/rust/dictate/profile.rs` for the matcher's own unit tests and
//! `src/rust/platform/foreground_window.rs` for the probe's tests -- the
//! coverage here is deliberately about the SESSION's behaviour when the
//! matcher fires (event emission, config overlay, reset semantics, non-
//! fatal probe failures).

use serde_json::{json, Value};

use super::tests_support::*;
use super::{SessionConfig, UtteranceOutcome};
use crate::dictate::profile::StaticProfileMatcher;
use crate::platform::foreground_window::{FixedForegroundWindow, WindowInfo};

fn matcher_json() -> Value {
    // Two profiles chosen to exercise: a narrow title+process match with
    // BOTH session-owned settings overridden, followed by a wildcard
    // fallback (must not shadow the narrow one).
    json!([
        {
            "name": "Terminal-EN",
            "match": {"title": "Terminal", "process": "WindowsTerminal"},
            "settings": {
                "format_commands": "en",
                "min_record_seconds": "1.25",
                "lang": "en"
            }
        },
        {
            "name": "everything else",
            "match": {},
            "settings": {"format_commands": "da"}
        }
    ])
}

fn probe(title: Option<&str>, process: Option<&str>) -> Box<FixedForegroundWindow> {
    Box::new(FixedForegroundWindow::from_parts(title, process))
}

fn matcher(profiles: Value) -> Box<StaticProfileMatcher> {
    Box::new(StaticProfileMatcher::new(profiles))
}

#[test]
fn session_without_matcher_stays_byte_identical() {
    // A session that never opts into the matcher must NOT emit a
    // `state=profile` line and must NOT mutate its SessionConfig. Pins
    // the "opt-in" contract so every pre-profile test in tests_ported /
    // tests_transitions keeps its exact event trace.
    let transcribe = TestTranscribe::returning_text("hey");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());

    let events = parse_events(&bytes);
    let has_profile = events
        .iter()
        .any(|e| e.get("state").and_then(Value::as_str) == Some("profile"));
    assert!(
        !has_profile,
        "matcher-less sessions must not emit profile events"
    );
}

#[test]
fn matching_profile_overrides_format_command_set_for_the_utterance() {
    // A profile that matches THIS window flips `format_commands` from
    // the base config's None to `"en"`, so the format layer sees the
    // per-app override. The state=profile event carries the matched name
    // so the UI/telemetry can render the swap. Uses a 2-second clip so
    // the parallel `min_record_seconds=1.25` override does not skip it.
    let transcribe = TestTranscribe::returning_text("hello");
    let inject = TestInject::new();
    let config = SessionConfig::default();
    let (mut s, mut buf, _guard) = session_with_config(transcribe, inject, config);
    s = s.with_profile_matcher(
        matcher(matcher_json()),
        probe(Some("Terminal - my repo"), Some("WindowsTerminal.exe")),
    );

    s.start(&mut buf).expect("start");
    let two_seconds = vec![0.0_f32; (super::SR as usize) * 2];
    s.push_frame(&two_seconds);
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "unexpected outcome: {outcome:?}"
    );

    let events = parse_events(&buf);
    let profile_event = events
        .iter()
        .find(|e| e.get("state").and_then(Value::as_str) == Some("profile"))
        .expect("state=profile event must be emitted when a matcher is attached");
    assert_eq!(profile_event["active_profile"], "Terminal-EN");
    assert_eq!(profile_event["target_title"], "Terminal - my repo");
    assert_eq!(profile_event["target_process"], "WindowsTerminal.exe");

    let applied = s
        .active_profile()
        .expect("active_profile must reflect the matched entry");
    assert_eq!(applied.name.as_deref(), Some("Terminal-EN"));
    // The full settings map is exposed for downstream backend wiring
    // (Codex-anticipated follow-up: hot-swap the whisper lang hint).
    assert_eq!(applied.settings["lang"], "en");
}

#[test]
fn matching_profile_overrides_min_record_seconds_for_the_utterance() {
    // With `min_record_seconds=1.25`, a 1-second clip must be SKIPPED
    // (`too_short`) even though the base config's 0.5 s floor would
    // have accepted it. Pins that the numeric override actually reaches
    // the skip helper.
    let transcribe = TestTranscribe::returning_text("hey");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_profile_matcher(
        matcher(matcher_json()),
        probe(Some("Terminal"), Some("WindowsTerminal")),
    );

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(
        matches!(
            outcome,
            UtteranceOutcome::Skipped {
                reason: "too_short"
            }
        ),
        "profile min_record_seconds=1.25 must reject a 1 s clip, got {outcome:?}"
    );
}

#[test]
fn overrides_reset_between_utterances_when_the_next_match_differs() {
    // The overlay is per-utterance: a profile that fires for utterance 1
    // must NOT leak into utterance 2 when the window changed and only
    // the wildcard profile matches. Uses a probe wrapper we can
    // reconfigure between calls so this stays a single test rather than
    // two half-tests.
    struct RelayProbe {
        inner: std::sync::Mutex<WindowInfo>,
    }
    impl crate::platform::foreground_window::ForegroundWindowProbe for RelayProbe {
        fn probe(&self) -> WindowInfo {
            self.inner.lock().expect("probe mutex").clone()
        }
    }
    let relay = std::sync::Arc::new(RelayProbe {
        inner: std::sync::Mutex::new(WindowInfo::new(
            Some("Terminal".to_owned()),
            Some("WindowsTerminal".to_owned()),
        )),
    });
    struct Handle(std::sync::Arc<RelayProbe>);
    impl crate::platform::foreground_window::ForegroundWindowProbe for Handle {
        fn probe(&self) -> WindowInfo {
            self.0.probe()
        }
    }

    let transcribe = TestTranscribe::returning_text("first");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_profile_matcher(
        matcher(matcher_json()),
        Box::new(Handle(std::sync::Arc::clone(&relay))),
    );

    // Utterance 1: narrow profile matches, min_record floor 1.25 s.
    s.start(&mut buf).expect("start 1");
    s.push_frame(&one_second_pcm());
    let outcome_1 = s.stop_and_transcribe(&mut buf).expect("stop 1");
    assert!(matches!(
        outcome_1,
        UtteranceOutcome::Skipped {
            reason: "too_short"
        }
    ));

    // Flip the window so only the wildcard `everything else` profile
    // matches. Its settings do NOT include `min_record_seconds`, so the
    // floor MUST snap back to the base 0.5 s and the same 1 s clip is
    // accepted.
    *relay.inner.lock().unwrap() =
        WindowInfo::new(Some("Editor".to_owned()), Some("code".to_owned()));
    s.start(&mut buf).expect("start 2");
    s.push_frame(&one_second_pcm());
    let outcome_2 = s.stop_and_transcribe(&mut buf).expect("stop 2");
    assert!(matches!(outcome_2, UtteranceOutcome::Injected { .. }));
    // And the wildcard profile IS reflected on the second utterance's
    // active_profile snapshot so the wire event carried the swap.
    assert_eq!(
        s.active_profile().and_then(|p| p.name.as_deref()),
        Some("everything else"),
    );
}

#[test]
fn probe_failure_falls_back_to_default_settings_without_erroring() {
    // A probe that returns an empty WindowInfo (Wayland, macOS, denied
    // FFI, missing xdotool) must still let the session complete the
    // utterance -- the matcher just does not fire. Wildcard profiles CAN
    // still match (empty match block).
    let transcribe = TestTranscribe::returning_text("hi");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let profiles = json!([
        {"name": "narrow", "match": {"title": "Never"}, "settings": {"min_record_seconds": "9"}}
    ]);
    s = s.with_profile_matcher(
        matcher(profiles),
        Box::new(FixedForegroundWindow::default()),
    );

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(
        matches!(outcome, UtteranceOutcome::Injected { .. }),
        "empty probe must not activate the narrow profile"
    );
    assert!(s.active_profile().is_none());
}

#[test]
fn empty_profile_list_is_a_no_op_when_matcher_attached() {
    // Explicit contract: attaching a matcher with an empty profile list
    // is legal. The state=profile event is suppressed when the probe is
    // ALSO empty (else it would spam an empty line on every utterance
    // during test runs). With a probe that DOES return a window, the
    // event still fires with an empty `active_profile` -- that's the
    // "default profile" signal Python prints as
    // `[profile] active: default`.
    let transcribe = TestTranscribe::returning_text("hey");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s = s.with_profile_matcher(
        Box::new(StaticProfileMatcher::empty()),
        probe(Some("Editor"), Some("code.exe")),
    );

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));

    let events = parse_events(&buf);
    let profile_event = events
        .iter()
        .find(|e| e.get("state").and_then(Value::as_str) == Some("profile"))
        .expect("profile event with populated window must still fire");
    // `active_profile` is absent (dropped by wire::emit_status's empty-
    // string filter) when no profile matched -- consumers key on
    // "field absent OR empty" for the "default profile" case.
    assert!(profile_event
        .get("active_profile")
        .and_then(Value::as_str)
        .unwrap_or("")
        .is_empty());
    assert_eq!(profile_event["target_title"], "Editor");
    assert!(s.active_profile().is_none());
}

#[test]
fn unparseable_min_record_seconds_falls_back_to_base() {
    // A profile that carries a bogus numeric string must not crash the
    // session; the base value stays in effect (matches Python's
    // permissive treatment: bad values fall through the coercion and
    // the module default applies). Pins the failure mode so a corrupt
    // config never breaks PTT.
    let transcribe = TestTranscribe::returning_text("hey");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let profiles = json!([
        {"name": "bad", "match": {}, "settings": {"min_record_seconds": "not-a-number"}}
    ]);
    s = s.with_profile_matcher(
        matcher(profiles),
        probe(Some("Anything"), Some("anything.exe")),
    );

    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    // Base 0.5 s min_record → the 1 s clip is accepted.
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
}

// ── platform-specific coverage ────────────────────────────────────────────
//
// The per-OS probe backends live behind `#[cfg]` guards so their contract
// tests do too. The Wayland branch is exercised by an env-driven test that
// forces the Linux backend down the WAYLAND_DISPLAY short-circuit; the
// Windows + macOS branches are exercised through their default probe on
// the matching target (a contract-level "never panics, returns something"
// check -- assertion on real windows in CI is not portable).

#[cfg(target_os = "linux")]
#[test]
fn linux_probe_returns_empty_under_pure_wayland_env() {
    use crate::platform::foreground_window::{ForegroundWindowProbe, SystemForegroundWindow};
    let _guard = crate::test_env_lock::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev_display = std::env::var_os("DISPLAY");
    let prev_wayland = std::env::var_os("WAYLAND_DISPLAY");
    std::env::remove_var("DISPLAY");
    std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
    let info = SystemForegroundWindow.probe();
    // Restore the ambient env FIRST so an assertion failure still cleans
    // up (`_guard` alone would leak the mutated vars past the test).
    match prev_display {
        Some(v) => std::env::set_var("DISPLAY", v),
        None => std::env::remove_var("DISPLAY"),
    }
    match prev_wayland {
        Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
        None => std::env::remove_var("WAYLAND_DISPLAY"),
    }
    assert!(
        info.is_empty(),
        "pure Wayland must short-circuit the Linux probe to an empty WindowInfo"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_probe_contract_holds_without_a_visible_window() {
    // On CI there may be no foreground window; the probe must still be
    // safe to call and return either an empty snapshot or one whose
    // strings are trimmed and non-empty. The FFI itself is exercised
    // more directly by `platform::foreground_window::imp::tests`.
    use crate::platform::foreground_window::{ForegroundWindowProbe, SystemForegroundWindow};
    let info = SystemForegroundWindow.probe();
    if let Some(title) = info.title.as_deref() {
        assert_eq!(title.trim(), title);
    }
    if let Some(process) = info.process.as_deref() {
        assert_eq!(process.trim(), process);
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
#[test]
fn other_targets_probe_returns_empty() {
    // macOS + any other non-Linux, non-Windows target is a no-op
    // (matches Python, which has no `_capture_target_window` branch on
    // those platforms).
    use crate::platform::foreground_window::{ForegroundWindowProbe, SystemForegroundWindow};
    assert!(SystemForegroundWindow.probe().is_empty());
}
