//! `whisper-dictate self-test profile-match` — run the user's configured
//! target-profile matcher against a synthetic foreground window and
//! report the result.
//!
//! ## What this catches
//!
//! Profile matching is one of the trickier per-utterance seams: the
//! matcher lives in [`crate::profiles::match_profile`], the live-reload
//! wrapper in [`crate::dictate::profile::ReloadingProfileMatcher`], and
//! the config the user actually saves comes from the egui Profiles tab.
//! Debugging "why didn't my profile fire?" today requires a real live
//! session on the actual foreground window. This verb takes a synthetic
//! `--title` / `--process` pair and reports the resolved
//! [`AppliedProfile`] so the operator can iterate on the config without
//! juggling window focus.
//!
//! ## Envelope
//!
//! ```json
//! {
//!   "kind": "profile_match_self_test",
//!   "ok": true|false,
//!   "error": null | "…",
//!   "title": "Cursor - foo.rs",
//!   "process": "cursor.exe",
//!   "matched": true|false,
//!   "profile": {
//!     "name": "…" | null,
//!     "settings": { … } | null
//!   }
//! }
//! ```
//!
//! `matched=false` is a valid answer (the user's profile list doesn't
//! cover the synthetic window). The verb only exits non-zero when the
//! config layer failed to load at all (missing file with `VOICEPI_CONFIG`
//! pointing at a bad path) — that's an operator error worth surfacing.

use serde_json::{json, Value};

use crate::dictate::profile::{
    AppliedProfile, ProfileMatcher, ReloadingProfileMatcher, StaticProfileMatcher,
};
use crate::platform::foreground_window::WindowInfo;

/// Options for [`run_profile_match_self_test`].
#[derive(Debug, Clone, Default)]
pub struct ProfileMatchOptions {
    /// Foreground-window title to match against. Empty string sends
    /// `None` through to the matcher (mirrors a Wayland / macOS probe
    /// failure) so the operator can also test "no title" profiles.
    pub title: String,
    /// Foreground-window process image name. Empty string -> `None`.
    pub process: String,
    /// Override the profiles JSON. When empty, the live matcher reads
    /// the user's `config.json` (mirrors the shipping session). When
    /// populated, a `StaticProfileMatcher` is built from the string.
    pub profiles_json_override: String,
}

/// Verb output.
#[derive(Debug, Clone)]
pub struct ProfileMatchReport {
    /// Title the matcher saw (post-empty-to-`None` normalisation).
    pub title: Option<String>,
    /// Process the matcher saw.
    pub process: Option<String>,
    /// Resolved match. `AppliedProfile::none()` when nothing matched.
    pub applied: AppliedProfile,
    /// Populated when the operator's config could not be loaded.
    pub error: Option<String>,
}

impl ProfileMatchReport {
    pub fn exit_ok(&self) -> bool {
        self.error.is_none()
    }

    /// True when the matcher found a profile (any name / any settings).
    pub fn matched(&self) -> bool {
        !self.applied.is_none()
    }

    pub fn to_json(&self) -> String {
        let profile = if self.matched() {
            let settings: serde_json::Map<String, Value> = self
                .applied
                .settings
                .iter()
                .map(|(k, v)| (k.clone(), Value::from(v.clone())))
                .collect();
            json!({
                "name": self.applied.name,
                "settings": Value::Object(settings),
            })
        } else {
            json!({
                "name": null,
                "settings": null,
            })
        };
        json!({
            "kind": "profile_match_self_test",
            "ok": self.exit_ok(),
            "error": self.error,
            "title": self.title,
            "process": self.process,
            "matched": self.matched(),
            "profile": profile,
        })
        .to_string()
    }

    pub fn to_plain(&self) -> String {
        let mut out = format!(
            "[self-test profile-match] title={:?} process={:?}\n",
            self.title, self.process
        );
        if let Some(err) = &self.error {
            out.push_str(&format!("  FAIL: {err}\n"));
            return out;
        }
        if self.matched() {
            out.push_str(&format!(
                "  matched profile={:?} settings={:?}\n",
                self.applied.name, self.applied.settings
            ));
        } else {
            out.push_str("  no profile matched\n");
        }
        out.push_str("  PASS\n");
        out
    }
}

/// Trim empty strings to `None` — the matcher treats `None` and `Some("")`
/// differently (only `None` matches wildcard profiles cleanly), so an
/// operator who did not pass `--title` gets the wildcard path.
fn optional_field(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw.to_owned())
    }
}

/// Drive the matcher.
pub fn run_profile_match_self_test(opts: ProfileMatchOptions) -> ProfileMatchReport {
    let title = optional_field(&opts.title);
    let process = optional_field(&opts.process);
    let window = WindowInfo::new(title.clone(), process.clone());

    let (applied, error) = if opts.profiles_json_override.trim().is_empty() {
        // Live path: same matcher the shipping session uses.
        let matcher = ReloadingProfileMatcher::new();
        // The reloading matcher swallows load errors internally — we
        // surface a lack of config only when `load_settings` itself
        // fails, because that's the operator-visible error class this
        // verb should trip on.
        let load_err = crate::config::load_settings().err();
        let applied = matcher.resolve(&window);
        (applied, load_err.map(|err| err.to_string()))
    } else {
        let matcher = StaticProfileMatcher::from_json_str(&opts.profiles_json_override);
        (matcher.resolve(&window), None)
    };

    ProfileMatchReport {
        title,
        process,
        applied,
        error,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_title_and_process_are_normalised_to_none() {
        // Wayland / macOS foreground probes produce `WindowInfo::default`;
        // the operator running the verb with no --title / --process
        // should see the same shape so wildcard profiles fire.
        let opts = ProfileMatchOptions {
            title: String::new(),
            process: String::new(),
            profiles_json_override: r#"[{"name":"wild","match":{},"settings":{"lang":"en"}}]"#
                .to_owned(),
        };
        let report = run_profile_match_self_test(opts);
        assert_eq!(report.title, None);
        assert_eq!(report.process, None);
        assert!(report.matched());
        assert_eq!(report.applied.name.as_deref(), Some("wild"));
    }

    #[test]
    fn static_matcher_override_bypasses_config_layer() {
        // The verb runs without touching the user's real config when the
        // caller passes an override — this is the CI-safe path.
        let opts = ProfileMatchOptions {
            title: "Cursor - my-repo".to_owned(),
            process: "cursor.exe".to_owned(),
            profiles_json_override: r#"[
                {"name":"cursor","match":{"process":"cursor"},"settings":{"lang":"en"}}
            ]"#
            .to_owned(),
        };
        let report = run_profile_match_self_test(opts);
        assert!(report.matched());
        assert_eq!(report.applied.name.as_deref(), Some("cursor"));
        assert_eq!(report.applied.settings["lang"], "en");
    }

    #[test]
    fn no_match_still_reports_ok() {
        // The verb's job is diagnosis: "no match" is a valid answer and
        // must exit 0 so the operator can inspect the JSON.
        let opts = ProfileMatchOptions {
            title: "Notepad".to_owned(),
            process: "notepad.exe".to_owned(),
            profiles_json_override:
                r#"[{"name":"cursor","match":{"process":"cursor"},"settings":{"lang":"en"}}]"#
                    .to_owned(),
        };
        let report = run_profile_match_self_test(opts);
        assert!(!report.matched());
        assert!(report.exit_ok());
    }

    #[test]
    fn report_json_shape_matches_contract() {
        let mut applied = AppliedProfile {
            name: Some("Cursor".to_owned()),
            ..Default::default()
        };
        applied.settings.insert("lang".to_owned(), "en".to_owned());
        let report = ProfileMatchReport {
            title: Some("Cursor".to_owned()),
            process: Some("cursor.exe".to_owned()),
            applied,
            error: None,
        };
        let v: Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(v["kind"], "profile_match_self_test");
        assert_eq!(v["matched"], true);
        assert_eq!(v["profile"]["name"], "Cursor");
        assert_eq!(v["profile"]["settings"]["lang"], "en");
    }

    #[test]
    fn plain_report_prints_pass_line() {
        let report = run_profile_match_self_test(ProfileMatchOptions {
            title: "Editor".to_owned(),
            process: "editor.exe".to_owned(),
            profiles_json_override: "[]".to_owned(),
        });
        assert!(report.to_plain().contains("PASS"));
    }
}
