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
use super::{SessionConfig, SessionError, SessionState, UtteranceOutcome};

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
fn format_commands_off_by_default_injects_raw_text() {
    // Default config leaves `format_command_set` = None, so spoken
    // command words like "comma" stay literal and the raw transcript is
    // what gets injected -- byte-identical to the pre-format-wiring
    // behaviour.
    let transcribe = TestTranscribe::returning_text("write comma literally");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["write comma literally".to_owned()],
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["text"], "write comma literally");
}

#[test]
fn format_commands_applied_between_transcribe_and_inject() {
    // With `format_command_set = Some("en")` the session applies the
    // deterministic spoken-command dictionary before injection, exactly
    // like Python's `formatting.apply_format_commands` step. The
    // injected text AND the emitted utterance event both carry the
    // formatted result (what was actually typed), not the raw transcript.
    let transcribe = TestTranscribe::returning_text("first item comma new line second item period");
    let inject = TestInject::new();
    let config = SessionConfig {
        format_command_set: Some("en".to_owned()),
        ..SessionConfig::default()
    };
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["first item,\nsecond item.".to_owned()],
        "the format-command dictionary must be applied before inject",
    );
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["text"], "first item,\nsecond item.");
    // `text_chars` is derived from the emitted (formatted) text.
    assert_eq!(
        utterance["text_chars"],
        "first item,\nsecond item.".chars().count()
    );
}

#[test]
fn format_commands_explicit_off_is_passthrough() {
    // An explicit `Some("off")` normalises to a passthrough just like
    // `None` -- the transcript is injected verbatim.
    let transcribe = TestTranscribe::returning_text("new line stays literal");
    let inject = TestInject::new();
    let config = SessionConfig {
        format_command_set: Some("off".to_owned()),
        ..SessionConfig::default()
    };
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["new line stays literal".to_owned()],
    );
}

#[test]
fn no_post_processor_skips_pass_and_status() {
    // The default session has no post-processor: the transcript is
    // injected as-is and NO `post-processing` status is emitted, so the
    // status sequence is byte-identical to before the seam existed.
    let transcribe = TestTranscribe::returning_text("plain transcript");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["plain transcript".to_owned()],
    );
    assert!(
        !state_trace(&bytes).iter().any(|s| s == "post-processing"),
        "no post-processor must not emit a post-processing status: {:?}",
        state_trace(&bytes),
    );
}

#[test]
fn post_processor_rewrites_text_and_emits_status() {
    // With a post-processor attached, the rewritten text is what gets
    // injected, and a `post-processing` status fires between
    // `transcribing` and the utterance event (matching Python's
    // WorkerStatus.PostProcessing phase).
    let transcribe = TestTranscribe::returning_text("um raw transcript uh");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_post_process(Box::new(TestPostProcess::returning("clean transcript")));
    let (outcome, bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert!(matches!(outcome, UtteranceOutcome::Injected { .. }));
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["clean transcript".to_owned()],
        "the post-processed text must be what is injected",
    );
    let trace = state_trace(&bytes);
    let tr = trace.iter().position(|s| s == "transcribing");
    let pp = trace.iter().position(|s| s == "post-processing");
    assert!(
        matches!((tr, pp), (Some(t), Some(p)) if t < p),
        "expected transcribing before post-processing, got {trace:?}"
    );
    // The utterance event carries the rewritten text AND the post_*
    // metadata so the UI/telemetry report post-processing ran (Codex #2 /
    // vp_dictate.py:469-475).
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert_eq!(utterance["text"], "clean transcript");
    assert_eq!(utterance["post_processor"], "ollama");
    assert_eq!(utterance["post_mode"], "clean");
    assert_eq!(utterance["post_model"], "test-model");
    assert_eq!(utterance["post_latency_ms"], 12);
    assert_eq!(utterance["post_changed"], true);
    assert_eq!(utterance["post_fallback"], false);
    // No error -> the field is dropped, matching Python's `error or None`.
    assert!(utterance.get("post_error").is_none());
}

#[test]
fn no_post_processor_omits_post_fields() {
    // Without a post-processor the utterance event carries NO post_*
    // fields, so `log_render::post_processing_summary` reads "off".
    let transcribe = TestTranscribe::returning_text("plain");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("utterance event");
    assert!(utterance.get("post_processor").is_none());
    assert!(utterance.get("post_mode").is_none());
}

#[test]
fn post_process_runs_before_format_commands() {
    // Order fidelity: `postprocess -> format -> inject`. The
    // post-processor emits a transcript CONTAINING a spoken format
    // command; with format_command_set = `en` that command must then be
    // applied to the post-processor's OUTPUT (proving postprocess ran
    // first), yielding the final injected text.
    let transcribe = TestTranscribe::returning_text("noisy input");
    let inject = TestInject::new();
    let config = SessionConfig {
        format_command_set: Some("en".to_owned()),
        ..SessionConfig::default()
    };
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let s = s.with_post_process(Box::new(TestPostProcess::returning("done new line here")));
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());

    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["done\nhere".to_owned()],
        "format commands must apply to the post-processor's output \
         (proves postprocess ran before format)",
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
