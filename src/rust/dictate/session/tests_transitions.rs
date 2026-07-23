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
    // Redaction provenance is always present when a pass ran (empty list
    // when nothing was redacted), matching vp_dictate.py's
    // `post_redactions or []`.
    assert_eq!(utterance["post_redacted"], false);
    assert_eq!(utterance["post_redactions"], serde_json::json!([]));
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
fn dictionary_replacements_rewrite_the_transcript_before_injection() {
    // The attached dictionary's deterministic replacement table rewrites the
    // decoded text before it is injected -- Python's `_dictionary_runtime`.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "hello".to_owned(),
            to: "hi".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    match outcome {
        UtteranceOutcome::Injected { text, .. } => assert_eq!(text, "hi world"),
        other => panic!("expected Injected, got {other:?}"),
    }
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hi world".to_owned()]
    );
}

#[test]
fn dictionary_replacements_apply_before_format_commands() {
    // Order fidelity: `dictionary -> format`. The dictionary rewrites
    // "linebreak" -> "new line"; with format_command_set = `en` that spoken
    // command then becomes "\n". A final "alpha\nbeta" proves the dictionary
    // ran BEFORE the format-command layer (Python's order).
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("alpha linebreak beta");
    let inject = TestInject::new();
    let config = SessionConfig {
        format_command_set: Some("en".to_owned()),
        ..SessionConfig::default()
    };
    let (s, _, _guard) = session_with_config(transcribe, inject, config);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "linebreak".to_owned(),
            to: "new line".to_owned(),
        }],
    });
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["alpha\nbeta".to_owned()],
        "dictionary replacement must apply before the format-command layer",
    );
}

#[test]
fn no_dictionary_leaves_the_transcript_unchanged() {
    // Regression guard: a session without a dictionary injects the raw
    // transcript, byte-identical to before the seam existed.
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hello world".to_owned()]
    );
}

#[test]
fn dictionary_replacement_can_rescue_a_blacklisted_transcript() {
    // Ordering fidelity (Codex P2): STT returns a blacklist phrase ("tak")
    // flagged as a hallucination; a replacement "tak" -> "thanks" is applied
    // BEFORE classification, so the corrected text is re-classified as normal
    // dictation and injected -- Python runs `_dictionary_runtime` before the
    // hallucination check.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_hallucination("tak");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "tak".to_owned(),
            to: "thanks".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    match outcome {
        UtteranceOutcome::Injected { text, .. } => assert_eq!(text, "thanks"),
        other => panic!("expected Injected (rescued by replacement), got {other:?}"),
    }
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["thanks".to_owned()]
    );
}

#[test]
fn dictionary_replacement_into_a_blacklist_phrase_is_dropped() {
    // The reverse: a replacement rewrites normal dictation INTO a blacklist
    // phrase; re-classification then drops it as no_speech (nothing injected).
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("cheers");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "cheers".to_owned(),
            to: "tak".to_owned(),
        }],
    });
    let (outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
    assert!(
        matches!(
            outcome,
            UtteranceOutcome::NoText {
                reason: "no_speech"
            }
        ),
        "{outcome:?}"
    );
    assert!(s.inject_backend().injected.borrow().is_empty());
}

#[test]
fn utterance_event_carries_dictionary_replacements() {
    // Metadata (Codex P2): every replacement that fires is recorded on the
    // utterance event as `dictionary_replacements` ({from,to,count}), which
    // `ui/log_render.rs` counts and telemetry/history keep.
    use crate::dictionary::{Dictionary, Replacement};
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject);
    let s = s.with_dictionary(Dictionary {
        terms: Vec::new(),
        replacements: vec![Replacement {
            from: "hello".to_owned(),
            to: "hi".to_owned(),
        }],
    });
    let (_outcome, bytes, _s) = run_one_utterance(s, &one_second_pcm());
    let utterance = parse_events(&bytes)
        .into_iter()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("utterance"))
        .expect("an utterance event must be emitted");
    let reps = utterance
        .get("dictionary_replacements")
        .and_then(|v| v.as_array())
        .expect("dictionary_replacements array present when a replacement fired");
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0]["from"], "hello");
    assert_eq!(reps[0]["to"], "hi");
    assert_eq!(reps[0]["count"], 1);
}

#[test]
fn with_optional_dictionary_attaches_only_when_replacements_exist() {
    // The production seam `with_optional_dictionary` mirrors the inline guard
    // every real call site (simulate-session, make_real_session) used to
    // repeat: a SessionDictionary carrying replacements is attached and
    // rewrites the transcript; an empty one is a no-op (byte-identical to a
    // session built without a dictionary).
    use crate::dictionary::{Dictionary, Replacement, SessionDictionary};

    // Each case scopes its own `session()` (and thus its `ENV_LOCK` guard) so
    // the guard drops before the next `session()` call -- `ENV_LOCK` is a plain
    // non-reentrant mutex, so holding both guards at once would deadlock.

    // Replacements present -> attached -> transcript rewritten.
    {
        let with = SessionDictionary {
            dictionary: Dictionary {
                terms: Vec::new(),
                replacements: vec![Replacement {
                    from: "hello".to_owned(),
                    to: "hi".to_owned(),
                }],
            },
            max_terms: 80,
            max_chars: 1200,
            enabled: true,
        };
        let (s, _, _guard) = session(
            TestTranscribe::returning_text("hello world"),
            TestInject::new(),
        );
        let s = s.with_optional_dictionary(with);
        let (_outcome, _bytes, s) = run_one_utterance(s, &one_second_pcm());
        assert_eq!(
            s.inject_backend().injected.borrow().as_slice(),
            ["hi world".to_owned()]
        );
    }

    // No replacements -> not attached -> raw transcript passes through.
    {
        let without = SessionDictionary {
            dictionary: Dictionary::default(),
            max_terms: 80,
            max_chars: 1200,
            enabled: false,
        };
        let (s2, _, _guard2) = session(
            TestTranscribe::returning_text("hello world"),
            TestInject::new(),
        );
        let s2 = s2.with_optional_dictionary(without);
        let (_outcome2, _bytes2, s2) = run_one_utterance(s2, &one_second_pcm());
        assert_eq!(
            s2.inject_backend().injected.borrow().as_slice(),
            ["hello world".to_owned()]
        );
    }
}

#[test]
fn session_live_reloads_the_dictionary_between_utterances() {
    // End-to-end live reload: a session built with `with_reloading_dictionary`
    // re-reads the dictionary file at each utterance boundary, so editing the
    // replacement table between two utterances changes the injected text with
    // no app restart -- Python's per-utterance `_dictionary_runtime`. The
    // reload resolves config-first, so the dictionary path comes from a temp
    // config.json (via `VOICEPI_CONFIG`); an `EnvVarSnapshot` restores it on
    // drop even if an assertion panics.
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (s, _, _guard) = session(transcribe, inject); // holds ENV_LOCK
    let _env = EnvVarSnapshot::new(&["VOICEPI_CONFIG"]);

    let dir = tempfile::tempdir().unwrap();
    let dict = dir.path().join("dict.json");
    std::fs::write(&dict, r#"{"replacements":{"hello":"hi"}}"#).unwrap();
    let cfg = crate::config::AppSettings {
        dictionary: dict.display().to_string(),
        dictionary_enabled: true,
        ..Default::default()
    };
    crate::config::save_settings_to_path(&cfg, dir.path().join("config.json")).unwrap();
    std::env::set_var("VOICEPI_CONFIG", dir.path().join("config.json"));

    let s = s.with_reloading_dictionary(crate::dictionary::ReloadPrecedence::ConfigFirst);

    // Utterance 1 rewrites hello -> hi.
    let (_o1, _b1, s) = run_one_utterance(s, &one_second_pcm());
    // Edit the file to a different byte length (hello -> HELLO) so the size
    // component of the freshness stamp flips the cache key deterministically.
    std::fs::write(&dict, r#"{"replacements":{"hello":"HELLO"}}"#).unwrap();
    // Utterance 2 must pick up the edit and rewrite hello -> HELLO.
    let (_o2, _b2, s) = run_one_utterance(s, &one_second_pcm());

    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hi world".to_owned(), "HELLO world".to_owned()],
        "the second utterance must reflect the live-edited dictionary"
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
fn session_reusable_across_consecutive_ptt_cycles() {
    // Regression guard for the "PTT only worked the first time, then got
    // stuck" class of bug seen when the engine was first flipped to Rust
    // (reverted in 1.21.0): a single session must serve press after press,
    // not arm exactly once. We drive the SAME `DictateSession` through
    // three full start → push → stop_and_transcribe cycles and assert every
    // cycle:
    //   * arms cleanly (`start` returns Ok — a stuck session would refuse
    //     the 2nd press with `AlreadyActive`),
    //   * settles back to `Idle` afterwards (so the next press can arm),
    //   * actually injects the transcript (the user-visible "it typed
    //     something" — the epoch counter alone can advance while the output
    //     path is dead).
    let transcribe = TestTranscribe::returning_text("hello world");
    let inject = TestInject::new();
    let (mut s, mut buf, _guard) = session(transcribe, inject);

    for cycle in 1..=3 {
        assert!(
            matches!(s.state(), SessionState::Idle),
            "cycle {cycle}: session must be Idle before the press, got {:?}",
            s.state()
        );
        s.start(&mut buf)
            .unwrap_or_else(|e| panic!("cycle {cycle}: press must arm the session, got {e:?}"));
        s.push_frame(&one_second_pcm());
        let outcome = s
            .stop_and_transcribe(&mut buf)
            .unwrap_or_else(|e| panic!("cycle {cycle}: release must transcribe, got {e:?}"));
        match outcome {
            UtteranceOutcome::Injected { text, .. } => {
                assert_eq!(text, "hello world", "cycle {cycle}: wrong injected text");
            }
            other => panic!("cycle {cycle}: expected Injected, got {other:?}"),
        }
        assert!(
            matches!(s.state(), SessionState::Idle),
            "cycle {cycle}: session must return to Idle after release, got {:?}",
            s.state()
        );
    }

    // Every cycle reached the injector — not just the first. This is the
    // assertion the "stuck after one press" bug would trip on.
    assert_eq!(
        s.inject_backend().injected.borrow().as_slice(),
        ["hello world", "hello world", "hello world"],
        "each of the three presses must inject its transcript"
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
