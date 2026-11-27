//! The transcript copy button on a runtime log card, driven through a
//! headless egui pass: the button is painted only on transcript cards, a
//! click on it returns the card's text, and a disabled button returns none.

use super::super::*;
use super::log_card::runtime_log_card;
use egui_material_icons::icons;

fn card(kind: RuntimeLogCardKind, title: &str, badge: &str) -> RuntimeLogCard {
    RuntimeLogCard {
        kind,
        title: title.to_owned(),
        detail: String::new(),
        badge: badge.to_owned(),
        mic_quality: None,
    }
}

fn screen() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 400.0))
}

/// Run one frame of `card` and return what the card reported plus where the
/// copy glyph was painted, if it was.
fn frame(
    ctx: &egui::Context,
    card: &RuntimeLogCard,
    enabled: bool,
    events: Vec<egui::Event>,
) -> (Option<String>, Option<egui::Pos2>) {
    let palette = ui_palette("dark");
    let mut copied = None;
    let input = egui::RawInput {
        screen_rect: Some(screen()),
        events,
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        copied = runtime_log_card(ui, card, "", palette, Some((enabled, "Copy")), "en")
            .map(str::to_owned);
    });
    output.textures_delta.clear();
    let glyph = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == icons::ICON_CONTENT_COPY.codepoint => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        });
    (copied, glyph)
}

fn click(at: egui::Pos2) -> Vec<Vec<egui::Event>> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    vec![
        vec![egui::Event::PointerMoved(at)],
        vec![button(true)],
        vec![button(false)],
    ]
}

/// Render `card`, click where its copy glyph was painted, and return what the
/// card reported on the frame of the click.
fn click_copy(card: &RuntimeLogCard, enabled: bool) -> Option<String> {
    let ctx = egui::Context::default();
    let (_, glyph) = frame(&ctx, card, enabled, Vec::new());
    let at = glyph.expect("a transcript card paints its copy glyph");
    let mut copied = None;
    for events in click(at) {
        copied = copied.or(frame(&ctx, card, enabled, events).0);
    }
    copied
}

#[test]
fn a_click_on_the_copy_button_returns_the_transcript() {
    let final_text = card(RuntimeLogCardKind::FinalText, "hello world", "Final");
    assert_eq!(
        click_copy(&final_text, true).as_deref(),
        Some("hello world")
    );
    let dictation = card(RuntimeLogCardKind::Diagnostic, "spoken text", "Utterance");
    assert_eq!(click_copy(&dictation, true).as_deref(), Some("spoken text"));
}

#[test]
fn a_disabled_copy_button_returns_nothing() {
    let final_text = card(RuntimeLogCardKind::FinalText, "hello world", "Final");
    assert_eq!(click_copy(&final_text, false), None);
}

#[test]
fn cards_without_a_transcript_paint_no_copy_button() {
    let ctx = egui::Context::default();
    for card in [
        card(RuntimeLogCardKind::Status, "Worker started", "Worker"),
        card(RuntimeLogCardKind::HealthFair, "noisy", "HealthFair"),
    ] {
        assert_eq!(frame(&ctx, &card, true, Vec::new()).1, None, "{card:?}");
    }
    let final_text = card(RuntimeLogCardKind::FinalText, "hello", "Final");
    assert!(frame(&ctx, &final_text, true, Vec::new()).1.is_some());
    // Without a copy configuration a transcript card paints none either.
    let palette = ui_palette("dark");
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen()),
            ..Default::default()
        },
        |ui| {
            runtime_log_card(ui, &final_text, "", palette, None, "en");
        },
    );
    output.textures_delta.clear();
    assert!(!output.shapes.iter().any(|clipped| matches!(
        &clipped.shape,
        egui::Shape::Text(text) if text.galley.text() == icons::ICON_CONTENT_COPY.codepoint
    )));
}

fn synthetic_input(amplitude: f32, floor: f32) -> serde_json::Value {
    use crate::audio_dsp::{capture_metrics, StatusThresholds, FRAME_SAMPLES};

    let samples: Vec<_> = (0..20 * FRAME_SAMPLES)
        .map(|i| {
            let level = if i / FRAME_SAMPLES < 8 {
                floor
            } else {
                amplitude
            };
            if i % 2 == 0 {
                level
            } else {
                -level
            }
        })
        .collect();
    let metrics = capture_metrics(&samples, &StatusThresholds::default());
    serde_json::json!({
        "audio_raw_dbfs": metrics.raw_dbfs,
        "audio_peak": metrics.peak,
        "audio_noise_dbfs": metrics.noise_dbfs,
        "audio_snr_db": metrics.snr_db,
        "audio_gain": metrics.gain,
        "audio_input_status": metrics.input_status,
    })
}

fn painted_card(log: &str, mode: LogViewMode, lang: &str, width: f32) -> String {
    let cache = RuntimeLogCache::from_log(log);
    let [card] = cache.cards(mode) else {
        panic!("expected exactly one card in {mode:?}");
    };
    let output = egui::Context::default().run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 400.0),
            )),
            ..Default::default()
        },
        |ui| {
            runtime_log_card(
                ui,
                card,
                &card.badge,
                ui_palette("dark"),
                Some((true, "Copy")),
                lang,
            );
        },
    );
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn final_cards_paint_capture_only_quality_and_diagnostic_numbers_once() {
    for (amplitude, floor, verdict) in [
        (0.0005, 0.00001, "Mic quality: Low — signal too weak"),
        (
            0.10,
            0.08,
            "Mic quality: Low — weak signal relative to noise",
        ),
        (0.99, 0.00001, "Mic quality: Low — risk of clipping"),
        (0.006, 0.00001, "Mic quality: Adequate — quiet signal"),
        (0.10, 0.025, "Mic quality: Usable — limited contrast"),
        (0.10, 0.00001, "Mic quality: Good"),
    ] {
        let mut payload = synthetic_input(amplitude, floor);
        payload["text"] = serde_json::json!("Spoken words.");
        payload["post_processor"] = serde_json::json!("groq");
        payload["post_mode"] = serde_json::json!("clean");
        payload["health_grade"] = serde_json::json!("poor");
        let log = format!("[utterance] {payload}");
        let minimal = painted_card(&log, LogViewMode::Minimal, "en", 300.0);
        assert!(minimal.contains("Spoken words."));
        assert!(minimal.contains("Final"));
        assert!(minimal.contains(verdict), "{minimal}");
        assert!(minimal.contains("Post-processing: clean (groq)"));
        let diagnostic = painted_card(&log, LogViewMode::Diagnostic, "en", 900.0);
        assert!(diagnostic.contains(verdict), "{diagnostic}");
        for metric in ["raw=", "peak=", "noise=", "snr=", "gain="] {
            assert!(!minimal.contains(metric));
            assert_eq!(diagnostic.matches(metric).count(), 1, "{diagnostic}");
        }
    }
}

#[test]
fn missing_or_malformed_measurements_do_not_paint_mic_quality() {
    let legacy = serde_json::json!({ "text": "Legacy", "health_grade": "good" });
    let mut malformed = synthetic_input(0.10, 0.00001);
    malformed["text"] = serde_json::json!("Malformed");
    malformed["audio_peak"] = serde_json::json!("NaN");
    for payload in [legacy, malformed] {
        let log = format!("[utterance] {payload}");
        for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
            let painted = painted_card(&log, mode, "en", 900.0);
            assert!(painted.contains(payload["text"].as_str().unwrap()));
            assert!(!painted.contains("Mic quality:"), "{painted}");
        }
    }
}

#[test]
fn measured_no_text_paints_localized_quality_without_losing_failure() {
    let mut payload = synthetic_input(0.10, 0.00001);
    payload["reason"] = serde_json::json!("insufficient_gpu_memory");
    payload["error"] = serde_json::json!("GPU needs 4 GiB; 1 GiB is free.");
    let log = worker_status_log_line(&WorkerEvent {
        event: "status".to_owned(),
        state: Some("no_text".to_owned()),
        payload,
    })
    .unwrap();
    for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
        let painted = painted_card(&log, mode, "da", 300.0);
        assert!(painted.contains("Not enough free GPU memory"), "{painted}");
        assert!(painted.contains("GPU needs 4 GiB; 1 GiB is free."));
        assert!(painted.contains("Mikrofonkvalitet: God"));
        assert_eq!(painted.contains("raw="), mode == LogViewMode::Diagnostic);
        assert!(!painted.contains("gain="));
    }
}

#[test]
fn no_text_keeps_its_reason_and_does_not_grade_invalid_audio() {
    let mut measured = synthetic_input(0.10, 0.08);
    measured["reason"] = serde_json::json!("no_speech");
    measured["recording_s"] = serde_json::json!(0.6);
    let mut malformed = measured.clone();
    malformed["audio_peak"] = serde_json::json!("0.10 audio_input_status=good");
    for (payload, has_measurement) in [(measured, true), (malformed, false)] {
        let log = worker_status_log_line(&WorkerEvent {
            event: "status".to_owned(),
            state: Some("no_text".to_owned()),
            payload,
        })
        .unwrap();
        for mode in [LogViewMode::Minimal, LogViewMode::Diagnostic] {
            let painted = painted_card(&log, mode, "en", 900.0);
            assert!(painted.contains("No speech detected"));
            assert!(painted.contains("reason=no_speech  recording_s=0.6"));
            assert_eq!(painted.contains("Mic quality:"), has_measurement);
        }
    }
}
