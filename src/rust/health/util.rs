//! Small JSON-coercion helpers shared by [`super::grade`] and
//! [`super::format`]. Each mirrors a piece of Python stdlib semantics the
//! original `vp_health.py` relied on (Python `bool(x)` truthiness, `int(round(
//! x))` banker's rounding, lenient `float(...)` parsing for stringly-typed
//! payloads). Keeping them here means the grade/format modules only deal in
//! domain logic.

use serde_json::{Map, Value};

use super::CONFIDENCE_HIGH;
use super::CONFIDENCE_OK;

/// Look up `key` in an optional metrics map. Returns `None` if either the map
/// or the key is missing; mirrors Python `metrics.get(key)`.
pub(super) fn get<'a>(map: Option<&'a Map<String, Value>>, key: &str) -> Option<&'a Value> {
    map.and_then(|m| m.get(key))
}

/// Mean `avg_logprob` across `segments`, or `None` when unavailable. Mirrors
/// `_mean_avg_logprob` in `vp_health.py`: a segment with a non-numeric (or
/// missing) `avg_logprob` is silently dropped, an empty/None list yields
/// `None`.
pub(super) fn mean_avg_logprob(segments: Option<&Value>) -> Option<f64> {
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

/// Map a mean `avg_logprob` into a fixed-vocabulary band. Mirrors
/// `_confidence_band`: `None` → `"n/a"`, then the (high, ok, low) thresholds.
pub(super) fn confidence_band(avg_logprob: Option<f64>) -> &'static str {
    match avg_logprob {
        None => "n/a",
        Some(value) if value >= CONFIDENCE_HIGH => "high",
        Some(value) if value >= CONFIDENCE_OK => "ok",
        Some(_) => "low",
    }
}

/// Parse `audio_snr_db` as a float. `None` when the key is missing or the
/// value is not numeric/numeric-string.
pub(super) fn snr_db(raw: Option<&Value>) -> Option<f64> {
    coerce_float(raw?)
}

/// Round to the nearest integer with ties-to-even (banker's rounding), so the
/// rendered `[health]` line matches Python's `int(round(x))`. The display
/// layer is what users see and what the Python implementation diffs against
/// during the validation period — diverging on `-38.5 → -38 vs -39` would
/// trip the cross-impl comparison even though both implementations are
/// internally consistent.
pub(super) fn round_int(value: Option<&Value>) -> Option<i64> {
    let raw = coerce_float(value?)?;
    Some(raw.round_ties_even() as i64)
}

/// Coerce a JSON value into `f64`. Accepts numbers, numeric strings (with
/// whitespace), and `true`/`false` (Python's `float(bool)` semantics, used by
/// `_round_int` and `_snr_db` on best-effort dicts).
pub(super) fn coerce_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(true) => Some(1.0),
        Value::Bool(false) => Some(0.0),
        _ => None,
    }
}

/// Stringify a JSON value the way Python's `str(metrics.get(key) or "").strip()`
/// pattern does throughout `vp_health.py`. `None` and `Null` become "", strings
/// are trimmed, anything else is rendered with `Value::to_string` and trimmed.
pub(super) fn trimmed_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string().trim().to_owned(),
    }
}

/// Python `bool(value)` semantics — empty/0/None are falsy, anything else
/// truthy. Mirrors `metrics.get("post_fallback")` etc. being passed through
/// `bool(...)`.
pub(super) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(map)) => !map.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn round_int_uses_banker_rounding_on_halves() {
        // Ties-to-even (matches Python int(round(x))):
        //   -38.5 → -38 (toward even), not -39 (away from zero).
        //   -39.5 → -40 (toward even), not -40 either; both are even/odd here.
        // Spot-check the cases Codex flagged in the PR review.
        assert_eq!(round_int(Some(&json!(-38.5))), Some(-38));
        assert_eq!(round_int(Some(&json!(-37.5))), Some(-38));
        assert_eq!(round_int(Some(&json!(0.5))), Some(0));
        assert_eq!(round_int(Some(&json!(1.5))), Some(2));
        assert_eq!(round_int(Some(&json!(2.5))), Some(2));
        assert_eq!(round_int(Some(&json!(-0.5))), Some(0));
        // Non-halves keep nearest-integer semantics.
        assert_eq!(round_int(Some(&json!(-38.2))), Some(-38));
        assert_eq!(round_int(Some(&json!(56.4))), Some(56));
        assert_eq!(round_int(Some(&json!(-44.0))), Some(-44));
        // Missing / unparsable → None.
        assert_eq!(round_int(None), None);
        assert_eq!(round_int(Some(&json!("n/a"))), None);
    }

    #[test]
    fn confidence_band_boundaries() {
        assert_eq!(confidence_band(None), "n/a");
        assert_eq!(confidence_band(Some(-0.34)), "high");
        assert_eq!(confidence_band(Some(CONFIDENCE_HIGH)), "high");
        assert_eq!(confidence_band(Some(-0.36)), "ok");
        assert_eq!(confidence_band(Some(CONFIDENCE_OK)), "ok");
        assert_eq!(confidence_band(Some(-0.61)), "low");
    }

    #[test]
    fn mean_avg_logprob_skips_non_numeric_and_handles_empty() {
        assert_eq!(mean_avg_logprob(None), None);
        assert_eq!(mean_avg_logprob(Some(&json!([]))), None);
        let arr = json!([
            {"avg_logprob": -0.1},
            {"avg_logprob": "not a number"},
            {"avg_logprob": -0.3},
            {"text": "no logprob here"},
        ]);
        // Two numeric values: (-0.1 + -0.3) / 2 = -0.2
        let got = mean_avg_logprob(Some(&arr)).unwrap();
        assert!((got - -0.2).abs() < 1e-9, "got={got}");
    }
}
