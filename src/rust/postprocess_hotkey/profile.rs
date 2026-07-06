//! One postprocess-hotkey profile: a named preset of
//! [`crate::postprocess::PostprocessSettings`] the user cycles through with
//! the second hotkey (issue #319).
//!
//! Each profile carries a display name plus every knob the underlying
//! [`crate::postprocess::postprocess_text`] pipeline needs. Serialising the
//! whole set as JSON lets us stuff the user's presets into a single
//! `postprocess_profiles` config string — same shape the Rust and Python
//! sides both parse.

use serde::{Deserialize, Serialize};

use crate::postprocess::{
    default_base_url, normalized_base_url, normalized_model, PostprocessSettings,
    DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_POST_MODEL,
};

/// One user-visible preset for the postprocess-hotkey pipeline.
///
/// The struct is intentionally flat and JSON-shaped: users store a list of
/// these in `postprocess_profiles` and the second hotkey cycles the active
/// index. Missing fields fall back to sensible defaults so a partial JSON
/// payload still produces a usable profile (matches the tolerant defaults
/// on [`PostprocessSettings`]).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PostprocessProfile {
    /// Short human-readable label ("Grammar fix", "Formal", "Translate to
    /// English", ...). Shown in the tray / status line so the user knows
    /// which profile is currently active. Whitespace-trimmed by
    /// [`Self::normalized`]; the empty string is treated as "unnamed
    /// profile" and pretty-printed by [`Self::display_name`].
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_processor")]
    pub processor: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// Provider base URL. Empty string means "use the default for the
    /// processor" ([`default_base_url`]) — the [`Self::normalized`] helper
    /// substitutes it before we hand the profile to the pipeline.
    #[serde(default)]
    pub base_url: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default = "default_max_chars")]
    pub max_input_chars: usize,
    #[serde(default = "default_max_chars")]
    pub max_output_chars: usize,
    #[serde(default)]
    pub redact: bool,
    #[serde(default)]
    pub redact_terms: String,
}

fn default_processor() -> String {
    "ollama".to_owned()
}
fn default_mode() -> String {
    "clean".to_owned()
}
fn default_model() -> String {
    DEFAULT_OLLAMA_POST_MODEL.to_owned()
}
fn default_timeout_ms() -> u64 {
    4_000
}
fn default_max_chars() -> usize {
    4_000
}

impl Default for PostprocessProfile {
    fn default() -> Self {
        Self {
            name: "Clean grammar".to_owned(),
            processor: default_processor(),
            mode: default_mode(),
            model: default_model(),
            base_url: DEFAULT_OLLAMA_BASE_URL.to_owned(),
            timeout_ms: default_timeout_ms(),
            max_input_chars: default_max_chars(),
            max_output_chars: default_max_chars(),
            redact: false,
            redact_terms: String::new(),
        }
    }
}

/// Env-var fallback used when a profile does not carry its own API key.
/// Kept in sync with `ui::api_keys::load_post_api_key_from_env` so cloud
/// profiles pick up the same environment secrets the primary
/// post-processing path honours (VOICEPI_POST_API_KEY →
/// VOICEPI_STT_API_KEY → provider-specific).
///
/// Claude-P1 + Codex-P2 finding on #439: the previous per-profile
/// `api_key` field was persisted in plaintext inside `config.json`,
/// bypassing the OS credential store the rest of the codebase uses. The
/// field is gone; secrets flow through env vars (which the runtime
/// populates from the OS credential store during worker launch).
pub fn resolve_api_key_from_env(processor: &str) -> String {
    let candidates: &[&str] = match processor {
        "groq" => &[
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "GROQ_API_KEY",
        ],
        "openai" => &[
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "OPENAI_API_KEY",
        ],
        // Ollama and the passthrough "none" processor do not need a key
        // — the chat client only rejects on the cloud path.
        _ => return String::new(),
    };
    candidates
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

impl PostprocessProfile {
    /// Return a copy with `model` / `base_url` swapped to the processor's
    /// default when the user left the field empty or on the "wrong
    /// processor's default" — same substitution the base postprocess
    /// pipeline runs so both call sites agree on the effective settings.
    pub fn normalized(&self) -> Self {
        // Codex-P2 finding on #439: strip trailing slashes AND surrounding
        // whitespace before matching against the built-in provider defaults.
        // `normalized_base_url` uses an exact match against
        // `DEFAULT_OLLAMA_BASE_URL` / `DEFAULT_OPENAI_BASE_URL` / `GROQ_BASE_URL`,
        // so a hand-edited `http://localhost:11434/` (trailing slash) or
        // `  http://localhost:11434  ` would slip through the substitution
        // and the wrong endpoint would be called for an openai/groq profile.
        let trimmed_base_url = self.base_url.trim().trim_end_matches('/');
        let base_url = if trimmed_base_url.is_empty() {
            default_base_url(&self.processor).to_owned()
        } else {
            normalized_base_url(&self.processor, trimmed_base_url)
        };
        let model = normalized_model(&self.processor, &self.model);
        Self {
            name: self.name.trim().to_owned(),
            processor: self.processor.clone(),
            mode: self.mode.clone(),
            model,
            base_url,
            timeout_ms: self.timeout_ms,
            max_input_chars: self.max_input_chars,
            max_output_chars: self.max_output_chars,
            redact: self.redact,
            redact_terms: self.redact_terms.clone(),
        }
    }

    /// Convert to the underlying [`PostprocessSettings`] the pipeline
    /// expects. The `local_only` gate is threaded in from the caller so
    /// the same profile can be validated against the current privacy
    /// mode without baking a stale value into the profile itself.
    ///
    /// Cloud profiles resolve their API key at the very last moment via
    /// [`resolve_api_key_from_env`] so a user who keeps their secret in
    /// the OS credential store / environment (rather than in
    /// `config.json`) still gets a live key on every dispatch. Claude-P1
    /// + Codex-P2 finding on #439.
    pub fn to_settings(&self, local_only: bool) -> PostprocessSettings {
        let norm = self.normalized();
        let api_key = resolve_api_key_from_env(&norm.processor);
        PostprocessSettings {
            processor: norm.processor,
            mode: norm.mode,
            model: norm.model,
            base_url: norm.base_url,
            timeout_ms: norm.timeout_ms,
            max_input_chars: norm.max_input_chars,
            max_output_chars: norm.max_output_chars,
            api_key,
            redact: norm.redact,
            redact_terms: norm.redact_terms,
            local_only,
        }
    }

    /// User-facing label. Falls back to a synthetic
    /// `<mode> (<processor>)` when the profile has no name set — always
    /// non-empty so the tray/status UI never renders a blank chip.
    pub fn display_name(&self) -> String {
        let trimmed = self.name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
        format!("{} ({})", self.mode, self.processor)
    }
}

/// Ship the standard set of profiles a first-run user sees when the
/// second hotkey is enabled with no explicit `postprocess_profiles`
/// config. Keeps a single grammar-fix on local Ollama so nothing leaves
/// the box by default; power users can override in settings.
pub fn built_in_profiles() -> Vec<PostprocessProfile> {
    vec![
        PostprocessProfile {
            name: "Clean grammar".to_owned(),
            mode: "clean".to_owned(),
            ..PostprocessProfile::default()
        },
        PostprocessProfile {
            name: "Email tone".to_owned(),
            mode: "email".to_owned(),
            ..PostprocessProfile::default()
        },
        PostprocessProfile {
            name: "Bullet list".to_owned(),
            mode: "bullets".to_owned(),
            ..PostprocessProfile::default()
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_api::GROQ_BASE_URL;

    #[test]
    fn default_profile_uses_local_ollama_clean() {
        let profile = PostprocessProfile::default();
        assert_eq!(profile.processor, "ollama");
        assert_eq!(profile.mode, "clean");
        assert_eq!(profile.base_url, DEFAULT_OLLAMA_BASE_URL);
        assert_eq!(profile.model, DEFAULT_OLLAMA_POST_MODEL);
        assert!(!profile.redact);
        assert_eq!(profile.display_name(), "Clean grammar");
    }

    #[test]
    fn normalized_substitutes_groq_defaults_when_switching_processor() {
        let profile = PostprocessProfile {
            processor: "groq".to_owned(),
            base_url: DEFAULT_OLLAMA_BASE_URL.to_owned(),
            model: DEFAULT_OLLAMA_POST_MODEL.to_owned(),
            ..PostprocessProfile::default()
        };
        let norm = profile.normalized();
        assert_eq!(norm.base_url, GROQ_BASE_URL);
        assert_eq!(norm.model, "llama-3.1-8b-instant");
    }

    #[test]
    fn normalized_fills_empty_base_url_with_processor_default() {
        let profile = PostprocessProfile {
            processor: "groq".to_owned(),
            base_url: String::new(),
            ..PostprocessProfile::default()
        };
        assert_eq!(profile.normalized().base_url, GROQ_BASE_URL);
    }

    #[test]
    fn to_settings_threads_local_only_gate() {
        let profile = PostprocessProfile::default();
        let strict = profile.to_settings(true);
        let lax = profile.to_settings(false);
        assert!(strict.local_only);
        assert!(!lax.local_only);
        // Everything else must round-trip identically for the same profile
        // — the local-only gate is the ONLY field the caller controls.
        assert_eq!(strict.processor, lax.processor);
        assert_eq!(strict.mode, lax.mode);
        assert_eq!(strict.model, lax.model);
    }

    #[test]
    fn display_name_falls_back_to_mode_processor_pair_when_unnamed() {
        let profile = PostprocessProfile {
            name: "  ".to_owned(),
            mode: "clean".to_owned(),
            processor: "ollama".to_owned(),
            ..PostprocessProfile::default()
        };
        assert_eq!(profile.display_name(), "clean (ollama)");
    }

    #[test]
    fn built_in_profiles_covers_grammar_email_bullets() {
        let profiles = built_in_profiles();
        assert_eq!(profiles.len(), 3);
        assert!(profiles.iter().any(|p| p.mode == "clean"));
        assert!(profiles.iter().any(|p| p.mode == "email"));
        assert!(profiles.iter().any(|p| p.mode == "bullets"));
        // First-run defaults must be local-only-safe: nothing leaves the
        // box until the user explicitly picks a cloud processor.
        for p in &profiles {
            assert_eq!(p.processor, "ollama");
        }
    }

    #[test]
    fn json_round_trip_preserves_fields() {
        let profile = PostprocessProfile {
            name: "Formal".to_owned(),
            processor: "openai".to_owned(),
            mode: "email".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            timeout_ms: 12_000,
            max_input_chars: 8_000,
            max_output_chars: 8_000,
            redact: true,
            redact_terms: "Codex,Fable".to_owned(),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let round: PostprocessProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(round, profile);
    }

    #[test]
    fn partial_json_falls_back_to_defaults() {
        // Users editing config.json by hand may omit fields — the tolerant
        // defaults keep those payloads usable rather than blowing up on
        // startup.
        let profile: PostprocessProfile =
            serde_json::from_str(r#"{"name":"Just a label"}"#).unwrap();
        assert_eq!(profile.name, "Just a label");
        assert_eq!(profile.processor, "ollama");
        assert_eq!(profile.mode, "clean");
    }

    /// Claude-P1 + Codex-P2 finding on #439: an inline `api_key` in the
    /// profile JSON must never round-trip back into `config.json` /
    /// `postprocess_profiles` — cloud secrets belong in the env /
    /// OS-credential-store path, same as the primary `post_api_key`.
    /// Serde ignores unknown fields, so a legacy value is silently
    /// dropped on load, and the serialized form has no `api_key` field
    /// at all.
    #[test]
    fn json_never_persists_api_key_on_the_profile() {
        // 1. A legacy JSON payload carrying `api_key` must parse (back-
        //    compat for anyone who set it under an earlier build) and
        //    the field must NOT be surfaced on re-serialization.
        let legacy = r#"{"name":"Formal","processor":"openai","api_key":"sk-test"}"#;
        let profile: PostprocessProfile = serde_json::from_str(legacy).unwrap();
        let json = serde_json::to_string(&profile).unwrap();
        assert!(
            !json.contains("api_key"),
            "profile JSON must not carry api_key (got: {json})",
        );
        assert!(!json.contains("sk-test"));

        // 2. The registry-level round-trip via `to_json` (the shape the
        //    config.json actually stores) also drops the secret.
        let json = serde_json::to_string(&vec![profile]).unwrap();
        assert!(!json.contains("api_key"));
        assert!(!json.contains("sk-test"));
    }

    /// Codex-P2 finding on #439: `to_settings` must resolve the API key
    /// from the same env vars the primary post-processing path honours
    /// so a cloud profile without an inline secret still runs against
    /// the user's `VOICEPI_POST_API_KEY` / provider-specific env var.
    /// Verified against the OpenAI processor since that has the widest
    /// env-var fan-out.
    #[test]
    fn to_settings_resolves_openai_api_key_from_env() {
        use crate::test_env_lock::ENV_LOCK;
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<std::ffi::OsString>)> = [
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "OPENAI_API_KEY",
        ]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        std::env::set_var("VOICEPI_POST_API_KEY", "post-env-secret");

        let profile = PostprocessProfile {
            processor: "openai".to_owned(),
            ..PostprocessProfile::default()
        };
        let settings = profile.to_settings(false);

        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }

        assert_eq!(settings.api_key, "post-env-secret");
    }

    /// Sibling coverage: the ordered fallback chain
    /// (POST → STT → provider-specific) matches
    /// `ui::api_keys::load_post_api_key_from_env`, so a user who only
    /// sets `OPENAI_API_KEY` still lands a resolved key.
    #[test]
    fn resolve_api_key_from_env_falls_through_to_provider_specific() {
        use crate::test_env_lock::ENV_LOCK;
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<std::ffi::OsString>)> = [
            "VOICEPI_POST_API_KEY",
            "VOICEPI_STT_API_KEY",
            "OPENAI_API_KEY",
            "GROQ_API_KEY",
        ]
        .iter()
        .map(|k| (*k, std::env::var_os(k)))
        .collect();
        for (k, _) in &saved {
            std::env::remove_var(k);
        }
        std::env::set_var("GROQ_API_KEY", "groq-env-secret");

        let got = resolve_api_key_from_env("groq");

        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }

        assert_eq!(got, "groq-env-secret");
    }

    /// The Ollama and passthrough processors never need a key, so the
    /// env-resolver returns empty even when secrets are set — no need
    /// to fake up an unused HTTP header for a local process.
    /// Codex-P2 finding on #439: an openai profile with the model omitted
    /// (or still carrying the Ollama default) must be rewritten to the
    /// OpenAI post default before dispatch — otherwise the Chat API is
    /// called with a nonsense model id and the pipeline falls back.
    #[test]
    fn normalized_substitutes_openai_default_model_off_ollama_default() {
        let profile = PostprocessProfile {
            processor: "openai".to_owned(),
            model: DEFAULT_OLLAMA_POST_MODEL.to_owned(),
            ..PostprocessProfile::default()
        };
        assert_eq!(profile.normalized().model, "gpt-4o-mini");

        // And the same substitution when the model field was left empty
        // (partial-JSON path).
        let profile = PostprocessProfile {
            processor: "openai".to_owned(),
            model: String::new(),
            ..PostprocessProfile::default()
        };
        assert_eq!(profile.normalized().model, "gpt-4o-mini");
    }

    /// Codex-P2 finding on #439: a stale Ollama base URL with a trailing
    /// slash (e.g. `http://localhost:11434/`) or surrounding whitespace
    /// bypassed the exact-match substitution in `normalized_base_url` and
    /// dispatched an openai/groq call at the Ollama endpoint. The trim
    /// step in `normalized` guards it.
    #[test]
    fn normalized_trims_base_url_before_matching_processor_defaults() {
        let profile = PostprocessProfile {
            processor: "openai".to_owned(),
            base_url: "http://localhost:11434/".to_owned(),
            ..PostprocessProfile::default()
        };
        // Ollama base URL with trailing slash must still map to the OpenAI
        // provider default when the profile switches processor.
        assert_eq!(
            profile.normalized().base_url,
            crate::cloud_api::DEFAULT_OPENAI_BASE_URL
        );

        // Same for whitespace padding — a hand-edited config with stray
        // spaces round-trips through the substitution.
        let profile = PostprocessProfile {
            processor: "groq".to_owned(),
            base_url: "  http://localhost:11434  ".to_owned(),
            ..PostprocessProfile::default()
        };
        assert_eq!(profile.normalized().base_url, GROQ_BASE_URL);
    }

    #[test]
    fn resolve_api_key_from_env_returns_empty_for_local_processors() {
        assert!(resolve_api_key_from_env("ollama").is_empty());
        assert!(resolve_api_key_from_env("none").is_empty());
    }
}
