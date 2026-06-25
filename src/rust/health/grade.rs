//! 4-level quality grade for a single utterance.
//!
//! See [`health_grade`] for the full priority table. The Rust UI maps the
//! emitted token (`"perfect"`/`"good"`/`"fair"`/`"poor"`) to a colour, icon
//! and label, so the strings here must stay stable in lock-step with the
//! Python implementation in `vp_health.py` for as long as both ports
//! coexist.

use serde_json::{Map, Value};

use super::util::{confidence_band, get, mean_avg_logprob, snr_db, trimmed_string, truthy};
use super::{HEALTH_SNR_GOOD, HEALTH_SNR_PERFECT, HEALTH_SNR_POOR};

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

/// Fold every per-utterance signal into one 4-level quality grade.
///
/// Returns one of [`GRADE_PERFECT`] / [`GRADE_GOOD`] / [`GRADE_FAIR`] /
/// [`GRADE_POOR`]. Pure and total: any missing or unparsable signal degrades
/// gracefully toward the safe middle "fair" grade rather than panicking — a
/// metrics dict is never trusted to be complete.
///
/// Priority — the worst signal wins:
///
/// * **poor**  — `audio_input_status` in {too_quiet, low_snr, clip_risk}, OR
///   the confidence band is "low", OR `audio_snr_db < HEALTH_SNR_POOR`.
/// * **fair**  — not poor, but something is off: band "ok", post-processing
///   fell back to raw, input ran "hot", OR a signal we need to judge quality
///   is missing (no SNR; no `audio_input_status`; "n/a" confidence on a
///   backend that should expose it). OpenAI-compatible remote STT is the
///   exception: "n/a" is expected there, so clean audio may still be "good"
///   but never "perfect".
/// * **good**  — clean: band "high" (or expected-unavailable for remote STT)
///   and `snr >= HEALTH_SNR_GOOD`. A "quiet" input is fine here (the worker
///   boosts it) as long as the other signals hold up.
/// * **perfect** — pristine: band "high", no post-processing fallback,
///   `audio_input_status == "good"`, and `snr >= HEALTH_SNR_PERFECT`.
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
    // to promote it. We never claim "good"/"perfect" on incomplete info —
    // a missing `audio_input_status` is treated like a missing SNR (Codex
    // P3 on PR #342: an empty status used to silently promote partial
    // payloads such as `{segments:[...], audio_snr_db: 42}` to "good").
    if band == "ok"
        || (band == "n/a" && !confidence_n_a_is_neutral)
        || post_fallback
        || status == "hot"
        || status.is_empty()
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

fn confidence_n_a_is_expected(map: Option<&Map<String, Value>>) -> bool {
    let backend = trimmed_string(get(map, "stt_backend")).to_ascii_lowercase();
    REMOTE_STT_WITHOUT_CONFIDENCE.contains(&backend.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn grade_fair_when_input_status_missing() {
        // Codex P3 (#342): a missing `audio_input_status` is incomplete info,
        // so even with high confidence + clean SNR we should NOT claim "good".
        let metrics = json!({
            "audio_snr_db": 42.0,
            "segments": [{"avg_logprob": -0.10}],
        });
        assert_eq!(health_grade(&metrics), GRADE_FAIR);
    }

    #[test]
    fn grade_fair_when_input_status_empty_string() {
        // Same as above but the key is present with an empty value — Python's
        // `str(metrics.get("audio_input_status") or "").strip()` also produces
        // "" here, so both implementations must agree the payload is incomplete.
        let metrics = json!({
            "audio_input_status": "",
            "audio_snr_db": 42.0,
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
}
