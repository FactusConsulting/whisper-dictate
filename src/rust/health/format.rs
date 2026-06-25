//! One-line `[health]` summary rendering.
//!
//! Format (kept verbatim from `vp_health.format_health_line` so the UI parser
//! in `ui/log_render.rs` keeps working):
//!
//! ```text
//! [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq | grade=good
//! ```
//!
//! The trailing `grade=<token>` segment is always last and never starts with
//! "WARN", so the structural `| WARN ...` detection in the UI is unaffected.

use serde_json::{Map, Value};

use super::grade::health_grade;
use super::util::{
    coerce_float, confidence_band, get, mean_avg_logprob, round_int, trimmed_string, truthy,
};

/// Render the one-line `[health]` summary from a metrics map.
///
/// Expected keys (all optional; missing ones degrade gracefully): see the
/// Python docstring on `format_health_line` in `vp_health.py`.
pub fn format_health_line(metrics: &Value) -> String {
    let map = metrics.as_object();
    let avg_logprob = mean_avg_logprob(get(map, "segments"));
    let band = confidence_band(avg_logprob);
    let confidence = match avg_logprob {
        Some(value) => format!("confidence {band} ({value:.2})"),
        None => format!("confidence {band}"),
    };

    let mut parts: Vec<String> = vec![mic_segment(map), confidence, post_segment(map)];
    parts.extend(warn_flags(map, band));
    // Trailing grade=<g> token: always last, never starts with "WARN", so the
    // structural `| WARN ...` has_warning detection in the UI is unaffected.
    parts.push(format!("grade={}", health_grade(metrics)));
    format!("[health] {}", parts.join(" | "))
}

fn mic_segment(map: Option<&Map<String, Value>>) -> String {
    let raw = round_int(get(map, "audio_raw_dbfs"));
    let snr = round_int(get(map, "audio_snr_db"));
    let mut status = trimmed_string(get(map, "audio_input_status"));
    if status.is_empty() {
        status = "n/a".to_owned();
    }
    let raw_s = raw
        .map(|value| format!("{value}dBFS"))
        .unwrap_or_else(|| "?dBFS".to_owned());
    let snr_s = snr
        .map(|value| format!("SNR {value}dB"))
        .unwrap_or_else(|| "SNR ?dB".to_owned());
    // When the input was "quiet", the worker boosted it — surface the applied
    // gain so the user can see how hard we had to work (e.g. "quiet (boosted
    // 11x)"). Matches Python's `f"{gain_f:.0f}"` rounding (half-to-even via
    // format!, same as Python format spec).
    if status == "quiet" {
        if let Some(gain) = get(map, "audio_gain").and_then(coerce_float) {
            if gain > 1.0 {
                status = format!("quiet (boosted {gain:.0}x)");
            }
        }
    }
    format!("mic {raw_s} {snr_s} {status}")
}

fn post_segment(map: Option<&Map<String, Value>>) -> String {
    let mode = trimmed_string(get(map, "post_mode"));
    let processor = trimmed_string(get(map, "post_processor"));
    if processor.is_empty() || processor == "none" || mode.is_empty() || mode == "raw" {
        "post off".to_owned()
    } else {
        format!("post {mode}/{processor}")
    }
}

fn warn_flags(map: Option<&Map<String, Value>>, band: &str) -> Vec<String> {
    let mut flags = Vec::new();
    if band == "low" {
        flags.push("WARN low confidence".to_owned());
    }
    // Quiet-but-clean is fine; only warn when input is quiet AND SNR is low.
    let status = trimmed_string(get(map, "audio_input_status"));
    let snr = get(map, "audio_snr_db").and_then(coerce_float);
    let quiet = status == "quiet" || status == "too_quiet";
    let low_snr = snr.is_some_and(|value| value < 6.0);
    if quiet && low_snr {
        flags.push("WARN quiet input".to_owned());
    }
    if truthy(get(map, "no_text")) {
        flags.push("WARN no text".to_owned());
    }
    // Post-processing silently fell back to raw, uncleaned text (most often a
    // timeout). Surface it so the Rust health card turns amber. ASCII-safe.
    if truthy(get(map, "post_fallback")) {
        let latency = round_int(get(map, "post_latency_ms")).unwrap_or(0);
        let secs = (latency.max(0)) / 1000;
        flags.push(format!("WARN post timeout->raw ({secs}s)"));
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map};

    fn segments(logprobs: &[f64]) -> Value {
        Value::Array(
            logprobs
                .iter()
                .map(|lp| json!({ "avg_logprob": lp }))
                .collect(),
        )
    }

    fn base_metrics() -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("audio_raw_dbfs".to_owned(), json!(-38.0));
        map.insert("audio_snr_db".to_owned(), json!(56.0));
        map.insert("audio_input_status".to_owned(), json!("good"));
        map.insert("post_mode".to_owned(), json!("clean"));
        map.insert("post_processor".to_owned(), json!("groq"));
        map
    }

    fn with(over: &[(&str, Value)]) -> Value {
        let mut map = base_metrics();
        for (key, value) in over {
            map.insert((*key).to_owned(), value.clone());
        }
        Value::Object(map)
    }

    // -- confidence band --------------------------------------------------

    #[test]
    fn high_band_renders_average_and_no_warn() {
        let line = format_health_line(&with(&[("segments", segments(&[-0.13, -0.20]))]));
        assert!(line.contains("confidence high (-0.17)"), "line={line}");
        assert!(!line.contains("WARN"), "line={line}");
    }

    #[test]
    fn ok_band_renders_average_and_no_warn() {
        let line = format_health_line(&with(&[("segments", segments(&[-0.45, -0.50]))]));
        assert!(line.contains("confidence ok (-0.47)"), "line={line}");
        assert!(!line.contains("WARN"));
    }

    #[test]
    fn low_band_warns() {
        let line = format_health_line(&with(&[("segments", segments(&[-0.80, -0.90]))]));
        assert!(line.contains("confidence low (-0.85)"), "line={line}");
        assert!(line.contains("WARN low confidence"));
    }

    #[test]
    fn band_boundaries_match_python() {
        // -0.35 is the high/ok boundary (>= -0.35 => high)
        assert!(format_health_line(&with(&[("segments", segments(&[-0.35]))])).contains("high"));
        assert!(format_health_line(&with(&[("segments", segments(&[-0.36]))])).contains("ok"));
        // -0.60 is the ok/low boundary (>= -0.60 => ok)
        assert!(format_health_line(&with(&[("segments", segments(&[-0.60]))])).contains("ok"));
        assert!(format_health_line(&with(&[("segments", segments(&[-0.61]))])).contains("low"));
    }

    #[test]
    fn missing_segments_is_na_without_number() {
        let line = format_health_line(&with(&[("segments", Value::Array(vec![]))]));
        assert!(line.contains("confidence n/a"));
        // No "(...)" between "confidence" and the next "|"
        let after = line.split("confidence").nth(1).unwrap();
        let stage = after.split('|').next().unwrap();
        assert!(!stage.contains('('), "stage={stage}");
    }

    // -- mic segment ------------------------------------------------------

    #[test]
    fn good_input_renders_mic_prefix() {
        let metrics = json!({
            "audio_raw_dbfs": -38.2,
            "audio_snr_db": 56.4,
            "audio_input_status": "good",
            "segments": [{"avg_logprob": -0.1}],
        });
        let line = format_health_line(&metrics);
        assert!(
            line.starts_with("[health] mic -38dBFS SNR 56dB good |"),
            "line={line}"
        );
    }

    #[test]
    fn mic_segment_uses_banker_rounding_on_halves() {
        // Codex P3 (#342): `audio_raw_dbfs: -38.5` must render as "-38dBFS"
        // (ties-to-even), not "-39dBFS" (ties-away-from-zero). Same for SNR.
        let metrics = json!({
            "audio_raw_dbfs": -38.5,
            "audio_snr_db": 24.5,
            "audio_input_status": "good",
            "segments": [{"avg_logprob": -0.1}],
        });
        let line = format_health_line(&metrics);
        assert!(
            line.starts_with("[health] mic -38dBFS SNR 24dB good |"),
            "line={line}"
        );
    }

    #[test]
    fn quiet_with_boost_renders_gain_suffix() {
        let metrics = json!({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "audio_gain": 11.3,
            "segments": [{"avg_logprob": -0.2}],
        });
        let line = format_health_line(&metrics);
        assert!(line.contains("quiet (boosted 11x)"), "line={line}");
        // Quiet + clean (high SNR) must NOT warn.
        assert!(!line.contains("WARN quiet input"));
    }

    #[test]
    fn quiet_without_gain_has_no_boost_suffix() {
        let metrics = json!({
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 40.0,
            "audio_input_status": "quiet",
            "segments": [{"avg_logprob": -0.2}],
        });
        let line = format_health_line(&metrics);
        assert!(line.contains("mic -44dBFS SNR 40dB quiet"), "line={line}");
        assert!(!line.contains("boosted"));
    }

    // -- warn flags -------------------------------------------------------

    #[test]
    fn quiet_and_low_snr_warns() {
        let line = format_health_line(&json!({
            "audio_raw_dbfs": -55.0,
            "audio_snr_db": 3.0,
            "audio_input_status": "too_quiet",
            "segments": [{"avg_logprob": -0.2}],
        }));
        assert!(line.contains("WARN quiet input"), "line={line}");
    }

    #[test]
    fn quiet_status_with_low_snr_number_warns() {
        let line = format_health_line(&json!({
            "audio_raw_dbfs": -50.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "quiet",
            "segments": [{"avg_logprob": -0.2}],
        }));
        assert!(line.contains("WARN quiet input"));
    }

    #[test]
    fn loud_but_low_snr_does_not_warn_quiet() {
        let line = format_health_line(&json!({
            "audio_raw_dbfs": -20.0,
            "audio_snr_db": 4.0,
            "audio_input_status": "low_snr",
            "segments": [{"avg_logprob": -0.2}],
        }));
        assert!(!line.contains("WARN quiet input"));
    }

    #[test]
    fn no_text_warns() {
        let line = format_health_line(&json!({"no_text": true}));
        assert!(line.contains("WARN no text"));
    }

    #[test]
    fn no_text_with_mic_metrics_renders_numbers() {
        let line = format_health_line(&json!({
            "no_text": true,
            "audio_raw_dbfs": -44.0,
            "audio_snr_db": 56.0,
            "audio_input_status": "quiet",
        }));
        assert!(line.contains("mic -44dBFS"));
        assert!(line.contains("SNR 56dB"));
        assert!(line.contains("quiet"));
        assert!(line.contains("WARN no text"));
        assert!(!line.contains("?dBFS"));
        assert!(!line.contains("SNR ?dB"));
    }

    // -- post segment -----------------------------------------------------

    #[test]
    fn post_on() {
        let line = format_health_line(&json!({
            "post_mode": "clean",
            "post_processor": "groq",
            "segments": [{"avg_logprob": -0.1}],
        }));
        assert!(line.contains("post clean/groq"));
    }

    #[test]
    fn post_off_when_processor_none() {
        let line = format_health_line(&json!({
            "post_mode": "raw",
            "post_processor": "none",
            "segments": [{"avg_logprob": -0.1}],
        }));
        assert!(line.contains("post off"));
    }

    #[test]
    fn post_off_when_keys_missing() {
        let line = format_health_line(&json!({"segments": [{"avg_logprob": -0.1}]}));
        assert!(line.contains("post off"));
    }

    #[test]
    fn post_fallback_emits_warn_segment() {
        let line = format_health_line(&json!({
            "post_mode": "clean",
            "post_processor": "groq",
            "post_fallback": true,
            "post_latency_ms": 4012,
            "segments": [{"avg_logprob": -0.1}],
        }));
        assert!(line.contains("WARN post timeout->raw (4s)"), "line={line}");
        // The WARN flag is BEFORE the trailing grade= segment.
        let parts: Vec<&str> = line.split(" | ").map(str::trim).collect();
        let warn_index = parts
            .iter()
            .position(|p| *p == "WARN post timeout->raw (4s)")
            .expect("WARN segment must be present");
        assert!(parts.last().unwrap().starts_with("grade="));
        assert!(warn_index < parts.len() - 1);
        assert!(line.contains("post clean/groq"));
    }

    #[test]
    fn post_success_has_no_warn_segment() {
        let line = format_health_line(&json!({
            "post_mode": "clean",
            "post_processor": "groq",
            "post_fallback": false,
            "post_latency_ms": 800,
            "segments": [{"avg_logprob": -0.1}],
        }));
        assert!(line.contains("post clean/groq"));
        assert!(!line.contains("WARN"));
    }

    // -- grade segment in the line ---------------------------------------

    #[test]
    fn grade_is_last_segment() {
        let line = format_health_line(&with(&[("segments", segments(&[-0.10]))]));
        let parts: Vec<&str> = line.split(" | ").map(str::trim).collect();
        assert!(parts.last().unwrap().starts_with("grade="), "line={line}");
    }

    #[test]
    fn openai_remote_line_ends_with_grade_good() {
        // Sanity: the remote-STT path still threads through to format_health_line
        // and emits grade=good without a "(...)" confidence number.
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "stt_backend": "openai",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "clean remote transcript"}],
        });
        let line = format_health_line(&metrics);
        assert!(line.contains("confidence n/a"), "line={line}");
        assert!(line.ends_with("grade=good"), "line={line}");
    }

    #[test]
    fn grade_token_matches_health_grade_across_levels() {
        let cases: Vec<Vec<(&str, Value)>> = vec![
            vec![
                ("segments", segments(&[-0.10])),
                ("audio_snr_db", json!(42.0)),
            ], // perfect
            vec![
                ("segments", segments(&[-0.20])),
                ("audio_snr_db", json!(24.0)),
                ("audio_input_status", json!("quiet")),
            ], // good
            vec![
                ("segments", segments(&[-0.45])),
                ("audio_snr_db", json!(42.0)),
            ], // fair
            vec![
                ("segments", segments(&[-0.90])),
                ("audio_snr_db", json!(42.0)),
            ], // poor
        ];
        for over in cases {
            let metrics = with(&over);
            let line = format_health_line(&metrics);
            let expected = super::super::grade::health_grade(&metrics);
            let last = line.split(" | ").last().unwrap().trim().to_owned();
            assert_eq!(last, format!("grade={expected}"), "metrics={metrics}");
        }
    }

    #[test]
    fn grade_segment_never_starts_with_warn() {
        let line = format_health_line(&with(&[
            ("segments", segments(&[-0.90])),
            ("audio_snr_db", json!(42.0)),
            ("audio_input_status", json!("too_quiet")),
        ]));
        let last = line.split(" | ").last().unwrap().trim();
        assert!(last.starts_with("grade="), "line={line}");
        assert!(!last.starts_with("WARN"));
    }
}
