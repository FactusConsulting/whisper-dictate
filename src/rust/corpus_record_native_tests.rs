//! Tests for [`crate::corpus_record_native`], extracted from the module file
//! to keep it under the 500-line modularity limit.
//!
//! Included via `#[cfg(test)] #[path = "corpus_record_native_tests.rs"] mod
//! tests;` in the parent so `use super::*` reaches every private item
//! (`f32_to_i16`, `round1`, `resolve_item`, `write_wav_int16`, `CorpusEvent`,
//! `TARGET_SAMPLE_RATE`, …). Kept as a sibling file rather than
//! `#[cfg(test)] mod tests {}` inline so the module file itself is a compact
//! read.

use super::*;

#[test]
fn output_wav_path_matches_worker_layout() {
    // Must land under `<appdata>/benchmark/audio/<id>.wav` — the SAME
    // location `vp_corpus_record._write_wav` writes to (via
    // `appdata_audio_dir`) AND the location `ui::corpus::recorded_audio_path`
    // reads back for the picker's ✓ marker. If this diverges, a fresh
    // native recording won't show its check-mark and the benchmark will
    // report the item as "no audio available".
    let appdata = Path::new("/home/u/.config/whisper-dictate");
    let path = output_wav_path(appdata, "da-001");
    assert!(
        path.ends_with(Path::new("benchmark").join("audio").join("da-001.wav")),
        "unexpected path: {path:?}",
    );
}

#[test]
fn wav_bytes_match_16k_mono_int16_shape() {
    // Byte-shape parity guard vs. Python's `wave.open(...)` with
    // setnchannels=1, setsampwidth=2, setframerate=16000: the WAV must
    // parse back as a 1-channel, 16 kHz, 16-bit-int file with exactly
    // the sample count we wrote. Any drift from this shape (wrong rate,
    // stereo, 32-bit float) would break interchangeability with the
    // corpus WAVs already recorded by the Python worker.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("nested").join("dir").join("a.wav");
    let pcm: Vec<i16> = vec![0, 100, -100, 32_767, -32_768, 1_234];
    write_wav_int16(&path, &pcm).expect("write");
    assert!(path.exists(), "wav must be created");
    let reader = hound::WavReader::open(&path).expect("read back");
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "must be mono");
    assert_eq!(spec.sample_rate, TARGET_SAMPLE_RATE, "must be 16 kHz");
    assert_eq!(spec.bits_per_sample, 16, "must be 16-bit");
    assert!(
        matches!(spec.sample_format, hound::SampleFormat::Int),
        "must be integer PCM (not float)",
    );
    let read_back: Vec<i16> = reader
        .into_samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .expect("samples");
    assert_eq!(read_back, pcm, "samples must round-trip verbatim");
}

#[test]
fn peak_rms_dbfs_empty_matches_python_zero_case() {
    // Python's _peak_rms_dbfs returns peak_dbfs = -120.0 when peak == 0.
    // The RMS path floors to 1e-9 → 20*log10(1e-9) ≈ -180.0. Pin both
    // so the shape stays parseable by the UI's `DoneEvent` deserialiser
    // even for an all-zero capture.
    let (peak, rms) = peak_rms_dbfs(&[]);
    assert_eq!(peak, -120.0);
    assert!(rms <= -170.0 && rms >= -180.1, "unexpected rms: {rms}");
}

#[test]
fn peak_rms_dbfs_full_scale_sine_reads_near_zero_peak() {
    // A ±32767 square wave has peak norm ≈ 1.0 → peak_dbfs ≈ 0.0.
    // RMS of a full-swing ±1.0 square is 1.0 → rms_dbfs ≈ 0.0. Off by
    // at most one LSB on i16 MIN so a tiny tolerance is fine.
    let pcm: Vec<i16> = (0..1024)
        .map(|i| if i & 1 == 0 { 32_767 } else { -32_768 })
        .collect();
    let (peak, rms) = peak_rms_dbfs(&pcm);
    assert!(peak.abs() < 0.1, "peak dBFS should be ~0, got {peak}");
    assert!(rms.abs() < 0.1, "rms dBFS should be ~0, got {rms}");
}

#[test]
fn peak_rms_dbfs_half_scale_reads_about_minus_six_db() {
    // A ±16384 square wave has peak norm = 0.5 → peak_dbfs ≈ -6.0.
    let pcm: Vec<i16> = (0..1024)
        .map(|i| if i & 1 == 0 { 16_384 } else { -16_384 })
        .collect();
    let (peak, _rms) = peak_rms_dbfs(&pcm);
    assert!((peak + 6.0).abs() < 0.5, "expected ~-6.0 dBFS, got {peak}");
}

#[test]
fn f32_to_i16_clamps_out_of_range() {
    // Prevents the wrap-to-negative you get from a naive `(x * 32767) as i16`
    // on x > 1.0 — matches Python's numpy clip-then-multiply idiom in
    // `_capture_frame_to_int16`.
    assert_eq!(f32_to_i16(2.0), i16::MAX);
    assert_eq!(f32_to_i16(-2.0), -i16::MAX);
    assert_eq!(f32_to_i16(0.0), 0);
    assert_eq!(f32_to_i16(1.0), i16::MAX);
    assert_eq!(f32_to_i16(-1.0), -i16::MAX);
}

#[test]
fn round1_matches_python_half_precision() {
    // Pins the rounding rule the Done event's dBFS fields use — the UI's
    // `corpus_record_log_detail` prints these to one decimal.
    assert_eq!(round1(-6.05), -6.1);
    assert_eq!(round1(-6.04), -6.0);
    assert_eq!(round1(0.0), 0.0);
    assert_eq!(round1(-120.0), -120.0);
}

#[test]
fn resolve_item_missing_manifest_errors_cleanly() {
    // A machine with no corpus manifest anywhere (fresh install / test
    // scaffold) MUST yield a clean error string, not a panic — the
    // caller surfaces this as a single `corpus_record_error` event.
    let tmp = tempfile::tempdir().unwrap();
    let err = resolve_item("da-001", tmp.path(), tmp.path()).unwrap_err();
    assert!(
        err.to_lowercase().contains("no benchmark corpus"),
        "unexpected error: {err}",
    );
}

#[test]
fn resolve_item_unknown_id_errors_cleanly() {
    // A corpus that does not contain the requested id must surface a
    // short, greppable message — mirroring `vp_corpus_record._resolve_item`
    // which raises `LookupError(f"unknown corpus id: {item_id}")`. The
    // UI's log-detail line uses this string verbatim.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("benchmark").join("corpus.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest,
        r#"{"items":[{"id":"other","text":"Hi","language":"en"}]}"#,
    )
    .unwrap();
    let err = resolve_item("da-001", tmp.path(), tmp.path()).unwrap_err();
    assert!(err.contains("unknown corpus id"), "unexpected error: {err}");
    assert!(err.contains("da-001"), "should name the id: {err}");
}

#[test]
fn resolve_item_finds_matching_item_by_id() {
    // Positive control: a valid manifest + a matching id returns the
    // parsed item so downstream event emission has the reference text
    // for the start line.
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("benchmark").join("corpus.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    std::fs::write(
        &manifest,
        r#"{"items":[{"id":"da-001","text":"Hej med dig.","language":"da"}]}"#,
    )
    .unwrap();
    let item = resolve_item("da-001", tmp.path(), tmp.path()).expect("resolve");
    assert_eq!(item.id, "da-001");
    assert_eq!(item.text, "Hej med dig.");
}

#[test]
fn event_start_line_matches_ui_parser_contract() {
    // The UI's `parse_corpus_record_result` walks stdout for lines with
    // `"event":"corpus_record_start"` and extracts `id`, `text`, `seconds`.
    // Serialise the same shape here and cross-check every field so a
    // future rename can't drift silently.
    let ev = CorpusEvent::Start {
        event: "corpus_record_start",
        id: "da-001",
        text: "Hej",
        seconds: 10.0,
    };
    let line = serde_json::to_string(&ev).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["event"], "corpus_record_start");
    assert_eq!(value["id"], "da-001");
    assert_eq!(value["text"], "Hej");
    assert_eq!(value["seconds"], 10.0);
    assert!(!line.contains('\n'), "one event = one line: {line}");
}

#[test]
fn event_done_line_matches_ui_parser_contract() {
    // Pin the Done envelope shape — the UI's DoneEvent deserializer
    // requires `id`, `path`, `seconds_recorded`, and optionally
    // `peak_dbfs`. All four MUST be present with those exact names.
    let ev = CorpusEvent::Done {
        event: "corpus_record_done",
        id: "da-001",
        path: "/a/da-001.wav",
        seconds_recorded: 9.8,
        peak_dbfs: -6.0,
        rms_dbfs: -20.0,
    };
    let line = serde_json::to_string(&ev).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["event"], "corpus_record_done");
    assert_eq!(value["id"], "da-001");
    assert_eq!(value["path"], "/a/da-001.wav");
    assert_eq!(value["seconds_recorded"], 9.8);
    assert_eq!(value["peak_dbfs"], -6.0);
    assert_eq!(value["rms_dbfs"], -20.0);
    // Field-name lockstep with `ui::corpus_record::DoneEvent`: the
    // UI's serde_derive deserialiser reads exactly `id`, `path`,
    // `seconds_recorded`, `peak_dbfs`. Every one MUST be present so
    // the terminal-event scanner turns this into `Saved { ... }`.
    for field in ["id", "path", "seconds_recorded", "peak_dbfs"] {
        assert!(
            value.get(field).is_some(),
            "missing field {field} in {line}"
        );
    }
}

#[test]
fn event_error_line_matches_ui_parser_contract() {
    let ev = CorpusEvent::Error {
        event: "corpus_record_error",
        error: "unknown corpus id: x",
    };
    let line = serde_json::to_string(&ev).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["event"], "corpus_record_error");
    assert_eq!(value["error"], "unknown corpus id: x");
}

#[test]
fn event_progress_line_matches_ui_countdown_contract() {
    // The progress line's remaining_s must serialise as an integer (the
    // Python worker uses `int(round(remaining))`). Anything else would
    // still be parseable but the UI's log line would print a float, so
    // pin the int shape.
    let ev = CorpusEvent::Progress {
        event: "corpus_record_progress",
        remaining_s: 5,
    };
    let line = serde_json::to_string(&ev).unwrap();
    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["event"], "corpus_record_progress");
    assert_eq!(value["remaining_s"], 5);
    assert!(value["remaining_s"].is_i64(), "remaining_s must be int");
}

#[test]
fn danish_text_survives_ensure_ascii_false_equivalent() {
    // Python's `_print_event` uses `ensure_ascii=False` so Danish
    // reference text is written verbatim. serde_json's default is the
    // same (non-ASCII characters serialise literally, not as \uXXXX).
    // Pin this so a future serde_json config change doesn't quietly
    // start escaping — the UI's parser doesn't care but a
    // grep-for-text log consumer would break.
    let ev = CorpusEvent::Start {
        event: "corpus_record_start",
        id: "da-001",
        text: "Hej med dig, æøå",
        seconds: 10.0,
    };
    let line = serde_json::to_string(&ev).unwrap();
    assert!(line.contains("Hej med dig, æøå"), "escaped: {line}");
}
