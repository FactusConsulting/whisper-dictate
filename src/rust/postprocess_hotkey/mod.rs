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
    Dispatch {
        profiles_json: String,
        #[serde(default)]
        active_index: usize,
        #[serde(default)]
        history_text: Option<String>,
        #[serde(default)]
        fallback_text: Option<String>,
        #[serde(default)]
        local_only: bool,
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

fn load_registry(profiles_json: &str, active_index: usize) -> ProfileRegistry {
    match ProfileRegistry::from_json(profiles_json, active_index) {
        Ok(reg) if !reg.is_empty() => reg,
        // Fall back to the built-in seed so an empty / malformed config
        // string still yields a usable registry rather than a hard
        // error — the response's `error` field surfaces the parse
        // failure to the caller.
        _ => ProfileRegistry::built_in(),
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
    let reg = load_registry(profiles_json, active_index);
    let profile = reg
        .current()
        .cloned()
        .unwrap_or_else(|_| PostprocessProfile::default());
    let history_owned = history_text.map(str::to_owned);
    let source = || history_owned.clone();
    dispatch_last_text(&profile, &source, fallback_text, local_only)
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
}
