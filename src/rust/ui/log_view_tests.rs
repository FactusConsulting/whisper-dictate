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
