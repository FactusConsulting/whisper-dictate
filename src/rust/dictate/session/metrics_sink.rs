//! Metrics-JSONL sink for [`super::DictateSession`].
//!
//! Ports the metrics-file WRITE side of Python's
//! `vp_history.append_record_sinks` -- the second sink that
//! `vp_dictate._record_utterance_event` fans the utterance event out to
//! alongside the `_emit_worker_event` stdout emission and the history sink
//! (see [`super::history_sink`]). Closes parity blocker #6 for flipping
//! the default engine to Rust so a user's existing `metrics.jsonl` accepts
//! Rust-written rows without a schema break.
//!
//! # Contract
//!
//! * **Path**: [`AppSettings::metrics_jsonl`] (config key `metrics_jsonl`,
//!   env `VOICEPI_METRICS_JSONL`). Tilde-expanded (`~/foo` ->
//!   `$HOME/foo`) to match Python's `os.path.expanduser(raw_metrics_path)`
//!   in `vp_history.append_record_sinks`. There is NO platform default:
//!   Python only writes the metrics file when the user opts in by setting
//!   the path explicitly, and the Rust port mirrors that -- an unset
//!   `metrics_jsonl` means no metrics file, ever.
//!
//! * **Gate**: BOTH `settings.inject_json` (config key `json_output`,
//!   env `VOICEPI_JSON`) AND a non-empty `metrics_jsonl` are required,
//!   matching Python's `metrics_path = os.path.expanduser(raw) if
//!   json_output and raw_metrics_path else ""`. When either is off
//!   [`metrics_sink_from_settings`] returns `None` so the session pays
//!   zero per-utterance cost. The metrics file is part of the
//!   machine-readable integration surface, so it is only written when the
//!   operator has opted into structured JSON output on stdout -- a
//!   prefilled-but-unused path (the UI suggests `metrics.jsonl` next to
//!   `config.json`) therefore stays inert until "JSON output" is enabled.
//!
//! * **Schema**: the FULL utterance event, unfiltered. Unlike the history
//!   sink (which passes rows through the [`crate::telemetry::history_event`]
//!   allow-list) the metrics file preserves every field the emitter wrote,
//!   matching Python's `append_record_sinks(event, metrics_path=...)`
//!   which calls `_append_jsonl(metrics_path, event)` on the raw dict.
//!   This is deliberate: metrics is the machine-readable timing/quality
//!   feed for external tooling, so it carries everything.
//!
//! * **Errors**: non-fatal. A failed write logs one `[metrics]` warning to
//!   stderr and the session continues -- matching Python's
//!   `_record_utterance_event`, which wraps `append_record_sinks` in
//!   `try / except OSError`. A metrics-file break can never abort a
//!   dictation.

use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

/// Config-key + env-var pair for the "JSON output" gate. Kept as named
/// constants because Python's `vp_history.append_record_sinks` also
/// combines the two via `vp_config.get_value("VOICEPI_JSON")`.
const JSON_OUTPUT_KEY: &str = "json_output";
const JSON_OUTPUT_ENV: &str = "VOICEPI_JSON";
/// Config-key + env-var pair for the "metrics JSONL path" override.
const METRICS_JSONL_KEY: &str = "metrics_jsonl";
const METRICS_JSONL_ENV: &str = "VOICEPI_METRICS_JSONL";

/// Resolved metrics settings AFTER config→env→default overlay. `None`
/// on either the "json_output off" or the "path empty" branch, since
/// both gate the sink identically -- matching Python's
/// `metrics_path = os.path.expanduser(raw) if json_output and raw else ""`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveMetricsSettings {
    /// Absolute (tilde-expanded) path the metrics JSONL is written to.
    pub path: PathBuf,
}

/// Resolve the effective metrics settings the way Python does:
/// **config-file value wins first, then env var, then schema default**.
/// Mirrors `vp_config.get_value` (which is what `vp_dictate` consults
/// for the equivalent `json_output` / `metrics_jsonl` pair). The Rust
/// [`crate::config::AppSettings`] path only reads config.json, so left
/// alone it silently ignores `VOICEPI_JSON=1` /
/// `VOICEPI_METRICS_JSONL=...` set in the environment -- this helper is
/// the sinks' single overlay point. Codex P1 #606 finding 3 + 5.
///
/// Returns `None` when the metrics file must NOT be written (either
/// `json_output` is off OR `metrics_jsonl` is empty/whitespace) --
/// matching Python's gate.
pub fn effective_metrics_settings() -> Option<EffectiveMetricsSettings> {
    let raw_config = crate::config::load_raw_config().unwrap_or(serde_json::Value::Null);
    let object = raw_config.as_object();

    // json_output: config → env → default (unset).
    let json_output_raw = object
        .and_then(|obj| obj.get(JSON_OUTPUT_KEY))
        .and_then(value_as_env_string)
        .or_else(|| std::env::var(JSON_OUTPUT_ENV).ok())
        .filter(|v| !v.trim().is_empty());
    let json_output = json_output_raw.as_deref().map(is_truthy).unwrap_or(false);
    if !json_output {
        return None;
    }

    let path_raw = object
        .and_then(|obj| obj.get(METRICS_JSONL_KEY))
        .and_then(value_as_env_string)
        .or_else(|| std::env::var(METRICS_JSONL_ENV).ok())
        .filter(|v| !v.trim().is_empty())?;
    Some(EffectiveMetricsSettings {
        path: expand_user(path_raw.trim()),
    })
}

/// Mirror of Python's `_truthy` in `vp_events.py` / `vp_history.py`:
/// everything except the falsy tokens is truthy.
fn is_truthy(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "no" | "off"
    )
}

/// Normalise a JSON value the same way `config::schema::value_to_env_string`
/// does. `null` / empty-string are treated as "unset" (fall through).
fn value_as_env_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(v) if v.is_empty() => None,
        serde_json::Value::String(v) => Some(v.clone()),
        serde_json::Value::Bool(true) => Some("1".to_owned()),
        serde_json::Value::Bool(false) => Some("0".to_owned()),
        other => Some(other.to_string()),
    }
}

/// The seam the session calls on every utterance the wire layer emitted a
/// payload for. The production impl [`JsonlMetricsSink`] writes to the
/// local JSONL file; [`NoopMetricsSink`] exists for tests that want the
/// sink attached without touching disk.
pub trait MetricsSink {
    /// Record one utterance event. `event` is the full payload the
    /// worker-event emitter just wrote (already includes `ts`, `text`,
    /// `stt_backend`-family fields, plus the `post_*` and
    /// `dictionary_replacements` metadata when applicable). The
    /// implementation is responsible for the write itself; unlike
    /// [`super::history_sink::HistorySink`] the metrics file preserves
    /// every field (no allow-list filter).
    fn append(&self, event: &Value);
}

/// Writes each utterance event, unfiltered, to a JSONL file on disk. Path
/// is resolved at construction and cached; the process env / settings are
/// re-read at construction, not per utterance, so a Settings save
/// requires building a fresh session (matching the current supervisor
/// lifecycle).
pub struct JsonlMetricsSink {
    path: PathBuf,
}

impl JsonlMetricsSink {
    /// Build a sink that writes to `path`. Prefer [`metrics_sink_from_settings`]
    /// in production so the gate + path resolution match Python.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The file this sink writes to. Exposed so tests can assert
    /// [`metrics_sink_from_settings`] picked the expected path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl MetricsSink for JsonlMetricsSink {
    fn append(&self, event: &Value) {
        if let Err(err) = crate::telemetry::append_jsonl(&self.path, event) {
            // Non-fatal, matching Python's
            // `except OSError: print(f"[sinks] could not write ...")`.
            // Tag the prefix `[metrics]` (vs the history sink's
            // `[history]`) so log-scrapers can tell the two sinks apart.
            eprintln!(
                "[metrics] could not append to {}: {err}",
                self.path.display()
            );
        }
    }
}

/// Discard every event. Useful for tests that want to build a session
/// with the sink seam wired up without touching disk, and for callers
/// that want to explicitly disable metrics for a single session without
/// changing the global gate.
#[derive(Default)]
pub struct NoopMetricsSink;

impl MetricsSink for NoopMetricsSink {
    fn append(&self, _event: &Value) {}
}

/// Live-reloading metrics sink: re-reads config + env on EVERY
/// [`Self::append`], so a Settings save between utterances flips the
/// gate or picks up a new `metrics_jsonl` path on the next utterance
/// without an app restart. Mirrors Python's `vp_history.append_record_sinks`,
/// which resolves the path + gate per call. Codex P1 #606 finding 3.
pub struct ReloadingMetricsSink {
    /// Cache of the last-resolved settings so a test can inspect what
    /// the sink saw on its most recent call. `None` when the gate was
    /// off (Python's "no path" branch) at last resolution.
    last: Mutex<Option<Option<EffectiveMetricsSettings>>>,
}

impl ReloadingMetricsSink {
    /// Build a reloading sink. No settings are read until the first
    /// [`Self::append`] call.
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    /// Last-resolved settings (test hook). Outer `None` before the
    /// first `append`; inner `None` when the gate was off.
    #[cfg(test)]
    pub fn last_resolved(&self) -> Option<Option<EffectiveMetricsSettings>> {
        self.last.lock().ok().and_then(|guard| guard.clone())
    }
}

impl Default for ReloadingMetricsSink {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsSink for ReloadingMetricsSink {
    fn append(&self, event: &Value) {
        let settings = effective_metrics_settings();
        if let Ok(mut guard) = self.last.lock() {
            *guard = Some(settings.clone());
        }
        let Some(settings) = settings else {
            // Gate off, or path unset -- silently drop, matching
            // Python's `if not metrics_path and not history_out: return`
            // short-circuit on the metrics branch.
            return;
        };
        if let Err(err) = crate::telemetry::append_jsonl(&settings.path, event) {
            eprintln!(
                "[metrics] could not append to {}: {err}",
                settings.path.display()
            );
        }
    }
}

/// Resolve the production metrics sink for the session. Always returns
/// a [`ReloadingMetricsSink`] -- the sink itself re-reads config + env
/// on every [`ReloadingMetricsSink::append`] so a Settings save between
/// utterances toggles the sink on/off or repoints it to a fresh path
/// without an app restart (Python parity:
/// `vp_history.append_record_sinks` reads its knobs per call). Codex P1
/// #606 findings 3 + 5.
///
/// Callers no longer pre-check the gate here -- the reloading sink
/// short-circuits internally when the gate is off, so a session that
/// always attaches this sink still pays zero write cost when the user
/// has disabled JSON output. Returned as `Option` for API compatibility
/// with the previous "off => None" behaviour; the sole caller today
/// (`rust_session_real_backends`) always feeds it to
/// `with_optional_metrics_sink`.
pub fn metrics_sink_from_settings() -> Option<Box<dyn MetricsSink + Send>> {
    Some(Box::new(ReloadingMetricsSink::new()))
}

/// Expand a leading `~` to the user's home directory, matching Python's
/// `os.path.expanduser`. Anything without a leading `~` is returned as-is.
/// A missing `HOME`/`USERPROFILE` falls through to `.` -- the same
/// last-resort the sibling `dictionary::store::expand_user` and
/// `corpus::expand_tilde` helpers pick.
fn expand_user(raw: &str) -> PathBuf {
    if let Some(stripped) = raw.strip_prefix('~') {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let rest = stripped.trim_start_matches(['/', '\\']);
        if rest.is_empty() {
            return home;
        }
        return home.join(rest);
    }
    PathBuf::from(raw)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    #[test]
    fn jsonl_sink_appends_unfiltered_row_to_disk() {
        // The metrics sink is UNFILTERED (unlike history_sink). Every
        // field on the input event must survive to disk -- external
        // tooling relies on the machine-readable metrics file carrying
        // the full per-utterance record (timing + quality + backend
        // metadata + arbitrary future fields).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        sink.append(&json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hello world",
            "stt_backend": "whisper",
            "recording_s": 1.23,
            "compute_ms": 42,
            "language": "en",
            "audio_duration_s": 1.2,
            // Arbitrary future field -- metrics must not filter it out.
            "custom_debug_field": {"nested": true},
        }));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.ends_with('\n'), "JSONL row must end with newline");
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(row["text"], "hello world");
        assert_eq!(row["stt_backend"], "whisper");
        assert_eq!(row["recording_s"], 1.23);
        assert_eq!(row["compute_ms"], 42);
        assert_eq!(
            row["custom_debug_field"]["nested"], true,
            "metrics sink must NOT drop unknown fields (schema-open by design)"
        );
    }

    #[test]
    fn jsonl_sink_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        // Path with a not-yet-created parent -- the sink must create it,
        // matching the shared `telemetry::append_jsonl` contract.
        let path = dir.path().join("nested").join("dirs").join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());
        sink.append(&json!({"text": "hi"}));
        assert!(
            path.exists(),
            "sink must create parent directories on first write"
        );
    }

    #[test]
    fn jsonl_sink_appends_multiple_rows_preserving_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        sink.append(&json!({"text": "first"}));
        sink.append(&json!({"text": "second"}));
        sink.append(&json!({"text": "third"}));

        let raw = fs::read_to_string(&path).unwrap();
        let texts: Vec<Value> = raw
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap()["text"].clone())
            .collect();
        assert_eq!(texts, vec![json!("first"), json!("second"), json!("third")]);
    }

    #[test]
    fn jsonl_sink_write_failure_is_non_fatal() {
        // Aim the sink at a path whose parent CANNOT be created (an
        // existing regular file, so `create_dir_all` would fail). The
        // sink must swallow the error -- a dropped dictation would be
        // worse than a missing metrics row.
        let dir = tempfile::tempdir().unwrap();
        let file_as_parent = dir.path().join("not-a-dir");
        fs::write(&file_as_parent, "").unwrap();
        let path = file_as_parent.join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());
        // Must not panic / propagate.
        sink.append(&json!({"text": "should not blow up"}));
        assert!(
            !path.exists(),
            "the unwritable path should stay non-existent -- \
             the sink swallowed the error"
        );
    }

    #[test]
    fn round_trip_write_then_read_back_matches_input() {
        // Round-trip: parse the written line back and assert every input
        // field survives. Metrics has no reader like `crate::history`, so
        // the acceptance is a raw JSON round-trip: what we wrote is what a
        // JSON-parsing consumer will see.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.jsonl");
        let sink = JsonlMetricsSink::new(path.clone());

        let event = json!({
            "ts": 1706280000.5,
            "event": "utterance",
            "text": "hej verden",
            "text_chars": 10,
            "recording_s": 1.23,
            "compute_ms": 42,
            "compute_s": 0.04,
            "audio_duration_s": 1.2,
            "language": "da",
            "stt_backend": "whisper",
        });
        sink.append(&event);

        let raw = fs::read_to_string(&path).unwrap();
        let row: Value = serde_json::from_str(raw.trim()).unwrap();
        for key in [
            "ts",
            "event",
            "text",
            "text_chars",
            "recording_s",
            "compute_ms",
            "compute_s",
            "audio_duration_s",
            "language",
            "stt_backend",
        ] {
            assert_eq!(row[key], event[key], "field {key} must round-trip");
        }
    }

    #[test]
    fn noop_sink_writes_nothing() {
        // Trivially: the noop sink is a discard. Kept as an executable
        // spec so a future refactor cannot accidentally turn Noop into
        // a fallback that touches disk.
        let sink = NoopMetricsSink;
        sink.append(&json!({"text": "ignored"}));
    }

    #[test]
    fn expand_user_expands_leading_tilde() {
        // Set a fake HOME so the test is deterministic on any machine.
        // Uses a scratch dir so we don't rely on the real user's home.
        // Take the crate-wide ENV_LOCK: `set_var`/`remove_var` are
        // `unsafe` under Rust 2024 precisely because parallel tests in
        // the same binary observing HOME/USERPROFILE would race here.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let saved_home = std::env::var_os("HOME");
        let saved_userprofile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", home);
        std::env::set_var("USERPROFILE", home);

        assert_eq!(expand_user("~"), home.to_path_buf());
        assert_eq!(expand_user("~/metrics.jsonl"), home.join("metrics.jsonl"));
        assert_eq!(
            expand_user("/tmp/metrics.jsonl"),
            PathBuf::from("/tmp/metrics.jsonl")
        );

        // Restore env so sibling tests are unaffected.
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
    }

    // ── env overlay + live reload (Codex P1 #606) ──────────────────

    struct EnvSnapshot {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }
    impl EnvSnapshot {
        fn new(keys: &[&'static str]) -> Self {
            let saved = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
            Self { saved }
        }
    }
    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Config file wins over env var (Python parity: `vp_config.get_value`).
    /// Fixture:
    ///   config -> json_output=1, metrics_jsonl="/config/metrics.jsonl"
    ///   env    -> VOICEPI_JSON=0, VOICEPI_METRICS_JSONL="/env/metrics.jsonl"
    ///   want   -> Some(config path) because config's json_output=1 wins,
    ///             and config's path wins over env path.
    #[test]
    fn effective_metrics_settings_config_wins_over_env() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", JSON_OUTPUT_ENV, METRICS_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        let cfg_path = dir.path().join("config-metrics.jsonl");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "json_output": "1",
                "metrics_jsonl": cfg_path.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(JSON_OUTPUT_ENV, "0");
        std::env::set_var(METRICS_JSONL_ENV, dir.path().join("env-metrics.jsonl"));

        let resolved = effective_metrics_settings()
            .expect("config json_output=1 must beat env=0; sink must be enabled");
        assert_eq!(
            resolved.path, cfg_path,
            "config-file path must override env path"
        );
    }

    /// Env var wins over the schema default (Python parity). Fixture:
    ///   config -> {} (no keys)
    ///   env    -> VOICEPI_JSON=1, VOICEPI_METRICS_JSONL=/env/path
    ///   want   -> Some(env path) because env beats the "unset" default
    #[test]
    fn effective_metrics_settings_env_wins_over_default() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", JSON_OUTPUT_ENV, METRICS_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::set_var(JSON_OUTPUT_ENV, "1");
        let env_path = dir.path().join("env-only.jsonl");
        std::env::set_var(METRICS_JSONL_ENV, &env_path);

        let resolved = effective_metrics_settings()
            .expect("env VOICEPI_JSON=1 must beat the unset default (off)");
        assert_eq!(
            resolved.path, env_path,
            "env VOICEPI_METRICS_JSONL must beat the unset default"
        );
    }

    /// The gate is OFF by default when nothing is configured: schema
    /// `json_output` default is null (unset), so the metrics file stays
    /// inert. Codex P1 #606 finding 5 sanity check.
    #[test]
    fn effective_metrics_settings_all_unset_yields_none() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", JSON_OUTPUT_ENV, METRICS_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(JSON_OUTPUT_ENV);
        std::env::remove_var(METRICS_JSONL_ENV);

        assert!(effective_metrics_settings().is_none());
    }

    /// Reloading sink: config-flip between utterances flips the sink
    /// on / off with no session rebuild. Codex P1 #606 finding 3.
    #[test]
    fn reloading_metrics_sink_picks_up_config_change_between_appends() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _snap = EnvSnapshot::new(&["VOICEPI_CONFIG", JSON_OUTPUT_ENV, METRICS_JSONL_ENV]);

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        let metrics = dir.path().join("metrics.jsonl");

        // Round 1: enabled + path set. Env unset so config wins.
        std::fs::write(
            &cfg,
            serde_json::json!({
                "json_output": "1",
                "metrics_jsonl": metrics.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();
        std::env::set_var("VOICEPI_CONFIG", &cfg);
        std::env::remove_var(JSON_OUTPUT_ENV);
        std::env::remove_var(METRICS_JSONL_ENV);

        let sink = ReloadingMetricsSink::new();
        sink.append(&serde_json::json!({"text": "one", "event": "utterance"}));
        assert_eq!(
            std::fs::read_to_string(&metrics).unwrap().lines().count(),
            1,
            "first append lands"
        );

        // Round 2: flip json_output off between utterances. Next append is a no-op.
        std::fs::write(
            &cfg,
            serde_json::json!({
                "json_output": "0",
                "metrics_jsonl": metrics.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();

        sink.append(&serde_json::json!({"text": "two", "event": "utterance"}));
        assert_eq!(
            std::fs::read_to_string(&metrics).unwrap().lines().count(),
            1,
            "second append must skip -- gate flipped off between utterances"
        );

        // Round 3: enable again with a fresh path -- sink writes there.
        let metrics2 = dir.path().join("metrics-v2.jsonl");
        std::fs::write(
            &cfg,
            serde_json::json!({
                "json_output": "1",
                "metrics_jsonl": metrics2.to_string_lossy(),
            })
            .to_string(),
        )
        .unwrap();
        sink.append(&serde_json::json!({"text": "three", "event": "utterance"}));
        assert!(metrics2.exists(), "new path picked up on next append");
    }
}
