use super::tabs::{
    audio_device_label, full_audio_device_label, live_audio_level_summary, mic_label_char_budget,
};
use super::test_support::{test_app, EnvVarGuard, ENV_TEST_LOCK};
use super::*;

#[test]
fn log_view_modes_round_trip_persisted_config_ids() {
    for mode in LogViewMode::ALL {
        assert_eq!(LogViewMode::from_raw(mode.id()), mode);
    }
    assert_eq!(LogViewMode::from_raw("full"), LogViewMode::Minimal);
    assert_eq!(LogViewMode::from_raw(""), LogViewMode::Minimal);
}

#[test]
fn log_view_modes_filter_runtime_output_by_detail_level() {
    let log = [
        "Rust UI ready. Start launches the Python dictation worker directly.",
        "[ui] started: python worker",
        "[worker] status=listening",
        "[gate] raw=-30dBFS noise=-80dBFS snr=54dB",
        "[cap] raw=-30dBFS peak=0.359 gain=2.8x noise=-80dBFS snr=54dB",
        "[stt] dur=12.8s post-boost=-21dBFS compute=0.5s rtf=0.04 text='hej'",
        "[stt-debug] segment#0 avg_logprob=-0.1",
        "[post] clean/groq changed in 450ms",
        "[inject] -> \"hej\"  (target: editor)",
        "[inject] strategy: type",
        "third-party library noise",
    ]
    .join("\n");

    let minimal = log_view_text(&log, LogViewMode::Minimal);
    assert_eq!(minimal, "hej");
    assert!(!minimal.contains("[ui] started"));
    assert!(!minimal.contains("[post] clean/groq"));
    assert!(!minimal.contains("[inject]"));
    assert!(!minimal.contains("[inject] strategy"));
    assert!(!minimal.contains("[gate] raw="));
    assert!(!minimal.contains("third-party library noise"));

    let diagnostic = log_view_text(&log, LogViewMode::Diagnostic);
    assert!(diagnostic.contains("[post] clean/groq"));
    assert!(diagnostic.contains("[inject] -> \"hej\""));
    assert!(diagnostic.contains("[inject] strategy"));
    assert!(diagnostic.contains("[gate] raw="));
    assert!(diagnostic.contains("[cap] raw="));
    assert!(diagnostic.contains("[stt] dur="));
    assert!(diagnostic.contains("[stt-debug] segment#0"));
    assert!(!diagnostic.contains("third-party library noise"));

    let debug = log_view_text(&log, LogViewMode::Debug);
    assert!(debug.contains("third-party library noise"));
}

#[test]
fn minimal_log_cards_prioritize_final_output() {
    let log = [
        "[worker] status=listening",
        "[post] clean/groq changed in 450ms",
        "[inject] -> \"Hej, Sara.\"  (target: Word)",
        "[inject] strategy: type",
    ]
    .join("\n");

    let cards = runtime_log_cards(&log, LogViewMode::Minimal);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, RuntimeLogCardKind::FinalText);
    assert_eq!(cards[0].title, "Hej, Sara.");
    assert_eq!(cards[0].detail, "");
    assert_eq!(cards[0].badge, "Final");
}

#[test]
fn minimal_log_cards_ignore_blank_inject_preview() {
    let log = [
        "[inject] -> \"\"  (target: whisper-dictate 1.0.0)",
        "[inject] -> \"   \"  (target: whisper-dictate 1.0.0)",
        "[inject] skipped self-target",
    ]
    .join("\n");

    assert!(runtime_log_cards(&log, LogViewMode::Minimal).is_empty());
    assert_eq!(log_view_text(&log, LogViewMode::Minimal), "");
}

#[test]
fn minimal_copy_text_contains_only_final_injected_text() {
    let log = [
        "[worker] status=listening",
        "[stt] dur=1.0s text='midlertidig tekst'",
        "[post] clean/groq changed in 120ms",
        "[inject] -> \"First final.\"  (target: Word)",
        "[inject] strategy: type",
        "[post] skipped clean/none",
        "[inject] -> \"Second final.\"  (target: Word)",
    ]
    .join("\n");

    assert_eq!(
        log_view_text(&log, LogViewMode::Minimal),
        "First final.\nSecond final."
    );
}

#[test]
fn diagnostic_log_cards_summarize_audio_and_stt_metrics() {
    let log = [
        "[gate] raw=-30dBFS noise=-80dBFS snr=54dB input=good",
        "[cap] raw=-30dBFS peak=0.359 input=good gain=2.8x noise=-80dBFS snr=54dB",
        "[stt] dur=12.8s post-boost=-21dBFS compute=0.5s rtf=0.04 text='hej'",
        "[post] clean/groq changed in 450ms",
        "[inject] -> \"hej\"  (target: editor)",
    ]
    .join("\n");

    let cards = runtime_log_cards(&log, LogViewMode::Diagnostic);
    assert!(cards.iter().any(|card| {
        card.kind == RuntimeLogCardKind::FinalText
            && card.title == "hej"
            && card.detail == "clean/groq changed in 450ms"
    }));
    assert!(cards.iter().any(|card| {
        card.kind == RuntimeLogCardKind::Diagnostic
            && card.badge == "Capture"
            && card.title.contains("peak=0.359")
            && card.title.contains("input=good")
    }));
    assert!(cards.iter().any(|card| {
        card.kind == RuntimeLogCardKind::Diagnostic
            && card.badge == "STT"
            && card.title.contains("dur=12.8s")
            && card.title.contains("rtf=0.04")
    }));
}

#[test]
fn minimal_cards_and_copy_use_full_utterance_text_over_truncated_inject() {
    // The [inject] log line truncates at ~57 chars at the source; the
    // [utterance] event carries the full text. Cards and Copy must show the
    // full sentence (and not duplicate the truncated inject preview).
    // Includes a quoted word so the fixture exercises real-world JSON escaping
    // (built via serde_json so the log line is always valid JSON).
    let full = "Okay, det vil sige, at den burde tage og skrive \"helt\" ud, hvad \
                det er, jeg dikterer, imens jeg dikterer det, hele vejen til punktum.";
    let payload = serde_json::json!({
        "event": "utterance",
        "text": full,
        "text_preview": "Okay, det vil sige, at den burde tage og skrive...",
        "recording_s": 12.0,
    });
    let log = [
        r#"[inject] -> "Okay, det vil sige, at den burde tage og skrive..."  (target: Word)"#
            .to_owned(),
        format!("[utterance] {payload}"),
    ]
    .join("\n");

    let cards = runtime_log_cards(&log, LogViewMode::Minimal);
    assert_eq!(cards.len(), 1, "no truncated duplicate card: {cards:?}");
    assert_eq!(cards[0].kind, RuntimeLogCardKind::FinalText);
    assert_eq!(cards[0].title, full);
    assert!(!cards[0].title.ends_with("..."));
    // No post fields in the payload → the card states post-processing was off.
    assert_eq!(cards[0].detail, "Post-processing off");

    // Copy (the Minimal view text) hands out the full sentence too.
    assert_eq!(log_view_text(&log, LogViewMode::Minimal), full);

    // Diagnostic's utterance card also carries the full text now.
    let diag = runtime_log_cards(&log, LogViewMode::Diagnostic);
    let utterance = diag.iter().find(|c| c.badge == "Utterance").unwrap();
    assert_eq!(utterance.title, full);
}

#[test]
fn final_card_shows_post_processing_mode_per_utterance() {
    // Active post-processing → mode + provider under the Final card.
    let active = serde_json::json!({
        "event": "utterance", "text": "renset tekst",
        "post_processor": "groq", "post_mode": "clean",
    });
    let cards = runtime_log_cards(&format!("[utterance] {active}"), LogViewMode::Minimal);
    assert_eq!(cards[0].detail, "Post-processing: clean (groq)");

    // Raw mode counts as off even with a provider configured.
    let raw = serde_json::json!({
        "event": "utterance", "text": "rå tekst",
        "post_processor": "groq", "post_mode": "raw",
    });
    let cards = runtime_log_cards(&format!("[utterance] {raw}"), LogViewMode::Minimal);
    assert_eq!(cards[0].detail, "Post-processing off");

    // Provider present but mode missing (older/partial payloads): the worker
    // default is raw, so the card must read off — never "?".
    let no_mode = serde_json::json!({
        "event": "utterance", "text": "tekst",
        "post_processor": "groq",
    });
    let cards = runtime_log_cards(&format!("[utterance] {no_mode}"), LogViewMode::Minimal);
    assert_eq!(cards[0].detail, "Post-processing off");
}

#[test]
fn diagnostic_log_cards_prefer_structured_utterance_events() {
    let log = [
        "[stt] dur=12.8s post-boost=-21dBFS compute=0.5s rtf=0.04 text='mellemtekst'",
        "[inject] -> \"Hej, mit navn er Sara.\"  (target: Word)",
        r#"[utterance] {"event":"utterance","text":"Hej, mit navn er Sara.","text_preview":"Hej, mit navn er Sara.","recording_s":12.8,"audio_raw_dbfs":-33.2,"audio_peak":0.282,"audio_noise_dbfs":-78.0,"audio_snr_db":49.0,"audio_gain":3.5,"post_boost_dbfs":-22.0,"compute_s":0.5,"real_time_factor":0.04,"stt_backend":"openai","model":"whisper-large-v3-turbo","device":"api","dictionary_terms":["Sara"],"dictionary_replacements":[{"from":"Lars datter","to":"Lars' datter","count":1}],"post_processor":"groq","post_mode":"clean","post_model":"llama-3.3-70b-versatile","post_latency_ms":450,"post_changed":true,"post_fallback":false,"inject_strategy":"type","target_title":"Microsoft Word"}"#,
    ]
    .join("\n");

    let cards = runtime_log_cards(&log, LogViewMode::Diagnostic);

    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, RuntimeLogCardKind::Diagnostic);
    assert_eq!(cards[0].badge, "Utterance");
    assert_eq!(cards[0].title, "Hej, mit navn er Sara.");
    assert!(cards[0].detail.contains("recording=12.8s"));
    assert!(cards[0].detail.contains("raw=-33dBFS"));
    assert!(cards[0].detail.contains("peak=0.282"));
    assert!(cards[0].detail.contains("snr=49dB"));
    assert!(cards[0].detail.contains("gain=3.5x"));
    assert!(cards[0].detail.contains("compute=0.5s"));
    assert!(cards[0].detail.contains("rtf=0.04"));
    assert!(cards[0].detail.contains("backend=openai"));
    assert!(cards[0]
        .detail
        .contains("dictionary terms=1 replacements=1"));
    assert!(cards[0].detail.contains("provider=groq"));
    assert!(cards[0]
        .detail
        .contains("post_model=llama-3.3-70b-versatile"));
    assert!(cards[0].detail.contains("changed=true"));
    assert!(cards[0].detail.contains("fallback=false"));
    assert!(cards[0].detail.contains("inject=type"));
}

#[test]
fn worker_utterance_events_are_logged_as_structured_debug_lines() {
    let event = WorkerEvent {
        event: "utterance".to_owned(),
        state: None,
        payload: serde_json::json!({
            "event": "utterance",
            "text_preview": "Når æøå virker",
            "audio_raw_dbfs": -34.0,
            "post_processor": "groq",
        }),
    };

    let line = worker_utterance_log_line(&event).expect("utterance log line");

    assert!(line.starts_with("[utterance] "));
    assert!(line.contains(r#""text_preview":"Når æøå virker""#));
    assert!(line.contains(r#""audio_raw_dbfs":-34.0"#));
    assert_eq!(log_view_text(&line, LogViewMode::Minimal), "Når æøå virker");
    assert!(log_view_text(&line, LogViewMode::Debug).contains("[utterance]"));
}

#[test]
fn audio_meter_level_uses_live_worker_level_only_while_recording() {
    assert_eq!(audio_meter_level(0.42, RuntimeState::Stopped, true), 0.0);
    assert_eq!(audio_meter_level(0.42, RuntimeState::Running, false), 0.0);
    assert_eq!(audio_meter_level(0.42, RuntimeState::Running, true), 0.42);
    assert_eq!(audio_meter_level(2.0, RuntimeState::Running, true), 1.0);
    assert_eq!(audio_meter_level(-1.0, RuntimeState::Running, true), 0.0);
}

#[test]
fn live_audio_summary_reports_only_active_capture_levels() {
    assert_eq!(
        live_audio_level_summary(Some(-33.25), Some(0.282), true),
        "raw=-33.2dBFS  peak=0.282"
    );
    assert_eq!(
        live_audio_level_summary(Some(-47.0), None, true),
        "raw=-47.0dBFS"
    );
    assert_eq!(
        live_audio_level_summary(None, Some(0.4), true),
        "Waiting for audio level"
    );
    assert_eq!(
        live_audio_level_summary(Some(-33.25), Some(0.282), false),
        "Not recording"
    );
}

#[test]
fn audio_device_labels_fit_compact_meter_space() {
    assert_eq!(audio_device_label("", 20), "Input pending");
    assert_eq!(
        audio_device_label("  Microphone (Yeti Classic)  ", 34),
        "Microphone (Yeti Classic)"
    );
    assert_eq!(
        audio_device_label("Analogue 1 + 2 (Focusrite USB Audio)", 18),
        "Analogue 1 + 2 (Fo..."
    );
    assert_eq!(
        audio_device_label("Analogue 1 + 2 (Focusrite USB Audio)", 4),
        "Analogue..."
    );
    assert_eq!(full_audio_device_label(""), "Not reported yet");
    assert_eq!(
        full_audio_device_label("  Microphone (Yeti Classic)  "),
        "Microphone (Yeti Classic)"
    );
}

#[test]
fn mic_label_budget_is_bounded_for_narrow_and_wide_layouts() {
    assert_eq!(mic_label_char_budget(0.0), 8);
    assert_eq!(mic_label_char_budget(70.0), 10);
    assert_eq!(mic_label_char_budget(1000.0), 34);
}

#[test]
fn worker_status_updates_capture_state_and_logs_audio_device() {
    assert_eq!(
        audio_capture_active_for_worker_state("recording"),
        Some(true)
    );
    assert_eq!(
        audio_capture_active_for_worker_state("listening"),
        Some(true)
    );
    assert_eq!(
        audio_capture_active_for_worker_state("opening"),
        Some(false)
    );
    assert_eq!(
        audio_capture_active_for_worker_state("transcribing"),
        Some(false)
    );
    assert_eq!(audio_capture_active_for_worker_state("ready"), Some(false));

    let event = WorkerEvent {
        event: "status".to_owned(),
        state: Some("recording".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "recording",
            "capture_backend": "sounddevice",
            "capture_channels": 2,
            "audio_device": "USB Microphone",
            "startup_ms": 42,
            "first_audio": "ok",
        }),
    };
    assert_eq!(
        worker_status_log_line(&event),
        Some(
            "[worker] status=recording capture_backend=sounddevice capture_channels=2 audio_device=USB Microphone startup_ms=42 first_audio=ok"
                .to_owned()
        )
    );
}

#[test]
fn opening_worker_status_marks_meter_as_arming_not_recording() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.audio_capture_active = true;
    app.audio_meter_level = 0.5;

    let event = WorkerEvent {
        event: "status".to_owned(),
        state: Some("opening".to_owned()),
        payload: serde_json::json!({
            "event": "status",
            "state": "opening",
        }),
    };

    app.update_worker_status(&event);

    assert!(app.audio_capture_opening);
    assert!(!app.audio_capture_active);
    assert_eq!(app.audio_meter_level, 0.0);
}

#[test]
fn worker_audio_event_updates_live_meter_and_device_without_log_output() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    app.runtime_log = "[cap] raw=-10dBFS peak=0.999 gain=1.0x noise=-80dBFS snr=70dB".to_owned();

    let event = WorkerEvent {
        event: "audio".to_owned(),
        state: Some("recording".to_owned()),
        payload: serde_json::json!({
            "event": "audio",
            "state": "recording",
            "level": 0.31,
            "raw_dbfs": -35.5,
            "peak": 0.12,
            "audio_device": "Stereo USB Microphone",
        }),
    };

    app.update_worker_audio(&event);

    assert!(app.audio_capture_active);
    assert_eq!(app.audio_meter_level, 0.31);
    assert_eq!(app.audio_meter_raw_dbfs, Some(-35.5));
    assert_eq!(app.audio_meter_peak, Some(0.12));
    assert_eq!(app.active_audio_device, "Stereo USB Microphone");
    assert_eq!(
        audio_meter_level(
            app.audio_meter_level,
            app.runtime_state,
            app.audio_capture_active
        ),
        0.31
    );
}

#[test]
fn worker_audio_event_with_inactive_state_clears_meter_readings() {
    let mut app = test_app(AppSettings::default());
    app.runtime_state = RuntimeState::Running;
    // The meter is showing live readings from a prior recording chunk.
    app.audio_meter_level = 0.4;
    app.audio_meter_raw_dbfs = Some(-12.0);
    app.audio_meter_peak = Some(0.8);

    // An audio event whose state means capture is no longer active must blank
    // the whole meter — not just the level — so stale dBFS/peak don't linger.
    let event = WorkerEvent {
        event: "audio".to_owned(),
        state: Some("transcribing".to_owned()),
        payload: serde_json::json!({
            "event": "audio",
            "state": "transcribing",
            "level": 0.31,
            "raw_dbfs": -35.5,
            "peak": 0.12,
        }),
    };

    app.update_worker_audio(&event);

    assert!(!app.audio_capture_active);
    assert_eq!(app.audio_meter_level, 0.0);
    assert_eq!(app.audio_meter_raw_dbfs, None);
    assert_eq!(app.audio_meter_peak, None);
}

#[test]
fn toggling_log_view_persists_immediately_without_marking_settings_dirty() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    assert!(!app.has_unsaved_settings());

    // Switching the runtime log view should apply live, update the persisted
    // preference, and NOT leave the settings form looking unsaved.
    app.set_log_view(LogViewMode::Debug);

    assert_eq!(app.runtime_log_view, LogViewMode::Debug);
    assert_eq!(app.settings.ui_log_view, LogViewMode::Debug.id());
    assert_eq!(app.saved_settings.ui_log_view, LogViewMode::Debug.id());
    assert!(
        !app.has_unsaved_settings(),
        "toggling the log view must not mark settings dirty"
    );

    // The preference is written through to disk so it survives a restart.
    let on_disk = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk.ui_log_view, LogViewMode::Debug.id());
}

#[test]
fn toggling_log_view_leaves_unrelated_pending_edits_uncommitted() {
    let _lock = ENV_TEST_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let _config_guard = EnvVarGuard::set("VOICEPI_CONFIG", &config.to_string_lossy());

    let mut app = test_app(AppSettings::default());
    // A genuine unsaved edit the user has not chosen to save yet.
    app.settings.beam_size = "9".to_owned();
    assert!(app.has_unsaved_settings());

    app.set_log_view(LogViewMode::Debug);

    // The edit is still pending (dirty), and disk only got the view preference,
    // not the unrelated edit.
    assert!(app.has_unsaved_settings());
    let on_disk = config::AppSettings::from_value(
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(on_disk.ui_log_view, LogViewMode::Debug.id());
    assert_eq!(on_disk.beam_size, AppSettings::default().beam_size);
}

#[test]
fn debug_view_pretty_prints_utterance_json() {
    let log = [
        "[worker] status=ready",
        r#"[utterance] {"text":"hej","recording_s":2.0}"#,
    ]
    .join("\n");
    let debug = log_view_text(&log, LogViewMode::Debug);
    // Plain lines pass through unchanged...
    assert!(debug.contains("[worker] status=ready"));
    // ...but the one-line utterance JSON is expanded over indented lines.
    assert!(debug.contains("[utterance]\n{"));
    assert!(debug.contains("\n  \"text\": \"hej\""));
}

#[test]
fn diagnostic_view_drops_transient_worker_step_cards() {
    let log = [
        "[worker] status=ready",        // lifecycle → kept
        "[worker] status=recording",    // transient step → dropped
        "[worker] status=transcribing", // transient step → dropped
        r#"[utterance] {"text":"hej","recording_s":2.0}"#,
    ]
    .join("\n");
    let cards = runtime_log_cards(&log, LogViewMode::Diagnostic);
    let titles: Vec<&str> = cards.iter().map(|c| c.title.as_str()).collect();
    assert!(titles.contains(&"ready"), "ready worker card kept");
    assert!(!titles.contains(&"recording"));
    assert!(!titles.contains(&"transcribing"));
    assert!(cards.iter().any(|c| c.badge == "Utterance"));
}

#[test]
fn live_preview_status_lines_never_produce_cards() {
    // The high-frequency live "preview" ticks must NOT spawn a card each in
    // Minimal or Diagnostic (they would flood the card view); the growing text
    // is shown live in the recording card instead. The surrounding lifecycle /
    // final lines still produce their cards.
    let log = [
        "[worker] status=ready",
        "[worker] status=recording",
        "[worker] status=preview text_preview=hello recording_s=1.5",
        "[worker] status=preview text_preview=hello there recording_s=3.0",
        "[worker] status=preview text_preview=hello there friend recording_s=4.5",
        "[inject] -> \"hello there friend\"  (target: editor)",
    ]
    .join("\n");

    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = runtime_log_cards(&log, mode);
        // No card may carry the preview text or a "preview" worker title — the
        // three preview ticks must collapse to zero cards. (The `ready`
        // lifecycle line still legitimately yields one Worker-badge card in
        // Diagnostic, so we don't forbid the Worker badge outright.)
        assert!(
            !cards
                .iter()
                .any(|c| c.title.contains("preview") || c.detail.contains("text_preview")),
            "{mode:?}: a preview status leaked into a card: {cards:?}"
        );
    }
    // Diagnostic keeps exactly the `ready` Worker card (the three preview ticks
    // produced none).
    let diagnostic = runtime_log_cards(&log, LogViewMode::Diagnostic);
    let worker_titles: Vec<&str> = diagnostic
        .iter()
        .filter(|c| c.badge == "Worker")
        .map(|c| c.title.as_str())
        .collect();
    assert_eq!(worker_titles, vec!["ready"]);

    // Minimal still surfaces exactly the final injected text.
    let minimal = runtime_log_cards(&log, LogViewMode::Minimal);
    assert_eq!(minimal.len(), 1);
    assert_eq!(minimal[0].title, "hello there friend");
    assert_eq!(minimal[0].badge, "Final");

    // Debug (raw) view DOES keep the preview lines for troubleshooting.
    let debug = log_view_text(&log, LogViewMode::Debug);
    assert!(debug.contains("status=preview"));
}

#[test]
fn no_text_card_renders_friendly_title_in_diagnostic_and_minimal() {
    // no_audio → shown in both modes
    let line_no_audio = "[worker] status=no_text reason=no_audio";
    let card_diag = runtime_log_cards(line_no_audio, LogViewMode::Diagnostic);
    assert_eq!(card_diag.len(), 1);
    assert_eq!(card_diag[0].badge, "No text");
    assert_eq!(card_diag[0].title, "No audio captured");
    assert_eq!(card_diag[0].kind, RuntimeLogCardKind::Status);

    let card_min = runtime_log_cards(line_no_audio, LogViewMode::Minimal);
    assert_eq!(card_min.len(), 1);
    assert_eq!(card_min[0].badge, "No text");
    assert_eq!(card_min[0].title, "No audio captured");

    // too_short → correct title and recording_s surfaced
    let line_too_short = "[worker] status=no_text reason=too_short recording_s=0.12";
    let card_short = runtime_log_cards(line_too_short, LogViewMode::Diagnostic);
    assert_eq!(card_short.len(), 1);
    assert_eq!(
        card_short[0].title,
        "Too short — hold the key while speaking"
    );
    assert!(
        card_short[0].detail.contains("recording_s=0.12"),
        "detail should carry recording_s; got: {:?}",
        card_short[0].detail
    );

    // too_quiet
    let line_tq = "[worker] status=no_text reason=too_quiet recording_s=1.5";
    let card_tq = runtime_log_cards(line_tq, LogViewMode::Minimal);
    assert_eq!(card_tq[0].title, "Too quiet — check the microphone level");

    // no_speech
    let line_ns = "[worker] status=no_text reason=no_speech recording_s=2.0";
    let card_ns = runtime_log_cards(line_ns, LogViewMode::Diagnostic);
    assert_eq!(card_ns[0].title, "No speech detected");

    // unknown reason falls back to generic title
    let line_unk = "[worker] status=no_text reason=xyzzy";
    let card_unk = runtime_log_cards(line_unk, LogViewMode::Diagnostic);
    assert_eq!(card_unk[0].title, "No text produced");
}

#[test]
fn no_text_card_not_swallowed_by_structured_utterance_suppression() {
    // When a structured utterance IS present in the log, no_text cards from a
    // different (failed) press must still appear — they are not [post]/detail lines.
    let log = [
        "[worker] status=no_text reason=too_short recording_s=0.05",
        r#"[utterance] {"text":"hello","recording_s":2.0}"#,
    ]
    .join("\n");
    let cards = runtime_log_cards(&log, LogViewMode::Diagnostic);
    assert!(
        cards.iter().any(|c| c.badge == "No text"),
        "no_text card must survive structured-utterance suppression; cards={cards:?}"
    );
}

#[test]
fn worker_event_mappers_include_no_text_state() {
    // no_text signals the worker has returned to ready (model still loaded).
    assert_eq!(worker_ready_for_state("no_text"), Some(true));
    // no_text means capture is no longer active.
    assert_eq!(
        audio_capture_active_for_worker_state("no_text"),
        Some(false)
    );
    // no_text is NOT a pipeline stage (no live progress card).
    assert_eq!(pipeline_stage_for_worker_state("no_text"), None);
}

#[test]
fn capture_lost_card_renders_friendly_title_in_diagnostic_and_minimal() {
    let line = "[worker] status=capture_lost reason=arecord_eof";

    // Diagnostic mode.
    let card_diag = runtime_log_cards(line, LogViewMode::Diagnostic);
    assert_eq!(card_diag.len(), 1);
    assert_eq!(card_diag[0].badge, "Capture lost");
    assert_eq!(
        card_diag[0].title,
        "Audio capture stopped unexpectedly — check the microphone"
    );
    assert_eq!(card_diag[0].kind, RuntimeLogCardKind::Status);

    // Minimal mode (also shows capture_lost).
    let card_min = runtime_log_cards(line, LogViewMode::Minimal);
    assert_eq!(card_min.len(), 1);
    assert_eq!(card_min[0].badge, "Capture lost");
    assert_eq!(
        card_min[0].title,
        "Audio capture stopped unexpectedly — check the microphone"
    );
}

#[test]
fn capture_lost_card_not_swallowed_by_transient_state_filter() {
    // The transient-state filter in diagnostic_log_card must NOT drop capture_lost.
    let log = [
        "[worker] status=recording",
        "[worker] status=capture_lost reason=arecord_eof",
    ]
    .join("\n");
    let cards = runtime_log_cards(&log, LogViewMode::Diagnostic);
    assert!(
        cards.iter().any(|c| c.badge == "Capture lost"),
        "capture_lost card must survive transient-state suppression; cards={cards:?}"
    );
}

#[test]
fn diagnostic_utterance_detail_groups_onto_separate_lines() {
    let log = concat!(
        r#"[utterance] {"text":"hej","recording_s":2.0,"compute_s":0.5,"#,
        r#""real_time_factor":0.04,"stt_backend":"whisper"}"#,
    );
    let cards = runtime_log_cards(log, LogViewMode::Diagnostic);
    let card = cards
        .iter()
        .find(|c| c.badge == "Utterance")
        .expect("utterance card");
    // Groups are newline-separated (audio line, compute line, backend line…),
    // and the metric tokens are preserved.
    assert!(card.detail.contains('\n'));
    assert!(card.detail.contains("recording=2.0s"));
    assert!(card.detail.contains("compute=0.5s"));
    assert!(card.detail.contains("backend=whisper"));
}

#[test]
fn drag_overshoot_delta_follows_selection_past_the_edges() {
    use crate::ui::tabs::drag_overshoot_delta;
    // Inside the viewport: no auto-scroll.
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 300.0), 0.0);
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 100.0), 0.0);
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 500.0), 0.0);
    // Below the bottom: scroll toward later content (negative), growing with
    // the overshoot.
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 520.0), -10.0);
    assert!(drag_overshoot_delta(100.0, 500.0, 540.0) < drag_overshoot_delta(100.0, 500.0, 520.0));
    // Above the top: scroll toward earlier content (positive).
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 80.0), 10.0);
    // Capped so a wild drag stays controllable.
    assert_eq!(drag_overshoot_delta(100.0, 500.0, 5000.0), -30.0);
    assert_eq!(drag_overshoot_delta(100.0, 500.0, -5000.0), 30.0);
}

#[test]
fn health_card_shown_in_minimal_and_diagnostic() {
    let line = "[health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq";
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let cards = runtime_log_cards(line, mode);
        assert_eq!(cards.len(), 1, "expected one health card in {mode:?}");
        assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthOk);
        assert_eq!(cards[0].badge, "HealthOk");
        assert_eq!(
            cards[0].title,
            "mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq"
        );
        assert_eq!(cards[0].detail, "Microphone + model health");
    }
}

#[test]
fn health_card_flags_warnings_with_distinct_badge() {
    let line = "[health] mic -55dBFS SNR 3dB too_quiet | confidence low (-0.82) \
        | post off | WARN low confidence | WARN quiet input";
    let cards = runtime_log_cards(line, LogViewMode::Minimal);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].kind, RuntimeLogCardKind::HealthWarn);
    assert_eq!(cards[0].badge, "HealthWarn");
    assert!(cards[0].detail.contains("warnings"));
    assert!(cards[0].title.contains("WARN low confidence"));
}

#[test]
fn health_card_warn_detection_is_structural_not_substring() {
    // A field that merely contains the substring "WARN" (e.g. a provider/model
    // name) must NOT trigger the warning badge — only a genuine `| WARN ...`
    // segment should.
    let no_warn_line =
        "[health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/WARNer-llm";
    let cards = runtime_log_cards(no_warn_line, LogViewMode::Minimal);
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].kind,
        RuntimeLogCardKind::HealthOk,
        "WARN inside a field value must not trigger the warning card kind"
    );
    assert_eq!(
        cards[0].badge, "HealthOk",
        "WARN inside a field value must not trigger the warning badge"
    );

    // But a real `| WARN ...` segment must still fire.
    let warn_line =
        "[health] mic -38dBFS SNR 56dB good | confidence low (-0.82) | post off | WARN low confidence";
    let warn_cards = runtime_log_cards(warn_line, LogViewMode::Minimal);
    assert_eq!(warn_cards.len(), 1);
    assert_eq!(warn_cards[0].kind, RuntimeLogCardKind::HealthWarn);
    assert_eq!(warn_cards[0].badge, "HealthWarn");
}

#[test]
fn health_line_kept_in_diagnostic_text_view() {
    let log = "[health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post off\n\
        [cap] raw=-38dBFS peak=0.100 input=good gain=2.0x noise=-90dBFS snr=56dB";
    let diagnostic = log_view_text(log, LogViewMode::Diagnostic);
    assert!(
        diagnostic.contains("[health]"),
        "health line should survive the diagnostic text filter; got: {diagnostic:?}"
    );
}
