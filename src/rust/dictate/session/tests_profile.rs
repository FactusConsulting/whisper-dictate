//! Wire-up tests for the per-utterance target-profile matcher (Python
//! parity port of `_profiled_config` in `vp_dictate._start`). See
//! `src/rust/dictate/profile.rs` for the matcher's own unit tests and
//! `src/rust/platform/foreground_window.rs` for the probe's tests -- the
//! coverage here is deliberately about the SESSION's behaviour when the
//! matcher fires (event emission, config overlay, reset semantics, non-
//! fatal probe failures).

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use super::tests_support::*;
use super::{InjectBackend, InjectError, SessionConfig, UtteranceOutcome};
use crate::dictate::profile::StaticProfileMatcher;
use crate::platform::foreground_window::{FixedForegroundWindow, WindowInfo};

struct TargetRecordingInject {
    prepared: Arc<Mutex<Vec<Option<WindowInfo>>>>,
}

impl InjectBackend for TargetRecordingInject {
    fn inject(&self, _text: &str) -> Result<(), InjectError> {
        Ok(())
    }

    fn prepare_target(&self, window: Option<&WindowInfo>) -> Result<(), InjectError> {
        self.prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(window.cloned());
        Ok(())
    }
}

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
                "lang": "en",
                "inject_mode": "paste"
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
fn injection_prepares_the_target_captured_at_recording_start() {
    let prepared = Arc::new(Mutex::new(Vec::new()));
    let inject = TargetRecordingInject {
        prepared: Arc::clone(&prepared),
    };
    let (mut session, mut output, _guard) = session_with_config(
        TestTranscribe::returning_text("hello"),
        inject,
        SessionConfig::default(),
    );
    let captured = WindowInfo::new(Some("Terminal".to_owned()), Some("terminal.exe".to_owned()))
        .with_target_id(Some("42".to_owned()));
    session = session.with_profile_matcher(
        matcher(json!([])),
        Box::new(FixedForegroundWindow::new(captured.clone())),
    );
    session.start(&mut output).expect("start");
    session.push_frame(&one_second_pcm());
    session
        .stop_and_transcribe(&mut output)
        .expect("stop and transcribe");

    assert_eq!(
        prepared.lock().unwrap().as_slice(),
        &[Some(captured)],
        "the inject backend must receive the target snapshot before typing"
    );
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
        Box::new(FixedForegroundWindow::new(
            WindowInfo::new(
                Some("Terminal - my repo".to_owned()),
                Some("WindowsTerminal.exe".to_owned()),
            )
            .with_target_id(Some("42".to_owned())),
        )),
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
    assert_eq!(profile_event["target_id"], "42");

    let utterance_event = events
        .iter()
        .find(|e| e.get("event").and_then(Value::as_str) == Some("utterance"))
        .expect("the accepted utterance must include its captured target");
    assert_eq!(utterance_event["target_id"], "42");
    assert_eq!(utterance_event["inject_mode"], "paste");

    let applied = s
        .active_profile()
        .expect("active_profile must reflect the matched entry");
    assert_eq!(applied.name.as_deref(), Some("Terminal-EN"));
    // The full settings map is exposed for downstream backend wiring
    // (-anticipated follow-up: hot-swap the whisper lang hint).
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

// ── backend override coverage (#607) ────────────────────────────────

/// Recording backends that log every `apply_profile_overrides` call so a test
/// can assert the session forwarded the profile settings to the backend hooks.
/// Kept local (rather than in `tests_support.rs`) because only these tests
/// need to observe the override side effect.
mod backend_override_coverage {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::super::tests_support::*;
    use super::super::{SessionConfig, UtteranceOutcome};
    use crate::dictate::profile::StaticProfileMatcher;
    use crate::dictate::session::{
        DictateSession, InjectBackend, InjectError, PostProcessBackend, PostProcessOutcome,
        TranscribeBackend, TranscribeError, TranscribeResult,
    };
    use crate::platform::foreground_window::FixedForegroundWindow;
    use serde_json::json;

    struct SnoopTranscribe {
        seen: RefCell<Vec<BTreeMap<String, String>>>,
    }

    impl SnoopTranscribe {
        fn new() -> Self {
            Self {
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl TranscribeBackend for SnoopTranscribe {
        fn transcribe(
            &self,
            _pcm: &[f32],
            _sample_rate: u32,
        ) -> Result<TranscribeResult, TranscribeError> {
            Ok(TranscribeResult {
                dictionary_terms: None,
                text: "hello".to_owned(),
                raw_text: "hello".to_owned(),
                is_hallucination: false,
                latency_ms: 1,
                duration_s: 1.0,
                language: String::new(),
                language_probability: 0.0,
                gate: None,
                stt_impl: String::new(),
                stt_accel: String::new(),
            })
        }

        fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
            self.seen.borrow_mut().push(settings.clone());
        }
    }

    struct SnoopInject {
        seen: RefCell<Vec<BTreeMap<String, String>>>,
    }

    impl SnoopInject {
        fn new() -> Self {
            Self {
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl InjectBackend for SnoopInject {
        fn inject(&self, _text: &str) -> Result<(), InjectError> {
            Ok(())
        }

        fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
            self.seen.borrow_mut().push(settings.clone());
        }
    }

    struct SnoopPost {
        seen: std::sync::Mutex<Vec<BTreeMap<String, String>>>,
    }

    impl SnoopPost {
        fn new() -> Self {
            Self {
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl PostProcessBackend for SnoopPost {
        fn post_process(&self, text: &str, _lang: &str) -> PostProcessOutcome {
            PostProcessOutcome {
                text: text.to_owned(),
                processor: "mock".to_owned(),
                mode: "clean".to_owned(),
                model: "mock".to_owned(),
                latency_ms: 0,
                changed: false,
                fallback: false,
                error: String::new(),
                redacted: false,
                redactions: Vec::new(),
            }
        }

        fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
            self.seen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(settings.clone());
        }
    }

    #[test]
    fn profile_overrides_reach_all_three_backends_each_utterance() {
        // #607: a profile with `initial_prompt`, `inject_mode`,
        // and `post_processor` keys must reach the whisper/inject/post
        // backends respectively on the next utterance. Uses snooping
        // backends that only record the settings they received; the
        // production impls each own the interior-mutability slot that
        // consumes those settings.
        let guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

        let transcribe = SnoopTranscribe::new();
        let inject = SnoopInject::new();
        let post = std::sync::Arc::new(SnoopPost::new());
        let post_for_backend = std::sync::Arc::clone(&post);
        struct PostAdapter(std::sync::Arc<SnoopPost>);
        impl PostProcessBackend for PostAdapter {
            fn post_process(&self, text: &str, lang: &str) -> PostProcessOutcome {
                self.0.post_process(text, lang)
            }
            fn apply_profile_overrides(&self, settings: &BTreeMap<String, String>) {
                self.0.apply_profile_overrides(settings)
            }
            fn is_active(&self) -> bool {
                self.0.is_active()
            }
        }

        let mut s = DictateSession::new(transcribe, inject, SessionConfig::default())
            .with_post_process(Box::new(PostAdapter(post_for_backend)))
            .with_profile_matcher(
                Box::new(StaticProfileMatcher::new(json!([
                    {
                        "name": "code editor",
                        "match": {"process": "code"},
                        "settings": {
                            "initial_prompt": "Rust, Cargo, clippy",
                            "language": "en",
                            "inject_mode": "print",
                            "post_processor": "ollama",
                            "post_mode": "clean"
                        }
                    }
                ]))),
                Box::new(FixedForegroundWindow::from_parts(
                    Some("main.rs — code"),
                    Some("Code.exe"),
                )),
            );

        let mut buf = Vec::new();
        s.start(&mut buf).expect("start");
        s.push_frame(&one_second_pcm());
        let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
        assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));

        // Each backend must have received the FULL settings map on the
        // apply-profile step (the session forwards all keys, not just
        // the ones it consumes itself).
        let transcribe_seen = s.transcribe_backend().seen.borrow();
        let inject_seen = s.inject_backend().seen.borrow();
        let post_seen = post.seen.lock().unwrap_or_else(|p| p.into_inner());

        assert_eq!(
            transcribe_seen.len(),
            1,
            "transcribe backend must see one apply_profile_overrides call per utterance"
        );
        assert_eq!(
            transcribe_seen[0].get("initial_prompt").map(String::as_str),
            Some("Rust, Cargo, clippy")
        );
        assert_eq!(
            transcribe_seen[0].get("language").map(String::as_str),
            Some("en")
        );

        assert_eq!(inject_seen.len(), 1);
        assert_eq!(
            inject_seen[0].get("inject_mode").map(String::as_str),
            Some("print")
        );

        assert_eq!(post_seen.len(), 1);
        assert_eq!(
            post_seen[0].get("post_processor").map(String::as_str),
            Some("ollama")
        );

        drop(guard);
    }

    #[test]
    fn non_matching_profile_still_forwards_empty_map_to_reset_overrides() {
        // Reset semantics: when the matcher returns no profile the session
        // still calls apply_profile_overrides with an EMPTY map so the
        // backend can drop any per-utterance override it stashed for a
        // previous match. Without this a profile that fired for utterance N
        // would silently persist into N+1 (#607).
        let guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var("VOICEPI_WORKER_EVENTS", "1");

        let transcribe = SnoopTranscribe::new();
        let inject = SnoopInject::new();
        // A profile with a narrow match block that WON'T fire for the
        // probe below -- so the matcher returns AppliedProfile::none().
        let mut s = DictateSession::new(transcribe, inject, SessionConfig::default())
            .with_profile_matcher(
                Box::new(StaticProfileMatcher::new(json!([
                    {"name": "never", "match": {"process": "no-such-app"}, "settings": {"initial_prompt": "X"}}
                ]))),
                Box::new(FixedForegroundWindow::from_parts(Some("Editor"), Some("code"))),
            );

        let mut buf = Vec::new();
        s.start(&mut buf).expect("start");
        s.push_frame(&one_second_pcm());
        s.stop_and_transcribe(&mut buf).expect("stop");
        assert!(s.active_profile().is_none());
        assert_eq!(s.transcribe_backend().seen.borrow().len(), 1);
        assert!(
            s.transcribe_backend().seen.borrow()[0].is_empty(),
            "non-matching profile must forward an empty settings map so backends can RESET"
        );

        drop(guard);
    }
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
