//! `dictate-ops` JSON-RPC dispatcher (hidden CLI subcommand).
//!
//! The Python shell-out fallback (`VOICEPI_DICTATE_BACKEND=rust`) lives in
//! `vp_dictate_rust.py`. Same JSON-on-stdin, JSON-on-stdout pattern the
//! rest of the worker helpers use: one
//! hidden subcommand handles every dictate-side pure-logic decision via
//! a JSON envelope on stdin:
//!
//! ```json
//! { "op": "should_skip", "params": { ... } }
//! ```
//!
//! and writes a JSON response on stdout. On any unrecognised op /
//! malformed input we exit non-zero with a structured error so the Python
//! caller can cleanly fall back to its in-process code path.
//!
//! # Op catalogue
//!
//! | op                      | response shape                                                            |
//! |-------------------------|---------------------------------------------------------------------------|
//! | `should_skip`           | `{ "decision": "keep"\|"too_short", "reason": "too_short"\|null, "hint": str\|null }` |
//! | `changed_restart_keys`  | `{ "changed": ["..."] }`                                                  |
//! | `validate_backend`      | `{ "backend": "whisper"\|"openai", "label": "..." }`                      |
//! | `is_truthy`             | `{ "truthy": bool }`                                                      |
//! | `config_dump_enabled`   | `{ "enabled": bool }`                                                     |
//! | `trace_enabled`         | `{ "enabled": bool }`                                                     |
//!
//! Wave 8 of #348 removed the NeMo/Parakeet backend, so `should_skip` no
//! longer takes `recording_s`/`parakeet_min_seconds`/`backend` parameters
//! and `validate_backend` no longer accepts `"parakeet"`.

use std::collections::BTreeMap;
use std::io::{self, Read};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::backend::{validate_backend, BackendKind};
use super::env_gates::{config_dump_enabled, is_truthy, trace_enabled};
use super::restart::changed_restart_keys;
use super::skip::{should_skip, SkipDecision};

#[derive(Debug, Deserialize)]
struct OpRequest {
    op: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct OpError {
    error: String,
}

/// Entry point wired into `main.rs`.
pub fn handle_ops() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let request: OpRequest = serde_json::from_str(&raw)
        .map_err(|err| anyhow!("malformed dictate-ops request: {err}"))?;
    let response = dispatch(&request.op, request.params)?;
    println!("{response}");
    Ok(())
}

fn dispatch(op: &str, params: Value) -> Result<String> {
    match op {
        "should_skip" => json_response(should_skip_op(params)?),
        "changed_restart_keys" => json_response(changed_restart_keys_op(params)?),
        "validate_backend" => json_response(validate_backend_op(params)?),
        "is_truthy" => json_response(is_truthy_op(params)?),
        "config_dump_enabled" => json_response(config_dump_enabled_op(params)?),
        "trace_enabled" => json_response(trace_enabled_op(params)?),
        other => {
            let err = OpError {
                error: format!("unknown dictate op: {other}"),
            };
            // Print the structured error so the Python caller can detect &
            // fall back without parsing a free-form stderr line.
            println!("{}", serde_json::to_string(&err)?);
            Err(anyhow!("unknown dictate op: {other}"))
        }
    }
}

fn json_response<T: Serialize>(value: T) -> Result<String> {
    Ok(serde_json::to_string(&value)?)
}

// ---------------------------------------------------------------- should_skip

#[derive(Debug, Deserialize)]
struct ShouldSkipParams {
    samples: usize,
    min_record_seconds: f64,
    /// Wave-8 #348: ignored — kept in the wire format so a Python client
    /// from a transitional release that still sends the legacy fields
    /// doesn't fail to parse. Drop together with the Python caller when
    /// Wave 5/8 lands the in-process supervisor.
    #[serde(default)]
    _recording_s: Option<f64>,
    #[serde(default)]
    _parakeet_min_seconds: Option<f64>,
    #[serde(default)]
    _backend: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShouldSkipResponse {
    /// Stable wire token for the decision variant.
    decision: &'static str,
    /// `"too_short"` for the short-clip rejection, `None` for keep.
    reason: Option<&'static str>,
    /// Short user-facing hint (mirrors the stdout line `_should_skip_pcm`
    /// prints). `None` when the clip is kept.
    hint: Option<&'static str>,
}

fn should_skip_op(params: Value) -> Result<ShouldSkipResponse> {
    let p: ShouldSkipParams = serde_json::from_value(params)?;
    let d = should_skip(p.samples, p.min_record_seconds);
    let decision = match d {
        SkipDecision::Keep => "keep",
        SkipDecision::TooShort => "too_short",
    };
    Ok(ShouldSkipResponse {
        decision,
        reason: d.reason(),
        hint: d.hint(),
    })
}

// ------------------------------------------------- changed_restart_keys

#[derive(Debug, Deserialize)]
struct ChangedRestartParams {
    #[serde(default)]
    before: BTreeMap<String, String>,
    #[serde(default)]
    after: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ChangedRestartResponse {
    changed: Vec<String>,
}

fn changed_restart_keys_op(params: Value) -> Result<ChangedRestartResponse> {
    let p: ChangedRestartParams = serde_json::from_value(params)?;
    Ok(ChangedRestartResponse {
        changed: changed_restart_keys(&p.before, &p.after),
    })
}

// ----------------------------------------------------- validate_backend

#[derive(Debug, Deserialize)]
struct ValidateBackendParams {
    backend: String,
}

#[derive(Debug, Serialize)]
struct ValidateBackendResponse {
    backend: &'static str,
    label: &'static str,
}

fn validate_backend_op(params: Value) -> Result<ValidateBackendResponse> {
    let p: ValidateBackendParams = serde_json::from_value(params)?;
    let kind: BackendKind = validate_backend(&p.backend).map_err(|err| anyhow!(err.to_string()))?;
    Ok(ValidateBackendResponse {
        backend: kind.as_str(),
        label: kind.label(),
    })
}

// ----------------------------------------------------------- env_gates

#[derive(Debug, Deserialize)]
struct TruthyParams {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Serialize)]
struct TruthyResponse {
    truthy: bool,
}

fn is_truthy_op(params: Value) -> Result<TruthyResponse> {
    let p: TruthyParams = serde_json::from_value(params)?;
    Ok(TruthyResponse {
        truthy: is_truthy(p.value.as_deref()),
    })
}

#[derive(Debug, Deserialize)]
struct ConfigDumpParams {
    #[serde(default)]
    voicepi_debug: Option<String>,
    #[serde(default)]
    voicepi_stt_debug: Option<String>,
}

#[derive(Debug, Serialize)]
struct EnabledResponse {
    enabled: bool,
}

fn config_dump_enabled_op(params: Value) -> Result<EnabledResponse> {
    let p: ConfigDumpParams = serde_json::from_value(params)?;
    Ok(EnabledResponse {
        enabled: config_dump_enabled(p.voicepi_debug.as_deref(), p.voicepi_stt_debug.as_deref()),
    })
}

#[derive(Debug, Deserialize)]
struct TraceParams {
    #[serde(default)]
    voicepi_trace: Option<String>,
}

fn trace_enabled_op(params: Value) -> Result<EnabledResponse> {
    let p: TraceParams = serde_json::from_value(params)?;
    Ok(EnabledResponse {
        enabled: trace_enabled(p.voicepi_trace.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(op: &str, params: Value) -> String {
        dispatch(op, params).expect("dispatch op")
    }

    fn parse(json: &str) -> Value {
        serde_json::from_str(json).expect("response JSON")
    }

    #[test]
    fn should_skip_op_keep_round_trip() {
        let resp = run(
            "should_skip",
            serde_json::json!({
                "samples": 16_000,
                "min_record_seconds": 0.5,
            }),
        );
        let v = parse(&resp);
        assert_eq!(v["decision"], "keep");
        assert!(v["reason"].is_null());
        assert!(v["hint"].is_null());
    }

    #[test]
    fn should_skip_op_too_short_round_trip() {
        let resp = run(
            "should_skip",
            serde_json::json!({
                "samples": 1000,
                "min_record_seconds": 0.5,
            }),
        );
        let v = parse(&resp);
        assert_eq!(v["decision"], "too_short");
        assert_eq!(v["reason"], "too_short");
        assert!(v["hint"].as_str().unwrap().contains("too short"));
    }

    #[test]
    fn should_skip_op_tolerates_legacy_parakeet_params() {
        // Wave 8 of #348: a transitional Python client may still send
        // recording_s/parakeet_min_seconds/backend. They must be ignored
        // (not parsed as required) so the op stays backwards-compatible
        // until the Python caller is updated in the same release.
        let resp = run(
            "should_skip",
            serde_json::json!({
                "samples": 16_000,
                "min_record_seconds": 0.5,
                "recording_s": 1.0,
                "parakeet_min_seconds": 1.5,
                "backend": "parakeet",
            }),
        );
        let v = parse(&resp);
        // Decision is now backend-agnostic — generic too-short gate only.
        assert_eq!(v["decision"], "keep");
    }

    #[test]
    fn changed_restart_keys_op_reports_changes() {
        let resp = run(
            "changed_restart_keys",
            serde_json::json!({
                "before": {"model": "tiny", "device": "cpu"},
                "after":  {"model": "large-v3-turbo", "device": "cuda"},
            }),
        );
        let v = parse(&resp);
        let changed: Vec<String> = serde_json::from_value(v["changed"].clone()).unwrap();
        assert_eq!(changed, vec!["device", "model"]);
    }

    #[test]
    fn validate_backend_op_returns_canonical_form_and_label() {
        let resp = run(
            "validate_backend",
            serde_json::json!({"backend": "faster-whisper"}),
        );
        let v = parse(&resp);
        assert_eq!(v["backend"], "whisper");
        assert_eq!(v["label"], "Whisper");
    }

    #[test]
    fn validate_backend_op_rejects_unknown() {
        let err = dispatch("validate_backend", serde_json::json!({"backend": "groq"})).unwrap_err();
        assert!(err.to_string().contains("groq"));
    }

    #[test]
    fn is_truthy_op() {
        let v = parse(&run("is_truthy", serde_json::json!({"value": "on"})));
        assert_eq!(v["truthy"], true);
        let v = parse(&run("is_truthy", serde_json::json!({"value": "off"})));
        assert_eq!(v["truthy"], false);
        let v = parse(&run("is_truthy", serde_json::json!({})));
        assert_eq!(v["truthy"], false);
    }

    #[test]
    fn config_dump_enabled_op() {
        let v = parse(&run(
            "config_dump_enabled",
            serde_json::json!({"voicepi_debug": "1", "voicepi_stt_debug": "1"}),
        ));
        assert_eq!(v["enabled"], true);
        let v = parse(&run(
            "config_dump_enabled",
            serde_json::json!({"voicepi_debug": "1"}),
        ));
        assert_eq!(v["enabled"], false);
    }

    #[test]
    fn trace_enabled_op() {
        let v = parse(&run(
            "trace_enabled",
            serde_json::json!({"voicepi_trace": "1"}),
        ));
        assert_eq!(v["enabled"], true);
        let v = parse(&run("trace_enabled", serde_json::json!({})));
        assert_eq!(v["enabled"], false);
    }

    #[test]
    fn unknown_op_returns_error() {
        let err = dispatch("not_a_real_op", Value::Null).unwrap_err();
        assert!(err.to_string().contains("unknown dictate op"));
    }
}
