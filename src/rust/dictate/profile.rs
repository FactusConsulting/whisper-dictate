//! Per-utterance target-profile resolver used by [`super::DictateSession`].
//!
//! Rust parity port of `vp_events._apply_profile_settings` (matcher) +
//! `vp_dictate._profiled_config` (per-`_start` call site). The matcher
//! algebra itself lives in [`crate::profiles::match_profile`] — it was
//! already ported for the Python worker to shell out to via the
//! `apply-profile` hidden CLI verb. This module owns:
//!
//! 1. The **provider trait** the session consults at the start of every
//!    utterance ([`ProfileMatcher`]).
//! 2. The **live-reloading provider** that re-reads `config.json` (through
//!    [`crate::config::load_settings`]) each utterance, so a Settings save
//!    is picked up without an app restart — mirroring the reload path
//!    Python's `_reload_live_config_if_changed` runs immediately before
//!    `_profiled_config` (`vp_dictate.py:531`).
//! 3. The **fixed provider** used by tests and by callers that already
//!    know the profile list statically (e.g. an in-process bench harness).
//!
//! The resolved [`AppliedProfile`] carries the matched profile *name*
//! (for logging + telemetry parity with the Python `[profile] active: X`
//! print) plus the merged settings dictionary. The session picks the
//! subset it directly owns (`format_command_set`, `min_record_seconds`)
//! and exposes the full map through [`super::DictateSession::active_profile`]
//! so downstream consumers (backends, post-processor) can honour the
//! remaining keys as they get wired in follow-ups.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::platform::foreground_window::WindowInfo;
use crate::profiles::match_profile;

/// One resolved match, as produced by [`ProfileMatcher::resolve`].
///
/// Kept as a plain struct rather than a wrapper around
/// [`crate::profiles::ProfileMatch`] so a follow-up can add fields
/// (matched `WindowInfo`, elapsed micros, …) without touching the
/// underlying algebra.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppliedProfile {
    /// User-supplied `name` from the matched profile, or `None` when no
    /// profile matched. `Some("unnamed")` when the profile object matched
    /// but did not carry a `name` (mirrors the underlying matcher's
    /// unnamed-profile fallback).
    pub name: Option<String>,
    /// Merged settings the profile requested for this utterance. Empty
    /// when nothing matched. Keys mirror the setting names the Python
    /// worker reads from `profiles[*].settings` (`lang`, `initial_prompt`,
    /// `format_commands`, `min_record_seconds`, …); values are the raw
    /// string form the config layer stores them in.
    pub settings: BTreeMap<String, String>,
}

impl AppliedProfile {
    /// Convenience for the "nothing matched" case, kept explicit so call
    /// sites read as intent rather than an `AppliedProfile::default()` that
    /// might get mistaken for uninitialised state.
    pub fn none() -> Self {
        Self::default()
    }

    /// True when no profile matched (no name, no settings).
    pub fn is_none(&self) -> bool {
        self.name.is_none() && self.settings.is_empty()
    }
}

/// Provider trait consulted once per utterance by [`super::DictateSession`].
///
/// Two implementations ship: [`StaticProfileMatcher`] (fixed profile list
/// stamped at construction) and [`ReloadingProfileMatcher`] (re-reads the
/// live `config.json`). Both are `Send + Sync` so the session can hold
/// them across the coordinator/sink thread boundary.
pub trait ProfileMatcher: Send + Sync {
    /// Resolve the profile that applies to `window`. A `window.is_empty()`
    /// snapshot (probe failure, Wayland, macOS, …) still calls through so
    /// a profile whose `match` block is empty (matches anything) can still
    /// fire — but the matcher itself only considers the title/process
    /// substrings the snapshot actually carries.
    fn resolve(&self, window: &WindowInfo) -> AppliedProfile;
}

// ── static (fixed profile list) ─────────────────────────────────────────

/// Matcher with a fixed profile array stamped at construction. Cheap to
/// build, deterministic — used by tests and by the CLI simulate paths that
/// already own the raw config dict.
#[derive(Debug, Clone)]
pub struct StaticProfileMatcher {
    profiles: Value,
}

impl StaticProfileMatcher {
    /// Build a matcher from an already-parsed JSON array. Non-array values
    /// are accepted (the underlying matcher treats them as "no profiles",
    /// so the utterance falls back to the default settings) so callers do
    /// not have to pre-validate the shape.
    pub fn new(profiles: Value) -> Self {
        Self { profiles }
    }

    /// Parse `profiles` from a JSON string (as stored in
    /// [`crate::config::AppSettings::profiles_json`]). A parse error yields
    /// an empty matcher rather than propagating, so a corrupted config
    /// key cannot wedge PTT.
    pub fn from_json_str(profiles_json: &str) -> Self {
        let profiles =
            serde_json::from_str::<Value>(profiles_json).unwrap_or(Value::Array(Vec::new()));
        Self::new(profiles)
    }

    /// Empty matcher — never matches anything. Useful as the default when
    /// no configuration is available (e.g. simulate-session with a scratch
    /// SessionConfig).
    pub fn empty() -> Self {
        Self::new(Value::Array(Vec::new()))
    }
}

impl ProfileMatcher for StaticProfileMatcher {
    fn resolve(&self, window: &WindowInfo) -> AppliedProfile {
        resolve_from_value(&self.profiles, window)
    }
}

// ── reloading (live config.json) ────────────────────────────────────────

/// Matcher that re-reads the user's `config.json` on every utterance so a
/// Settings save (via the egui Profiles tab or the `config set` CLI) takes
/// effect on the next PTT press without restarting the process. Mirrors
/// Python's `_reload_live_config_if_changed` -> `_profiled_config`
/// sequence in `vp_dictate._start`.
///
/// A load error (missing file, unreadable JSON) is swallowed and the
/// matcher falls back to "no profiles" for that utterance — matching the
/// Python worker's silent no-op when the config file goes missing between
/// launches. The next utterance retries the read from scratch.
#[derive(Debug, Default)]
pub struct ReloadingProfileMatcher;

impl ReloadingProfileMatcher {
    /// Build the default reloading matcher. Zero-sized so wrapping in a
    /// `Box<dyn ProfileMatcher>` costs only the vtable pointer.
    pub fn new() -> Self {
        Self
    }
}

impl ProfileMatcher for ReloadingProfileMatcher {
    fn resolve(&self, window: &WindowInfo) -> AppliedProfile {
        let profiles = match crate::config::load_settings() {
            Ok(settings) => {
                serde_json::from_str::<Value>(&settings.profiles_json).unwrap_or(Value::Null)
            }
            Err(_) => Value::Null,
        };
        resolve_from_value(&profiles, window)
    }
}

// ── shared resolver ──────────────────────────────────────────────────────

fn resolve_from_value(profiles: &Value, window: &WindowInfo) -> AppliedProfile {
    let matched = match_profile(profiles, window.title.as_deref(), window.process.as_deref());
    if matched.name.is_none() && matched.settings.is_empty() {
        return AppliedProfile::none();
    }
    AppliedProfile {
        name: matched.name,
        settings: matched.settings,
    }
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn window(title: Option<&str>, process: Option<&str>) -> WindowInfo {
        WindowInfo::new(title.map(str::to_owned), process.map(str::to_owned))
    }

    #[test]
    fn static_matcher_returns_none_for_empty_profile_list() {
        let matcher = StaticProfileMatcher::empty();
        let applied = matcher.resolve(&window(Some("Editor"), Some("code")));
        assert!(applied.is_none());
    }

    #[test]
    fn static_matcher_matches_first_profile_by_title_and_process() {
        let matcher = StaticProfileMatcher::new(serde_json::json!([
            {
                "name": "Claude terminal",
                "match": {"title": "Claude Code", "process": "WindowsTerminal"},
                "settings": {"lang": "en", "format_commands": "en"}
            }
        ]));

        let applied = matcher.resolve(&window(
            Some("Claude Code - my-repo"),
            Some("WindowsTerminal.exe"),
        ));

        assert_eq!(applied.name.as_deref(), Some("Claude terminal"));
        assert_eq!(applied.settings["lang"], "en");
        assert_eq!(applied.settings["format_commands"], "en");
    }

    #[test]
    fn first_match_wins_across_multiple_profiles() {
        // Parity with Python's `_apply_profile_settings`: the profile list
        // is scanned in order and the first hit is returned. A later
        // profile with a broader match must NOT override an earlier
        // narrower one.
        let matcher = StaticProfileMatcher::new(serde_json::json!([
            {
                "name": "narrow",
                "match": {"title": "Editor"},
                "settings": {"lang": "en"}
            },
            {
                "name": "broad",
                "match": {},
                "settings": {"lang": "da"}
            }
        ]));

        let applied = matcher.resolve(&window(Some("Editor - X"), Some("Editor.exe")));
        assert_eq!(applied.name.as_deref(), Some("narrow"));
        assert_eq!(applied.settings["lang"], "en");
    }

    #[test]
    fn empty_match_block_matches_anything() {
        // Mirrors `contains_any` in `crate::profiles`: an absent / empty
        // needle set means "match anything". A profile with `match: {}`
        // therefore fires for every utterance and can be used as a
        // per-user default.
        let matcher = StaticProfileMatcher::new(serde_json::json!([
            {"name": "default", "match": {}, "settings": {"lang": "da"}}
        ]));

        let applied = matcher.resolve(&window(Some("Anything"), Some("anything.exe")));
        assert_eq!(applied.name.as_deref(), Some("default"));
    }

    #[test]
    fn window_with_no_title_or_process_still_matches_wildcard_profile() {
        // Foreground-window probe failure (Wayland, macOS, sandbox) leaves
        // both fields None. A wildcard profile still fires; a title-keyed
        // profile does not.
        let matcher = StaticProfileMatcher::new(serde_json::json!([
            {"name": "wildcard", "match": {}, "settings": {"lang": "da"}}
        ]));
        let applied = matcher.resolve(&WindowInfo::default());
        assert_eq!(applied.name.as_deref(), Some("wildcard"));

        let matcher = StaticProfileMatcher::new(serde_json::json!([
            {"name": "narrow", "match": {"title": "Editor"}, "settings": {"lang": "en"}}
        ]));
        let applied = matcher.resolve(&WindowInfo::default());
        assert!(applied.is_none());
    }

    #[test]
    fn from_json_str_accepts_stringified_config_and_falls_back_on_parse_error() {
        let matcher = StaticProfileMatcher::from_json_str(
            r#"[{"name":"E","match":{"process":"editor"},"settings":{"lang":"en"}}]"#,
        );
        let applied = matcher.resolve(&window(None, Some("editor.exe")));
        assert_eq!(applied.name.as_deref(), Some("E"));

        // Parse error -> empty matcher, no panic.
        let matcher = StaticProfileMatcher::from_json_str("not json");
        assert!(matcher.resolve(&window(None, Some("editor.exe"))).is_none());
    }

    #[test]
    fn applied_profile_none_helper_matches_default() {
        assert_eq!(AppliedProfile::none(), AppliedProfile::default());
        assert!(AppliedProfile::none().is_none());
    }
}
