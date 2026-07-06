//! Supplementary state-transition invariants for [`super::DictateSession`].
//!
//! These tests pin the small-but-load-bearing properties that the six
//! ported `tests_ported.rs` tests don't directly cover — they're the
//! kind of invariant a refactor can quietly break, so we keep them
//! explicit:
//!
//! - frames pushed outside Recording are dropped;
//! - `start()` while active is an error (epoch NOT bumped, state
//!   unchanged);
//! - `stop_and_transcribe()` while idle is a silent no-op;
//! - transcribe-error → no_text/no_speech;
//! - empty transcribe result → no_text/empty;
//! - the opening → recording transition shape is exact;
//! - cancel-while-idle is a silent no-op;
//! - inject failure still emits the utterance event (Python parity);
//! - epochs increase monotonically across start() calls.

use super::tests_support::*;
use super::{SessionError, SessionState, UtteranceOutcome};

#[test]
fn push_frame_while_idle_is_dropped() {
    // Frames pushed outside Recording are discarded — matches the
    // Python capture mixin's `if self.recording` ingestion gate.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.push_frame(&one_second_pcm());
    // Now actually run a recording with no real frames pushed — the
    // session must see an empty buffer (NoAudio outcome).
    s.start(&mut buf).expect("start");
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NoAudio);
}

#[test]
fn start_while_active_is_an_error() {
    // Python's `_start` early-returns silently; the Rust port returns
    // `AlreadyActive` so a buggy caller can't accidentally skip a
    // recording. The state must NOT change and the epoch must NOT bump.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let first = s.start(&mut buf).expect("start#1");
    let err = s.start(&mut buf).expect_err("nested start must error");
    assert!(
        matches!(err, SessionError::AlreadyActive { .. }),
        "expected AlreadyActive, got {err:?}"
    );
    assert_eq!(s.epoch(), first, "epoch must NOT bump on a refused start");
    assert!(matches!(s.state(), SessionState::Recording { .. }));
}

#[test]
fn stop_while_idle_is_a_noop() {
    // `_stop_and_transcribe` early-returns on `not self.recording` in
    // Python; the Rust port surfaces that as `NotRecording` with no
    // events emitted.
    let transcribe = TestTranscribe::returning_text("never");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let outcome = s.stop_and_transcribe(&mut buf).expect("stop");
    assert_eq!(outcome, UtteranceOutcome::NotRecording);
    assert!(buf.is_empty(), "stop-when-idle must not emit any events");
}

#[test]
fn transcribe_error_emits_no_text_no_speech() {
    // Python `_transcribe_pcm` wraps any model exception and surfaces
    // it as `reason="no_speech"` on the no-text event.
    let transcribe = TestTranscribe::returning_error("model panicked");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(
        outcome,
        UtteranceOutcome::NoText {
            reason: "no_speech"
        }
    ));
    assert!(s.inject_backend().injected.borrow().is_empty());
    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "no_speech");
}

#[test]
fn empty_transcribe_result_emits_no_text_empty() {
    // Mirrors the Python branch that maps an empty `result.text` to
    // `reason="empty"` (the `is_hallucination` flag is irrelevant on an
    // empty text, so the no_text reason is `empty` rather than `no_speech`).
    let transcribe = TestTranscribe::returning_empty();
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(
        outcome,
        UtteranceOutcome::NoText { reason: "empty" }
    ));
    let no_text: Vec<_> = parse_events(&bytes)
        .into_iter()
        .filter(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .collect();
    assert_eq!(no_text.len(), 1);
    assert_eq!(no_text[0]["reason"], "empty");
}

#[test]
fn start_emits_opening_then_recording_in_order() {
    // The supervisor / UI relies on the opening → recording transition
    // shape; the test pins it so a refactor that flips or skips a state
    // is caught here.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    let trace = state_trace(&buf);
    assert_eq!(
        &trace[..2],
        &["opening".to_owned(), "recording".to_owned()],
        "expected opening → recording prefix, got {trace:?}"
    );
}

#[test]
fn stale_cancel_while_idle_is_a_noop() {
    // A cancel that arrives when the session is idle (no recording in
    // flight) must do nothing, emit nothing — Python's
    // `if not self.recording: return`.
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.cancel(0, &mut buf).expect("cancel-while-idle");
    s.cancel(42, &mut buf)
        .expect("cancel-while-idle (bogus epoch)");
    assert!(buf.is_empty(), "cancel-while-idle must not emit any events");
    assert_eq!(s.state(), SessionState::Idle);
    assert_eq!(s.epoch(), 0);
}

#[test]
fn inject_failure_still_emits_utterance() {
    // Python's `_inject` logs and the utterance event still fires (the
    // user sees the text was decoded, just not pasted). The Rust port
    // matches that and surfaces the inject failure on the utterance
    // event so the supervisor can drive a "couldn't paste" UI without
    // re-parsing logs.
    let transcribe = TestTranscribe::returning_text("hello there");
    let inject = TestInject::failing("clipboard busy");
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert!(
        s.inject_backend().injected.borrow().is_empty(),
        "the failing inject must NOT record success"
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event must still fire on inject failure");
    assert_eq!(
        utterance["inject_error"],
        "inject backend error: clipboard busy"
    );
}

#[test]
fn epoch_bumps_monotonically_per_start() {
    let transcribe = TestTranscribe::returning_text("noop");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    let a = s.start(&mut buf).expect("start#1");
    s.stop_and_transcribe(&mut buf).expect("stop#1");
    let b = s.start(&mut buf).expect("start#2");
    s.stop_and_transcribe(&mut buf).expect("stop#2");
    let c = s.start(&mut buf).expect("start#3");
    assert!(
        a < b && b < c,
        "epochs must be monotonically increasing: {a},{b},{c}"
    );
}
#[test]
fn utterance_event_carries_recording_s() {
    // Codex P2 #413 wire.rs:61 (round 2). Successful utterance events
    // must carry `recording_s` -- it''s the clip duration that
    // log_render.rs / telemetry.rs read out of every utterance.
    let transcribe = TestTranscribe::returning_text("hello");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    s.stop_and_transcribe(&mut buf).expect("stop");

    let utterance = parse_events(&buf)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("an utterance event must be emitted");
    let rec = utterance
        .get("recording_s")
        .and_then(|v| v.as_f64())
        .expect("recording_s must be present on utterance events");
    assert!(
        (rec - 1.0).abs() < 0.01,
        "recording_s ({rec}) must be ~1.0 for a one-second-pcm utterance",
    );
}

#[test]
fn transcribing_state_fires_even_on_no_audio() {
    // Codex P2 #413 mod.rs:233 (round 2). Python emits
    // `state="transcribing"` BEFORE the empty-frames guard so the UI
    // sequence is `recording -> transcribing -> no_text -> ready` even
    // on a no-audio recording. Without this the Rust path would jump
    // straight from `recording` to `no_text`.
    let transcribe = TestTranscribe::returning_text("never runs");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    s.stop_and_transcribe(&mut buf).expect("stop");

    let trace = state_trace(&buf);
    let transcribing_idx = trace
        .iter()
        .position(|s| s == "transcribing")
        .expect("transcribing state must fire even on no-audio");
    let no_text_idx = trace.iter().position(|s| s == "no_text").expect("no_text");
    assert!(
        transcribing_idx < no_text_idx,
        "transcribing must precede no_text: trace was {trace:?}",
    );
}

#[test]
fn gate_text_normalises_to_reason_token() {
    // Codex P2 #413 mod.rs:284 (round 2). The Rust speech gate returns
    // free-form messages like "input too quiet: -42 dBFS" which Python
    // maps to "too_quiet"; the session must surface the reason token,
    // not the raw gate text.
    use crate::dictate::session::normalize_gate_reason;
    assert_eq!(
        normalize_gate_reason("input too quiet: -42 dBFS"),
        "too_quiet"
    );
    assert_eq!(normalize_gate_reason("Input Too Quiet"), "too_quiet"); // case-insensitive
    assert_eq!(
        normalize_gate_reason("no speech contrast: 0.02"),
        "no_speech"
    );
    assert_eq!(normalize_gate_reason("NO SPEECH detected"), "no_speech");
    assert_eq!(normalize_gate_reason(""), "empty");
    assert_eq!(normalize_gate_reason("something unrelated"), "empty");
}

#[test]
fn empty_text_with_gate_emits_normalised_reason() {
    // Wire-level lock for the previous test: an empty-text result whose
    // `gate` field carries the production "too quiet" message must
    // surface as `reason="too_quiet"` on the no_text event.
    use super::TranscribeResult;
    let transcribe = TestTranscribe::returning_text("");
    {
        let mut next = transcribe.next.borrow_mut();
        *next = super::tests_support::TranscribeOutcome::Ok(TranscribeResult {
            text: String::new(),
            gate: Some("input too quiet: -42 dBFS".to_owned()),
            ..Default::default()
        });
    }
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    s.stop_and_transcribe(&mut buf).expect("stop");

    let no_text = parse_events(&buf)
        .into_iter()
        .find(|e| e.get("state").and_then(|s| s.as_str()) == Some("no_text"))
        .expect("a no_text event must be emitted");
    assert_eq!(no_text["reason"], "too_quiet");
}
#[test]
fn worker_event_escapes_del_control_character() {
    // Codex P2 #413 wire.rs:146 (round 3). The ASCII escape helper
    // must treat DEL (U+007F) like any other non-ASCII control byte
    // and emit `\u007f`, matching Python `json.dumps(ensure_ascii=True)`
    // and PR 1's `events::AsciiFormatter`. Without this branch, a
    // device name / dictated text / error message carrying DEL lands as
    // a raw control byte and breaks consumers on non-UTF-8 shells.
    use super::TranscribeResult;
    let transcribe = TestTranscribe::returning_text("");
    {
        let mut next = transcribe.next.borrow_mut();
        *next = super::tests_support::TranscribeOutcome::Ok(TranscribeResult {
            text: String::new(),
            // DEL embedded in the gate text -- exercises the escape path
            // through the no_text emit.
            gate: Some("input too quiet:\u{7f}detail".to_owned()),
            ..Default::default()
        });
    }
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);
    s.start(&mut buf).expect("start");
    s.push_frame(&one_second_pcm());
    s.stop_and_transcribe(&mut buf).expect("stop");

    // The raw bytes MUST NOT contain a literal 0x7f byte -- it should be
    // serialised as the four-character escape `\u007f` (six bytes once
    // the leading backslash is counted: `\`, `u`, `0`, `0`, `7`, `f`).
    assert!(
        !buf.contains(&0x7f),
        "raw DEL (0x7f) leaked into worker-event stream: {:?}",
        std::str::from_utf8(&buf).unwrap_or("<non-utf8>"),
    );
}

// ── Wave 5.5 gap #2 + #3: postprocess + format-command wiring ───────────────
//
// The session's `run_transcription` weaves post-processing then format
// commands between the transcribe backend and the inject backend so a
// user with `post_processor != "none"` or `format_commands != "off"`
// no longer falls back to Python for the whole utterance pipeline.

use super::tests_support::session_with_config;
use super::types::SessionConfig;
use crate::postprocess::PostprocessSettings;

fn postprocess_off_settings() -> PostprocessSettings {
    PostprocessSettings {
        processor: "none".to_owned(),
        mode: "raw".to_owned(),
        model: "qwen2.5:3b".to_owned(),
        base_url: "http://localhost:11434".to_owned(),
        timeout_ms: 100,
        max_input_chars: 4000,
        max_output_chars: 4000,
        api_key: String::new(),
        redact: false,
        redact_terms: String::new(),
        local_only: false,
    }
}

#[test]
fn postprocess_disabled_omits_post_fields_but_still_reports_format_off() {
    // Default SessionConfig has `postprocess_settings: None` +
    // `format_commands: "off"`. Post-processing was never invoked so
    // the utterance event omits every `post_*` field; format commands
    // DID "run" (as a no-op path through `apply_format_commands`) so
    // the `format_commands_*` fields DO land, carrying `enabled=false`
    // + `command_set="off"` -- this matches Python's
    // `_utterance_event`, which always includes the format-command
    // provenance in the payload.
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["hello world".to_owned()]
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    // Post fields absent because no postprocess settings configured --
    // the session never touched the pipeline.
    for field in [
        "post_processor",
        "post_mode",
        "post_latency_ms",
        "post_changed",
        "post_fallback",
        "postprocess_error",
        "raw_text",
    ] {
        assert!(
            utterance.get(field).is_none(),
            "field {field:?} must be omitted when postprocess is not configured; \
             got: {utterance:?}"
        );
    }
    // Format fields present with "off" state -- Python parity.
    assert_eq!(utterance["format_commands_enabled"], false);
    assert_eq!(utterance["format_commands_set"], "off");
    assert_eq!(utterance["format_commands_changed"], false);
}

#[test]
fn postprocess_processor_none_passes_through_but_surfaces_provenance() {
    // A `Some(settings)` with `processor="none"` short-circuits the
    // pipeline inside `postprocess_text` but the session still surfaces
    // the provenance fields on the utterance event so downstream
    // consumers can tell "session offered post" apart from "session
    // never even invoked post".
    let config = SessionConfig {
        postprocess_settings: Some(postprocess_off_settings()),
        ..SessionConfig::default()
    };

    let transcribe = TestTranscribe::returning_text("keep this raw");
    let inject = TestInject::new();
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["keep this raw".to_owned()],
        "processor=none must inject the original transcribed text verbatim"
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["post_processor"], "none");
    assert_eq!(utterance["post_mode"], "raw");
    assert_eq!(utterance["post_changed"], false);
    assert_eq!(utterance["post_fallback"], false);
    // `postprocess_error` MUST be absent -- the pass-through is not an
    // error condition, and Python's `_postprocess_and_format` only sets
    // an error message on genuine transport / validation failures.
    assert!(utterance.get("postprocess_error").is_none());
    // `raw_text` is only emitted when postprocess/format CHANGED the
    // text; on this pass-through the final text matches the raw so the
    // field stays absent (byte-saving parity with Python).
    assert!(utterance.get("raw_text").is_none());
}

#[test]
fn postprocess_failure_falls_back_to_original_and_surfaces_error() {
    // A configured `processor="ollama"` pointed at an unreachable URL
    // must NOT abort injection -- Python `_postprocess_and_format` logs
    // and continues with the original text. The Rust port must match:
    // inject still fires with the raw transcribed text AND the
    // `postprocess_error` field lands on the utterance event so the UI
    // can drive a "post fallback" indicator.
    let mut settings = postprocess_off_settings();
    settings.processor = "ollama".to_owned();
    settings.mode = "clean".to_owned();
    settings.base_url = "http://127.0.0.1:1".to_owned();
    let config = SessionConfig {
        postprocess_settings: Some(settings),
        ..SessionConfig::default()
    };

    let transcribe = TestTranscribe::returning_text("dont drop this");
    let inject = TestInject::new();
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["dont drop this".to_owned()],
        "postprocess failure MUST inject the original transcribed text"
    );

    let events = parse_events(&bytes);
    let utterance = events
        .iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["post_fallback"], true);
    assert!(
        utterance
            .get("postprocess_error")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .is_some(),
        "postprocess_error MUST carry the transport error"
    );

    // Python also fires a `state="post-processing"` status card before
    // the call when the provider will actually run -- pin it so the
    // live pipeline UI sequence stays in lock-step with Python.
    let trace = state_trace(&bytes);
    assert!(
        trace.iter().any(|s| s == "post-processing"),
        "expected `post-processing` in state trace, got {trace:?}"
    );
}

#[test]
fn format_commands_transform_text_between_postprocess_and_inject() {
    // Format commands must run AFTER postprocess so a user-said "period"
    // survives an LLM cleanup pass and still becomes a real `.`. Here
    // postprocess is off (default), so we can pin the format-only path.
    let config = SessionConfig {
        format_commands: "en".to_owned(),
        ..SessionConfig::default()
    };

    let transcribe = TestTranscribe::returning_text("first item comma second period");
    let inject = TestInject::new();
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["first item, second.".to_owned()]
    );

    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["format_commands_enabled"], true);
    assert_eq!(utterance["format_commands_set"], "en");
    assert_eq!(utterance["format_commands_changed"], true);
    // The raw (pre-format) text must survive on the event so consumers
    // can diff raw vs. final text without re-running the model.
    assert_eq!(utterance["raw_text"], "first item comma second period");
    assert_eq!(utterance["text"], "first item, second.");
}

#[test]
fn format_off_does_not_touch_text() {
    // `format_commands: "off"` (the default) must leave the text
    // untouched even when the transcript LOOKS like a command sequence.
    let transcribe = TestTranscribe::returning_text("first item comma second period");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_, _, s) = run_one_utterance(s, &one_second_pcm());
    assert_eq!(
        *s.inject_backend().injected.borrow(),
        vec!["first item comma second period".to_owned()]
    );
}
