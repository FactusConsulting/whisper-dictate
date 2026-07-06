//! Second-hotkey LLM post-processing dispatcher (issue #319).
//!
//! Ships two orthogonal user-facing knobs on top of the Wave 4-B
//! [`crate::postprocess`] pipeline:
//!
//! 1. A separate hotkey binding (`postprocess_hotkey` in the settings
//!    schema) whose sole job is to fire the pipeline against the last
//!    dictated utterance — no new capture, no new STT.
//! 2. A **profile registry** (`postprocess_profiles`) the user can cycle
//!    through, so grammar-fix / formal-email / translate-to-en presets
//!    live one hotkey away instead of one Settings-tab-round-trip away.
//!
//! The three submodules are single-purpose and small:
//!
//! * [`profile`] — the `PostprocessProfile` record + normalisation.
//! * [`registry`] — cycle / activate / find-by-name over a list of
//!   profiles.
//! * [`dispatch`] — pick "last text" + run the pipeline + report the
//!   outcome.
//!
//! A hidden `postprocess-hotkey` CLI subcommand exposes the same JSON
//! envelope shape as `postprocess`, so the Python worker (or the tests)
//! can shell out to the Rust implementation without linking the library
//! statically.

mod dispatch;
mod profile;
mod registry;

use std::io::{self, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[cfg(feature = "history-sqlite")]
pub use dispatch::HistoryLastText;
pub use dispatch::{dispatch_last_text, DispatchOutcome, DispatchSource, LastTextSource};
pub use profile::{built_in_profiles, PostprocessProfile};
pub use registry::{ProfileRegistry, RegistryError};

/// Config key that stores the JSON-encoded profile list.
pub const PROFILES_CONFIG_KEY: &str = "postprocess_profiles";
/// Config key that stores the active profile index (persistent across
/// runs so the second hotkey stays on whatever the user left it on).
pub const ACTIVE_INDEX_CONFIG_KEY: &str = "postprocess_profile_index";
/// Config key that binds the second hotkey (empty = disabled).
pub const HOTKEY_CONFIG_KEY: &str = "postprocess_hotkey";

/// JSON envelope for the hidden `postprocess-hotkey` subcommand — one
/// top-level `action` discriminator, same shape the postprocess module
/// itself uses. Split off from the pipeline envelope because these
/// actions are ABOUT the profile set rather than a single call.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum HotkeyRequest {
    /// Load profiles from a JSON string (e.g. straight out of
    /// `config.json`), report their names + the resolved active index.
    LoadProfiles {
        profiles_json: String,
        #[serde(default)]
        active_index: usize,
    },
    /// Cycle to the next profile. Returns the new index + name.
    CycleNext {
        profiles_json: String,
        #[serde(default)]
        active_index: usize,
    },
    /// Cycle to the previous profile. Returns the new index + name.
    CyclePrevious {
        profiles_json: String,
        #[serde(default)]
        active_index: usize,
    },
    /// Run the pipeline against a caller-supplied fallback text using
    /// the active profile. History lookup is left to the host because
    /// the CLI runs in a fresh process without the runtime's cached
    /// SQLite connection.
    ///
    /// Codex-P2 finding on #439: `local_only` is `Option<bool>` — when
    /// the caller omits it entirely, the helper falls back to the
    /// process-wide `VOICEPI_LOCAL_ONLY` env var (see
    /// [`local_only_env_default`]) instead of defaulting to `false`.
    /// This keeps the privacy gate fail-closed: a partial dispatch
    /// envelope from a Python worker that already has
    /// `VOICEPI_LOCAL_ONLY=1` in its environment will still trip the
    /// remote-processor block inside `postprocess_text`. Callers that
    /// want to *override* the env (rare) can still pass an explicit
    /// `false`.
    Dispatch {
        profiles_json: String,
        #[serde(default)]
        active_index: usize,
        #[serde(default)]
        history_text: Option<String>,
        #[serde(default)]
        fallback_text: Option<String>,
        #[serde(default)]
        local_only: Option<bool>,
    },
}

#[derive(Debug, Serialize)]
struct ProfileSummary {
    index: usize,
    name: String,
    processor: String,
    mode: String,
}

#[derive(Debug, Serialize)]
struct ProfilesResponse {
    active_index: usize,
    profiles: Vec<ProfileSummary>,
    error: String,
}

#[derive(Debug, Serialize)]
struct CycleResponse {
    active_index: usize,
    name: String,
    error: String,
}

pub fn handle_postprocess_hotkey() -> Result<()> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    let request: HotkeyRequest = serde_json::from_str(&raw)?;
    match request {
        HotkeyRequest::LoadProfiles {
            profiles_json,
            active_index,
        } => {
            let response = load_response(&profiles_json, active_index);
            println!("{}", serde_json::to_string(&response)?);
        }
        HotkeyRequest::CycleNext {
            profiles_json,
            active_index,
        } => {
            let response = cycle_response(&profiles_json, active_index, CycleDirection::Next);
            println!("{}", serde_json::to_string(&response)?);
        }
        HotkeyRequest::CyclePrevious {
            profiles_json,
            active_index,
        } => {
            let response = cycle_response(&profiles_json, active_index, CycleDirection::Previous);
            println!("{}", serde_json::to_string(&response)?);
        }
        HotkeyRequest::Dispatch {
            profiles_json,
            active_index,
            history_text,
            fallback_text,
            local_only,
        } => {
            // Codex-P2 finding on #439: omitted `local_only` falls
            // through to the process env so the privacy gate cannot
            // be bypassed by an incomplete envelope.
            let local_only = local_only.unwrap_or_else(local_only_env_default);
            let outcome = dispatch_response(
                &profiles_json,
                active_index,
                history_text.as_deref(),
                fallback_text.as_deref(),
                local_only,
            );
            println!("{}", serde_json::to_string(&outcome)?);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CycleDirection {
    Next,
    Previous,
}

/// Fail-closed default for a `Dispatch` envelope that omitted the
/// `local_only` field: read the process-wide `VOICEPI_LOCAL_ONLY` env
/// var and treat any truthy value as "local-only".
///
/// Codex-P2 finding on #439: without this, a caller that forgot to set
/// `local_only` would send text to an OpenAI/Groq profile even under
/// `VOICEPI_LOCAL_ONLY=1`, because `postprocess_text` only enforces the
/// gate from the `PostprocessSettings.local_only` bool — not the env
/// var directly. Deriving the default from the env keeps the second-
/// hotkey helper aligned with the primary `postprocess` helper (which
/// injects the same value from the Python side's `_local_only_enabled`).
///
/// Accepts `1`, `true`, `True`, `TRUE` — matching
/// `whisper::model_manager::is_local_only` on the env-var side. The
/// helper intentionally does NOT read the persisted config here: this
/// path runs inside a helper process spawned by the Python supervisor,
/// which already reflects the config value into the env before the
/// spawn, so a second lookup would be redundant.
fn local_only_env_default() -> bool {
    std::env::var("VOICEPI_LOCAL_ONLY")
        .map(|v| matches!(v.trim(), "1" | "true" | "True" | "TRUE"))
        .unwrap_or(false)
}

fn load_registry(profiles_json: &str, active_index: usize) -> ProfileRegistry {
    match ProfileRegistry::from_json(profiles_json, active_index) {
        Ok(reg) if !reg.is_empty() => reg,
        // Fall back to the built-in seed so an empty / malformed config
        // string still yields a usable registry rather than a hard
        // error — the response's `error` field surfaces the parse
        // failure to the caller. Codex-2 finding on #439: preserve the
        // caller-supplied `active_index` here so cycling through the
        // built-in seed (Clean grammar / Email tone / Bullet list)
        // sticks across dispatches; `ProfileRegistry::new` clamps a
        // stale value to 0, so this is safe even if the built-in list
        // shrinks between releases.
        _ => ProfileRegistry::new(built_in_profiles(), active_index),
    }
}

fn load_response(profiles_json: &str, active_index: usize) -> ProfilesResponse {
    let error = ProfileRegistry::from_json(profiles_json, active_index)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    let reg = load_registry(profiles_json, active_index);
    let profiles = reg
        .profiles()
        .iter()
        .enumerate()
        .map(|(i, p)| ProfileSummary {
            index: i,
            name: p.display_name(),
            processor: p.processor.clone(),
            mode: p.mode.clone(),
        })
        .collect();
    ProfilesResponse {
        active_index: reg.active_index(),
        profiles,
        error,
    }
}

fn cycle_response(
    profiles_json: &str,
    active_index: usize,
    direction: CycleDirection,
) -> CycleResponse {
    let mut reg = load_registry(profiles_json, active_index);
    let result = match direction {
        CycleDirection::Next => reg.cycle_next(),
        CycleDirection::Previous => reg.cycle_previous(),
    };
    match result {
        Ok(new_index) => CycleResponse {
            active_index: new_index,
            name: reg.current().map(|p| p.display_name()).unwrap_or_default(),
            error: String::new(),
        },
        Err(err) => CycleResponse {
            active_index,
            name: String::new(),
            error: err.to_string(),
        },
    }
}

fn dispatch_response(
    profiles_json: &str,
    active_index: usize,
    history_text: Option<&str>,
    fallback_text: Option<&str>,
    local_only: bool,
) -> DispatchOutcome {
    // Codex-P2 finding on #439: if the caller-supplied
    // `postprocess_profiles` is non-empty but fails to parse, do NOT
    // silently substitute the built-in "Clean grammar" seed — that
    // would run a completely different processor/mode than the user
    // configured (a single typo in the JSON would silently switch the
    // dispatch target). Surface the parse error via
    // `DispatchOutcome.result.error` with `fallback=true` and
    // `source=None` so the caller can toast "malformed
    // postprocess_profiles" instead of getting a mystery translation.
    if !profiles_json.trim().is_empty() {
        if let Err(parse_err) = ProfileRegistry::from_json(profiles_json, active_index) {
            return malformed_profiles_outcome(parse_err.to_string());
        }
    }
    let reg = load_registry(profiles_json, active_index);
    let profile = reg
        .current()
        .cloned()
        .unwrap_or_else(|_| PostprocessProfile::default());
    let history_owned = history_text.map(str::to_owned);
    let source = || history_owned.clone();
    dispatch_last_text(&profile, &source, fallback_text, local_only)
}

/// Build a `DispatchOutcome` reporting a malformed-config parse error
/// without ever running the pipeline. Used by [`dispatch_response`] to
/// avoid substituting a built-in profile when the user's config is bad
/// (Codex-P2 finding on #439). Fields are chosen so the JSON envelope
/// matches the successful-dispatch shape — callers only need to check
/// `result.error` / `result.fallback` to distinguish the two.
fn malformed_profiles_outcome(error: String) -> DispatchOutcome {
    use crate::postprocess::PostprocessResult;
    DispatchOutcome {
        source: DispatchSource::None,
        profile_name: String::new(),
        result: PostprocessResult {
            text: String::new(),
            raw_text: String::new(),
            changed: false,
            provider: String::new(),
            mode: String::new(),
            model: String::new(),
            latency_ms: 0,
            fallback: true,
            error: format!("malformed postprocess_profiles: {error}"),
            redacted: false,
            redactions: Vec::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_profiles_json() -> String {
        // Two profiles: a passthrough grammar-fix and a bullets preset —
        // both processor="none" so unit tests do not need an LLM.
        r#"[
            {"name":"Grammar","processor":"none","mode":"raw"},
            {"name":"Bullets","processor":"none","mode":"raw"}
        ]"#
        .to_owned()
    }

    #[test]
    fn load_response_reports_profiles_and_resolved_index() {
        let resp = load_response(&sample_profiles_json(), 1);
        assert!(resp.error.is_empty());
        assert_eq!(resp.active_index, 1);
        assert_eq!(resp.profiles.len(), 2);
        assert_eq!(resp.profiles[0].name, "Grammar");
        assert_eq!(resp.profiles[1].name, "Bullets");
    }

    #[test]
    fn load_response_falls_back_to_built_in_when_json_is_empty() {
        let resp = load_response("", 0);
        assert!(resp.error.is_empty());
        assert!(!resp.profiles.is_empty());
        assert_eq!(resp.profiles[0].name, "Clean grammar");
    }

    #[test]
    fn load_response_reports_error_for_malformed_json_but_still_returns_seed() {
        let resp = load_response("[[[not json", 0);
        assert!(!resp.error.is_empty(), "malformed JSON must surface error");
        // ... AND the response still has a usable profile list because
        // we degrade to the built-in seed rather than crash the caller.
        assert!(!resp.profiles.is_empty());
    }

    #[test]
    fn cycle_next_response_advances_active_index() {
        let resp = cycle_response(&sample_profiles_json(), 0, CycleDirection::Next);
        assert!(resp.error.is_empty());
        assert_eq!(resp.active_index, 1);
        assert_eq!(resp.name, "Bullets");
    }

    #[test]
    fn cycle_previous_wraps_around_from_zero() {
        let resp = cycle_response(&sample_profiles_json(), 0, CycleDirection::Previous);
        assert!(resp.error.is_empty());
        assert_eq!(resp.active_index, 1);
        assert_eq!(resp.name, "Bullets");
    }

    #[test]
    fn dispatch_response_prefers_history_text_over_fallback() {
        let outcome = dispatch_response(
            &sample_profiles_json(),
            0,
            Some("from history"),
            Some("from clipboard"),
            false,
        );
        assert_eq!(outcome.source, DispatchSource::History);
        assert_eq!(outcome.result.text, "from history");
        assert_eq!(outcome.profile_name, "Grammar");
    }

    #[test]
    fn dispatch_response_falls_back_to_clipboard_when_no_history_text() {
        let outcome = dispatch_response(
            &sample_profiles_json(),
            0,
            None,
            Some("from clipboard"),
            false,
        );
        assert_eq!(outcome.source, DispatchSource::Fallback);
        assert_eq!(outcome.result.text, "from clipboard");
    }

    #[test]
    fn dispatch_response_reports_none_when_both_texts_missing() {
        let outcome = dispatch_response(&sample_profiles_json(), 0, None, None, false);
        assert_eq!(outcome.source, DispatchSource::None);
    }

    /// Codex-2 finding on #439: when the config's `postprocess_profiles`
    /// is empty the fallback used to hard-code `active_index = 0`, so a
    /// user who cycled to `Email tone` (index 1) would silently jump
    /// back to `Clean grammar` on the next press. The fix threads the
    /// caller-supplied index through the built-in fallback; this test
    /// guards it by asserting that `load_response("", 1)` reports the
    /// second built-in profile as active.
    #[test]
    fn load_response_preserves_active_index_when_falling_back_to_builtin() {
        let resp = load_response("", 1);
        assert!(resp.error.is_empty());
        assert_eq!(
            resp.active_index, 1,
            "empty JSON must not reset the persisted active index",
        );
        // The name at index 1 in `built_in_profiles()` is "Email tone".
        assert_eq!(resp.profiles[1].name, "Email tone");
    }

    /// Companion coverage for the `load_registry` helper used on the
    /// dispatch path: an empty JSON payload must yield the built-in seed
    /// with the caller-supplied active index intact. Guarded here (not
    /// via `dispatch_response`) so the assertion does not accidentally
    /// depend on the built-in profile's Ollama endpoint being open.
    #[test]
    fn load_registry_preserves_active_index_when_falling_back_to_builtin() {
        let reg = load_registry("", 2);
        assert_eq!(reg.active_index(), 2);
        assert_eq!(reg.current().unwrap().name, "Bullet list");
    }

    /// Codex-P2 finding on #439: a malformed `postprocess_profiles`
    /// value must NOT silently swap in the built-in "Clean grammar"
    /// profile and dispatch — the user configured *some other*
    /// processor/mode, so running the built-in seed would post-process
    /// their text with unexpected settings. The fix short-circuits the
    /// dispatch and surfaces the parse error via `result.error`.
    #[test]
    fn dispatch_response_reports_parse_error_instead_of_running_builtin() {
        let outcome = dispatch_response("[[[not json", 0, Some("original text"), None, false);
        assert_eq!(outcome.source, DispatchSource::None);
        assert!(outcome.result.fallback);
        assert!(
            outcome
                .result
                .error
                .contains("malformed postprocess_profiles"),
            "expected malformed-profiles error, got: {}",
            outcome.result.error,
        );
        // The pipeline was never invoked, so the profile name is empty
        // (not "Clean grammar" — which is what the pre-fix behaviour
        // would have returned).
        assert_eq!(outcome.profile_name, "");
        assert_eq!(outcome.result.provider, "");
        assert_eq!(outcome.result.mode, "");
    }

    /// Companion: an *empty* profiles JSON still falls through to the
    /// built-in seed (that is a first-run affordance, not a parse
    /// error). Only a non-empty malformed value trips the new guard.
    /// Codex-P2 finding on #439: an envelope that omits `local_only`
    /// must fall back to `VOICEPI_LOCAL_ONLY=1` (fail-closed), not to
    /// `false`. This guards the wire-level shape by round-tripping a
    /// minimal Dispatch payload through `serde_json` and asserting the
    /// helper picks up the env value.
    #[test]
    fn dispatch_envelope_defaults_local_only_from_env_when_omitted() {
        use crate::test_env_lock::ENV_LOCK;
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("VOICEPI_LOCAL_ONLY");
        std::env::set_var("VOICEPI_LOCAL_ONLY", "1");

        // Deserialize a Dispatch envelope that OMITS local_only entirely.
        let raw = r#"{
            "action":"dispatch",
            "profiles_json":"[{"name":"P","processor":"openai","mode":"clean","base_url":"https://api.openai.com/v1"}]",
            "active_index":0,
            "fallback_text":"hello"
        }"#;
        let request: HotkeyRequest = serde_json::from_str(raw).unwrap();
        let (local_only_out, _profile_json, _active, _hist, _fallback) = match request {
            HotkeyRequest::Dispatch {
                profiles_json,
                active_index,
                history_text,
                fallback_text,
                local_only,
            } => (
                local_only.unwrap_or_else(local_only_env_default),
                profiles_json,
                active_index,
                history_text,
                fallback_text,
            ),
            _ => panic!("expected Dispatch variant"),
        };

        match prev {
            Some(v) => std::env::set_var("VOICEPI_LOCAL_ONLY", v),
            None => std::env::remove_var("VOICEPI_LOCAL_ONLY"),
        }

        assert!(
            local_only_out,
            "omitted local_only in a VOICEPI_LOCAL_ONLY=1 env must fail closed to true",
        );
    }

    /// Companion: when the env var is unset, an omitted `local_only`
    /// still resolves to `false` — the fail-closed default only bites
    /// when the user actually opted in.
    #[test]
    fn dispatch_envelope_defaults_local_only_to_false_when_env_unset() {
        use crate::test_env_lock::ENV_LOCK;
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("VOICEPI_LOCAL_ONLY");
        std::env::remove_var("VOICEPI_LOCAL_ONLY");

        let got = local_only_env_default();

        match prev {
            Some(v) => std::env::set_var("VOICEPI_LOCAL_ONLY", v),
            None => std::env::remove_var("VOICEPI_LOCAL_ONLY"),
        }

        assert!(!got, "unset env must not force local-only");
    }

    #[test]
    fn dispatch_response_still_falls_back_to_builtin_when_json_is_empty() {
        let outcome = dispatch_response("", 0, Some("hi"), None, false);
        // We do not assert on the specific built-in name / mode here to
        // avoid coupling this test to the built-in seed's Ollama
        // endpoint (which is unreachable in unit tests). What matters is
        // that we did NOT go down the malformed-profiles path.
        assert!(
            outcome.result.error.is_empty()
                || !outcome
                    .result
                    .error
                    .contains("malformed postprocess_profiles"),
            "empty JSON must not surface the malformed-profiles error, got: {}",
            outcome.result.error
        );
    }
}
