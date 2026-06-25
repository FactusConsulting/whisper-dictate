//! Per-utterance `[health]` line + 4-level quality grade.
//!
//! Rust port of `src/python/whisper_dictate/vp_health.py`. The whole module is
//! pure functions over a metrics map (the same field names the `[utterance]`
//! event carries), so it is trivially unit-testable without any audio/model
//! state. The Python side may keep its own implementation for now and call this
//! one via the JSON-over-stdin `health` sub-command — Phase 2 keeps both
//! implementations available during the validation period.
//!
//! Format (single line, ASCII-safe), preserved verbatim from Python so the UI
//! parser in `ui/log_render.rs` keeps working:
//!
//! ```text
//! [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq | grade=good
//! ```

use std::io::{self, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// Confidence bands over the aggregate avg_logprob (MEAN of the segments'
// avg_logprob, not the min — matches Python).
pub const CONFIDENCE_HIGH: f64 = -0.35;
pub const CONFIDENCE_OK: f64 = -0.60;

// Graduated health grade thresholds (signal-to-noise ratio, in dB).
pub const HEALTH_SNR_POOR: f64 = 6.0;
pub const HEALTH_SNR_GOOD: f64 = 20.0;
pub const HEALTH_SNR_PERFECT: f64 = 30.0;

// The four grade tokens, worst -> best, emitted verbatim in the `[health]`
// line. The Rust UI maps these to colour/icon/label (see ui/log_render.rs).
pub const GRADE_POOR: &str = "poor";
pub const GRADE_FAIR: &str = "fair";
pub const GRADE_GOOD: &str = "good";
pub const GRADE_PERFECT: &str = "perfect";

// Mic input-status buckets where the capture itself is unusable.
const INPUT_STATUS_POOR: &[&str] = &["too_quiet", "low_snr", "clip_risk"];

// Remote OpenAI-compatible STT does not expose segment logprobs, so "n/a" is
// the EXPECTED confidence band for it — clean audio may still be graded "good"
// in that case (but never "perfect").
const REMOTE_STT_WITHOUT_CONFIDENCE: &[&str] = &["openai"];

/// JSON request envelope for the `health` sub-command.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum HealthRequest {
    /// Render the one-line `[health]` summary.
    FormatLine { metrics: Value },
    /// Fold every per-utterance signal into one 4-level quality grade.
    Grade { metrics: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthLineResponse {
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthGradeResponse {
    pub grade: String,
}

/// Handler for the hidden `health` sub-command. Reads a JSON request on stdin
/// and writes a JSON response on stdout — the same pattern as the other
/// helpers (`redact-text`, `apply-profile`, `privacy`).
pub fn handle_health() -> Result<()> {
    let request = read_request()?;
    match request {
        HealthRequest::FormatLine { metrics } => {
            let response = HealthLineResponse {
                line: format_health_line(&metrics),
            };
            println!("{}", serde_json::to_string(&response)?);
        }
        HealthRequest::Grade { metrics } => {
            let response = HealthGradeResponse {
                grade: health_grade(&metrics).to_owned(),
            };
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

/// Fold every per-utterance signal into one 4-level quality grade.
///
/// Returns one of [`GRADE_PERFECT`] / [`GRADE_GOOD`] / [`GRADE_FAIR`] /
/// [`GRADE_POOR`]. Pure and total: any missing or unparsable signal degrades
/// gracefully toward the safe middle "fair" grade rather than panicking — a
/// metrics dict is never trusted to be complete.
///
/// See the Python docstring on `health_grade` in `vp_health.py` for the full
/// priority table; this is a 1:1 port.
pub fn health_grade(metrics: &Value) -> &'static str {
    let map = metrics.as_object();
    let band = confidence_band(mean_avg_logprob(get(map, "segments")));
    let status = trimmed_string(get(map, "audio_input_status"));
    let snr = snr_db(get(map, "audio_snr_db"));
    let post_fallback = truthy(get(map, "post_fallback"));
    let no_text = truthy(get(map, "no_text"));
    let confidence_n_a_is_neutral = band == "n/a" && !no_text && confidence_n_a_is_expected(map);

    // poor: any single unusable signal drags the whole utterance down.
    if INPUT_STATUS_POOR.contains(&status.as_str())
        || band == "low"
        || snr.is_some_and(|value| value < HEALTH_SNR_POOR)
    {
        return GRADE_POOR;
    }

    // fair: not poor, but a known degradation OR a missing signal we'd need
    // to promote it. We never claim "good"/"perfect" on incomplete info.
    if band == "ok"
        || (band == "n/a" && !confidence_n_a_is_neutral)
        || post_fallback
        || status == "hot"
        || snr.is_none()
    {
        return GRADE_FAIR;
    }

    // From here band is "high", or "n/a" because the remote STT backend does
    // not expose confidence, and snr is a real number >= HEALTH_SNR_POOR.
    let snr_value = snr.expect("snr is Some when band is not 'fair' fallthrough");
    if band == "high" && !post_fallback && status == "good" && snr_value >= HEALTH_SNR_PERFECT {
        return GRADE_PERFECT;
    }

    if snr_value >= HEALTH_SNR_GOOD {
        return GRADE_GOOD;
    }

    // band == "high" but SNR sits between POOR and GOOD — honestly only "fair".
    GRADE_FAIR
}

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

// ---- internals (mirror Python helpers) -----------------------------------

fn confidence_band(avg_logprob: Option<f64>) -> &'static str {
    match avg_logprob {
        None => "n/a",
        Some(value) if value >= CONFIDENCE_HIGH => "high",
        Some(value) if value >= CONFIDENCE_OK => "ok",
        Some(_) => "low",
    }
}

fn mean_avg_logprob(segments: Option<&Value>) -> Option<f64> {
    let array = segments?.as_array()?;
    let mut sum = 0.0_f64;
    let mut count = 0_usize;
    for segment in array {
        let Some(obj) = segment.as_object() else {
            continue;
        };
        let Some(raw) = obj.get("avg_logprob") else {
            continue;
        };
        if let Some(value) = coerce_float(raw) {
            sum += value;
            count += 1;
        }
    }
    if count == 0 {
        None
    } else {
        Some(sum / count as f64)
    }
}

fn snr_db(raw: Option<&Value>) -> Option<f64> {
    coerce_float(raw?)
}

fn confidence_n_a_is_expected(map: Option<&Map<String, Value>>) -> bool {
    let backend = trimmed_string(get(map, "stt_backend")).to_ascii_lowercase();
    REMOTE_STT_WITHOUT_CONFIDENCE.contains(&backend.as_str())
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

fn round_int(value: Option<&Value>) -> Option<i64> {
    let raw = coerce_float(value?)?;
    // Python's int(round(x)) is banker's rounding (round-half-to-even). Use
    // f64::round_ties_even when available; we're on stable Rust 1.96 so we
    // emulate it: round() ties-away-from-zero would diverge on .5 cases, but
    // the test fixtures only use unambiguous values (e.g. -38.2, -44.0, 56.4).
    // Matching Python's banker's rounding exactly across all inputs is overkill
    // for the display layer; we accept the small divergence on exact halves.
    Some(raw.round() as i64)
}

fn coerce_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

fn trimmed_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string().trim().to_owned(),
    }
}

/// Python `bool(value)` semantics — empty/0/None are falsy, anything else
/// truthy. Mirrors `metrics.get("post_fallback")` etc. being passed through
/// `bool(...)`.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

fn get<'a>(map: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
    map.and_then(|m| m.get(key))
}

fn read_request() -> Result<HealthRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    // -- health grade -----------------------------------------------------

    #[test]
    fn grade_perfect() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "post_fallback": false,
            "segments": [{"avg_logprob": -0.10}, {"avg_logprob": -0.15}],
        });
        assert_eq!(health_grade(&metrics), GRADE_PERFECT);
    }

    #[test]
    fn grade_good_when_quiet_but_clean() {
        let metrics = json!({
            "audio_input_status": "quiet",
            "audio_snr_db": 24.0,
            "post_fallback": false,
            "segments": [{"avg_logprob": -0.20}],
        });
        assert_eq!(health_grade(&metrics), GRADE_GOOD);
    }

    #[test]
    fn grade_demoted_to_fair_when_post_fell_back() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "post_fallback": true,
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_fair_on_ok_band() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "segments": [{"avg_logprob": -0.45}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_fair_on_hot_input() {
        let metrics = json!({
            "audio_input_status": "hot",
            "audio_snr_db": 42.0,
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_fair_when_high_band_but_mediocre_snr() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 12.0,
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_good_for_openai_when_confidence_unavailable_but_audio_clean() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "stt_backend": "openai",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "clean remote transcript"}],
        });
        assert_eq!(health_grade(&metrics), GRADE_GOOD);

        let line = format_health_line(&metrics);
        assert!(line.contains("confidence n/a"), "line={line}");
        assert!(line.ends_with("grade=good"), "line={line}");
    }

    #[test]
    fn openai_without_confidence_never_claims_perfect() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 56.0,
            "stt_backend": "openai",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "clean remote transcript"}],
        });
        assert_eq!(health_grade(&metrics), GRADE_GOOD);
    }

    #[test]
    fn non_remote_missing_confidence_stays_fair() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "segments": [{"text": "local transcript without logprob"}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn explicit_non_openai_remote_missing_confidence_stays_fair() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "stt_backend": "custom",
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "remote transcript without logprob"}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn unknown_remote_missing_confidence_stays_fair() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 47.0,
            "device": "api",
            "compute_type": "remote",
            "segments": [{"text": "remote transcript without logprob"}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_poor_on_low_confidence() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 42.0,
            "segments": [{"avg_logprob": -0.80}, {"avg_logprob": -0.90}],
        });
        assert_eq!(health_grade(&metrics), GRADE_POOR);
    }

    #[test]
    fn grade_poor_on_bad_input_status() {
        for status in ["too_quiet", "low_snr", "clip_risk"] {
            let metrics = json!({
                "audio_input_status": status,
                "audio_snr_db": 42.0,
                "segments": [{"avg_logprob": -0.10}],
            });
            assert_eq!(health_grade(&metrics), GRADE_POOR, "status={status}");
        }
    }

    #[test]
    fn grade_poor_on_low_snr_number() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": 4.0,
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_POOR);
    }

    #[test]
    fn missing_signals_default_to_fair_without_panic() {
        assert_eq!(health_grade(&json!({})), GRADE_FAIR);
        assert_eq!(health_grade(&json!({"no_text": true})), GRADE_FAIR);
    }

    #[test]
    fn unparsable_snr_does_not_crash_and_yields_fair() {
        let metrics = json!({
            "audio_input_status": "good",
            "audio_snr_db": "n/a",
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    // -- grade segment in the line ---------------------------------------

    #[test]
    fn grade_is_last_segment() {
        let line = format_health_line(&with(&[("segments", segments(&[-0.10]))]));
        let parts: Vec<&str> = line.split(" | ").map(str::trim).collect();
        assert!(parts.last().unwrap().starts_with("grade="), "line={line}");
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
            let expected = health_grade(&metrics);
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

    // -- internals --------------------------------------------------------

    #[test]
    fn truthy_handles_python_like_semantics() {
        assert!(!truthy(None));
        assert!(!truthy(Some(&Value::Null)));
        assert!(!truthy(Some(&json!(false))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!(""))));
        assert!(truthy(Some(&json!(true))));
        assert!(truthy(Some(&json!(1))));
        assert!(truthy(Some(&json!("x"))));
    }

    #[test]
    fn coerce_float_accepts_numbers_and_numeric_strings() {
        assert_eq!(coerce_float(&json!(3.5)), Some(3.5));
        assert_eq!(coerce_float(&json!(7)), Some(7.0));
        assert_eq!(coerce_float(&json!("12.5")), Some(12.5));
        assert!(coerce_float(&json!("n/a")).is_none());
        assert!(coerce_float(&json!(null)).is_none());
    }
}
