//! The typed [`AppSettings`] model and its defaults.
//!
//! The struct mirrors every key the app reads from / writes to config.json.
//! Loading (`from_value`), saving (`apply_to_object`), and validation live in
//! sibling modules as additional `impl AppSettings` blocks to keep each unit
//! small and focused.

use serde::{Deserialize, Serialize};

use crate::config::io::platform_config_dir;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub key: String,
    pub model: String,
    pub stt_backend: String,
    pub stt_provider: String,
    pub stt_model: String,
    pub stt_base_url: String,
    pub stt_timeout_ms: String,
    pub device: String,
    pub compute_type: String,
    pub audio_device: String,
    pub lang: String,
    pub xkb_layout: String,
    pub initial_prompt: String,
    pub inject_mode: String,
    pub format_commands: String,
    pub beam_size: String,
    pub temperature: String,
    pub context_min_seconds: String,
    pub hallucination_guard: bool,
    pub max_chars_per_second: String,
    pub min_record_seconds: String,
    pub release_tail_ms: String,
    pub preview_seconds: String,
    pub max_record_s: String,
    pub vad_threshold: String,
    pub vad_min_silence_ms: String,
    pub vad_speech_pad_ms: String,
    pub target_dbfs: String,
    pub min_input_dbfs: String,
    pub min_snr_db: String,
    pub audio_ducking: bool,
    pub audio_ducking_level: String,
    pub dictionary: String,
    pub dictionary_enabled: bool,
    pub dictionary_max_terms: String,
    pub dictionary_prompt_chars: String,
    pub inject_json: bool,
    pub metrics_jsonl: String,
    pub command_hook: String,
    pub command_hook_timeout_ms: String,
    pub history_enabled: bool,
    pub history_jsonl: String,
    pub local_only: bool,
    pub post_processor: String,
    pub post_mode: String,
    pub post_model: String,
    pub post_base_url: String,
    pub post_timeout_ms: String,
    pub post_max_input_chars: String,
    pub post_max_output_chars: String,
    pub post_redact: bool,
    pub post_redact_terms: String,
    /// Second hotkey binding for the LLM post-processing dispatcher
    /// (issue #319). Same string format as [`Self::key`]; empty disables.
    pub postprocess_hotkey: String,
    /// JSON-encoded list of postprocess profiles the second hotkey
    /// cycles through. Kept as an opaque string here so the UI and the
    /// Rust runtime can each parse it on demand with the shared
    /// [`crate::postprocess_hotkey::ProfileRegistry`] helper.
    pub postprocess_profiles: String,
    /// Persisted active-profile index. Clamped to the profile list on
    /// load so a stale value never crashes the second hotkey.
    pub postprocess_profile_index: String,
    pub feedback_sounds: bool,
    pub feedback_notify: bool,
    pub debug: bool,
    pub stt_debug: bool,
    pub trace: bool,
    pub toggle_mode: bool,
    pub quit_key: String,
    pub quit_count: String,
    pub quit_window_ms: String,
    pub update_check: bool,
    pub update_check_interval_minutes: String,
    pub update_include_prereleases: bool,
    pub ui_language: String,
    pub ui_log_view: String,
    pub ui_theme: String,
    pub ui_text_scale: String,
    /// Whether the small always-on-top recording overlay (Issue #320) appears
    /// during dictation. Stored as a typed bool but persisted through the
    /// same `"1"`/`"0"` string contract as the rest of the bool settings.
    pub overlay_enabled: bool,
    /// One of: `top-left`, `top-right`, `bottom-left`, `bottom-right`, or
    /// `custom:<x>,<y>` — see `crate::ui::overlay::position::OverlayPosition`.
    /// Anything unrecognised decodes to the default (`bottom-right`) at
    /// render time, so a hand-edited config can't crash the UI.
    pub overlay_position: String,
    /// When `true`, the overlay also shows while the worker is idle/ready —
    /// useful for a permanent meter while configuring devices; default `false`
    /// so the overlay only appears around live dictation.
    pub overlay_show_on_idle: bool,
    pub profiles_json: String,
    /// Issue #328: first-run onboarding gate. Defaults to `false` so a fresh
    /// install triggers the wizard on the first launch, then flips to `true`
    /// on either "Finish" or "Skip + don't show again". Users can re-open the
    /// wizard from the System tab, which does NOT flip this back to `false`
    /// (re-runs are always explicit user actions, not first-run detection).
    pub onboarding_completed: bool,
    /// Issue #328: RFC 3339 timestamp of the last time the user actually saw
    /// (opened) the onboarding wizard. Empty when the wizard has never been
    /// shown. Stored as a plain string to match the rest of the settings
    /// serialization contract; parsed on demand where needed.
    pub onboarding_seen_at: String,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            key: "ctrl_r".to_owned(),
            model: "large-v3-turbo".to_owned(),
            stt_backend: "whisper".to_owned(),
            stt_provider: "openai".to_owned(),
            stt_model: String::new(),
            stt_base_url: "https://api.openai.com/v1".to_owned(),
            stt_timeout_ms: "30000".to_owned(),
            device: "auto".to_owned(),
            compute_type: String::new(),
            audio_device: String::new(),
            lang: String::new(),
            xkb_layout: String::new(),
            initial_prompt: String::new(),
            inject_mode: "auto".to_owned(),
            format_commands: "off".to_owned(),
            beam_size: "1".to_owned(),
            temperature: "0.0,0.2".to_owned(),
            context_min_seconds: "5".to_owned(),
            hallucination_guard: true,
            max_chars_per_second: "30".to_owned(),
            min_record_seconds: "0.5".to_owned(),
            release_tail_ms: "200".to_owned(),
            preview_seconds: "3".to_owned(),
            max_record_s: "120".to_owned(),
            vad_threshold: "0.3".to_owned(),
            vad_min_silence_ms: "600".to_owned(),
            vad_speech_pad_ms: "200".to_owned(),
            target_dbfs: "-20".to_owned(),
            min_input_dbfs: "-55".to_owned(),
            min_snr_db: "6".to_owned(),
            audio_ducking: false,
            audio_ducking_level: "0.25".to_owned(),
            dictionary: default_dictionary_path().display().to_string(),
            dictionary_enabled: true,
            dictionary_max_terms: "80".to_owned(),
            dictionary_prompt_chars: "1200".to_owned(),
            inject_json: false,
            metrics_jsonl: String::new(),
            command_hook: String::new(),
            command_hook_timeout_ms: "2000".to_owned(),
            history_enabled: true,
            history_jsonl: String::new(),
            local_only: false,
            post_processor: "none".to_owned(),
            post_mode: "raw".to_owned(),
            post_model: "qwen2.5:3b".to_owned(),
            post_base_url: "http://localhost:11434".to_owned(),
            post_timeout_ms: "4000".to_owned(),
            post_max_input_chars: "4000".to_owned(),
            post_max_output_chars: "4000".to_owned(),
            post_redact: false,
            post_redact_terms: String::new(),
            postprocess_hotkey: String::new(),
            postprocess_profiles: String::new(),
            postprocess_profile_index: "0".to_owned(),
            feedback_sounds: false,
            feedback_notify: false,
            debug: false,
            stt_debug: false,
            trace: false,
            toggle_mode: false,
            quit_key: "esc".to_owned(),
            quit_count: "3".to_owned(),
            quit_window_ms: "1500".to_owned(),
            update_check: true,
            update_check_interval_minutes: "15".to_owned(),
            update_include_prereleases: false,
            ui_language: "en".to_owned(),
            ui_log_view: "minimal".to_owned(),
            ui_theme: "dark".to_owned(),
            ui_text_scale: "1.15".to_owned(),
            // Overlay defaults: off until the user opts in, bottom-right
            // corner so it doesn't crowd the active app's title bar, and
            // hidden while idle so the overlay only appears around live
            // dictation. See `crate::ui::overlay`.
            overlay_enabled: false,
            overlay_position: "bottom-right".to_owned(),
            overlay_show_on_idle: false,
            profiles_json: default_profiles_json(),
            // Issue #328: false on a fresh install triggers the first-run
            // wizard; users flip it to `true` by finishing / skipping it.
            onboarding_completed: false,
            onboarding_seen_at: String::new(),
        }
    }
}

/// A single, inert example profile so new users see the shape of a profile in
/// the Profiles tab. It matches a placeholder process/title, so it changes
/// nothing until edited — the names and keys just document the structure. See
/// the "Target profiles" section in docs/CONFIGURATION.md.
///
/// Built via `to_string_pretty` so it already matches the canonical form
/// `load()` reproduces (sorted keys, pretty-printed), keeping it stable across
/// a save/load round-trip.
pub(crate) fn default_profiles_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!([
        {
            "name": "Example: per-app overrides (edit the match + settings, or delete me)",
            "match": { "process": ["ExampleApp.exe"], "title": ["Example Window"] },
            "settings": { "lang": "en", "inject_mode": "paste", "post_mode": "prompt" }
        }
    ]))
    .unwrap_or_else(|_| "[]".to_owned())
}

impl AppSettings {
    /// The string view of a restart-relevant key, used to diff two snapshots.
    /// Returns `None` for keys that never trigger a restart.
    pub(crate) fn setting_value(&self, key: &str) -> Option<&str> {
        match key {
            "key" => Some(&self.key),
            "model" => Some(&self.model),
            "stt_backend" => Some(&self.stt_backend),
            "stt_provider" => Some(&self.stt_provider),
            "stt_model" => Some(&self.stt_model),
            "stt_base_url" => Some(&self.stt_base_url),
            "stt_timeout_ms" => Some(&self.stt_timeout_ms),
            "device" => Some(&self.device),
            "compute_type" => Some(&self.compute_type),
            "local_only" => Some(if self.local_only { "1" } else { "0" }),
            "toggle_mode" => Some(if self.toggle_mode { "1" } else { "0" }),
            "quit_key" => Some(&self.quit_key),
            "quit_count" => Some(&self.quit_count),
            "quit_window_ms" => Some(&self.quit_window_ms),
            "postprocess_hotkey" => Some(&self.postprocess_hotkey),
            // Iteration-3 review finding #3: not in the static
            // RESTART_KEYS table (Python backends are live-reloadable
            // for this key), but exposed here so the dynamic
            // rust-backend-aware check in `restart_required_keys` can
            // diff it. See keys.rs for the gate.
            "audio_device" => Some(&self.audio_device),
            _ => None,
        }
    }
}

pub(crate) fn default_dictionary_path() -> std::path::PathBuf {
    platform_config_dir().join("dictionary.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_use_expected_baseline_values() {
        let defaults = AppSettings::default();
        assert_eq!(defaults.model, "large-v3-turbo");
        assert_eq!(defaults.stt_backend, "whisper");
        assert_eq!(defaults.stt_provider, "openai");
        assert_eq!(defaults.ui_theme, "dark");
        assert_eq!(defaults.ui_text_scale, "1.15");
        assert!(defaults.dictionary.ends_with("dictionary.json"));
    }
}
