//! Resolve the target text for the second hotkey and run the configured
//! postprocess profile against it (issue #319).
//!
//! Contract: on a press of the second hotkey the caller invokes
//! [`dispatch_last_text`] with:
//!
//! * the currently active [`crate::postprocess_hotkey::PostprocessProfile`],
//! * an optional "fallback" text (e.g. what the OS clipboard held at press
//!   time), and
//! * whether the current session is running under `local_only` mode.
//!
//! The dispatcher then:
//!
//! 1. Pulls the most recent utterance from the SQLite history store (when
//!    the `history-sqlite` feature is on and one exists);
//! 2. Falls back to the caller-provided text otherwise;
//! 3. Runs [`crate::postprocess::postprocess_text`] with the profile's
//!    materialised settings;
//! 4. Wraps the outcome in a [`DispatchOutcome`] that tells the caller
//!    which source text was chosen, and preserves the underlying
//!    [`crate::postprocess::PostprocessResult`] verbatim.
//!
//! The whole path is pure (no OS I/O beyond the SQLite read + the LLM
//! HTTP call the pipeline itself owns), so integration tests drive it
//! by handing in the fallback text directly and pointing the profile at
//! `processor="none"` — the pipeline echoes the source text back.

use serde::Serialize;

use crate::postprocess::{postprocess_text, PostprocessResult};
use crate::postprocess_hotkey::profile::PostprocessProfile;

/// What text the dispatcher ended up running through the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchSource {
    /// The most recent history row (SQLite store).
    History,
    /// Caller-supplied fallback (clipboard / last-inject buffer).
    Fallback,
    /// No text found anywhere — the dispatcher short-circuited with a
    /// no-op result rather than call the LLM with an empty prompt.
    None,
}

/// Result of a single hotkey press.
#[derive(Debug, Clone, Serialize)]
pub struct DispatchOutcome {
    pub source: DispatchSource,
    pub profile_name: String,
    pub result: PostprocessResult,
}

/// Fetch-the-last-transcript trait. In production this hits the SQLite
/// history store; tests inject a stub so the whole pipeline is drivable
/// without a database. Kept minimal (one method, no lifetimes) because
/// the caller only ever needs "give me the last text".
pub trait LastTextSource {
    /// Return the text of the most recent utterance, or `None` when the
    /// store is empty / disabled.
    fn last_text(&self) -> Option<String>;
}

impl<F> LastTextSource for F
where
    F: Fn() -> Option<String>,
{
    fn last_text(&self) -> Option<String> {
        (self)()
    }
}

/// Core dispatcher. Picks the source text and hands it to the pipeline
/// with the profile's normalised settings.
///
/// `fallback` is used only when `source` yields `None`. An empty final
/// text produces a [`DispatchSource::None`] outcome with a passthrough
/// (raw) result so the caller can surface a "nothing to post-process"
/// toast rather than firing an LLM call with an empty prompt.
pub fn dispatch_last_text<S: LastTextSource>(
    profile: &PostprocessProfile,
    source: &S,
    fallback: Option<&str>,
    local_only: bool,
) -> DispatchOutcome {
    let (text, chosen) = pick_source(source, fallback);
    let settings = profile.to_settings(local_only);
    if text.trim().is_empty() {
        // Skip the pipeline entirely — the postprocess module also
        // treats empty input as a passthrough, but we still want to
        // report `DispatchSource::None` so the caller knows why nothing
        // happened. Reuse the pipeline's own passthrough for the shape.
        let result = postprocess_text(&text, &settings);
        return DispatchOutcome {
            source: DispatchSource::None,
            profile_name: profile.display_name(),
            result,
        };
    }
    let result = postprocess_text(&text, &settings);
    DispatchOutcome {
        source: chosen,
        profile_name: profile.display_name(),
        result,
    }
}

fn pick_source<S: LastTextSource>(source: &S, fallback: Option<&str>) -> (String, DispatchSource) {
    if let Some(text) = source.last_text() {
        if !text.trim().is_empty() {
            return (text, DispatchSource::History);
        }
    }
    if let Some(text) = fallback {
        if !text.trim().is_empty() {
            return (text.to_owned(), DispatchSource::Fallback);
        }
    }
    (String::new(), DispatchSource::None)
}

/// Concrete [`LastTextSource`] backed by the SQLite history store. Kept
/// feature-gated because the store itself is behind the `history-sqlite`
/// cargo feature.
#[cfg(feature = "history-sqlite")]
pub struct HistoryLastText;

#[cfg(feature = "history-sqlite")]
impl LastTextSource for HistoryLastText {
    fn last_text(&self) -> Option<String> {
        use crate::history::{is_enabled, open_default, search, SearchOptions};
        // Codex-2 finding on #439: honour VOICEPI_HISTORY_DISABLED before
        // ever touching the SQLite file. A user who opted out of history
        // for privacy should not have the second hotkey silently read a
        // stale utterance from the on-disk store — fall through to the
        // caller-provided fallback text (clipboard / last-inject) instead.
        if !is_enabled() {
            return None;
        }
        let conn = open_default().ok()?;
        let hits = search(
            &conn,
            &SearchOptions {
                limit: Some(1),
                ..SearchOptions::default()
            },
        )
        .ok()?;
        hits.into_iter().next().map(|hit| hit.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postprocess_hotkey::profile::PostprocessProfile;

    fn passthrough_profile() -> PostprocessProfile {
        // `processor = "none"` short-circuits postprocess_text into a
        // passthrough — perfect for unit tests that want to assert the
        // full pipeline was invoked without spinning up an LLM server.
        PostprocessProfile {
            name: "Passthrough".to_owned(),
            processor: "none".to_owned(),
            mode: "raw".to_owned(),
            ..PostprocessProfile::default()
        }
    }

    #[test]
    fn dispatch_uses_history_source_when_available() {
        let profile = passthrough_profile();
        let source = || Some("hello from history".to_owned());
        let outcome = dispatch_last_text(&profile, &source, Some("clipboard"), false);
        assert_eq!(outcome.source, DispatchSource::History);
        assert_eq!(outcome.result.text, "hello from history");
        assert_eq!(outcome.profile_name, "Passthrough");
        assert!(!outcome.result.fallback);
    }

    #[test]
    fn dispatch_falls_back_to_clipboard_when_history_empty() {
        let profile = passthrough_profile();
        let source = || None;
        let outcome = dispatch_last_text(&profile, &source, Some("clipboard text"), false);
        assert_eq!(outcome.source, DispatchSource::Fallback);
        assert_eq!(outcome.result.text, "clipboard text");
    }

    #[test]
    fn dispatch_reports_none_when_both_sources_are_empty() {
        let profile = passthrough_profile();
        let source = || Some("   ".to_owned());
        let outcome = dispatch_last_text(&profile, &source, Some("  "), false);
        assert_eq!(outcome.source, DispatchSource::None);
        // Passthrough on empty input echoes the empty string back.
        assert!(outcome.result.text.trim().is_empty());
    }

    #[test]
    fn dispatch_reports_none_when_no_source_and_no_fallback() {
        let profile = passthrough_profile();
        let source = || None;
        let outcome = dispatch_last_text(&profile, &source, None, false);
        assert_eq!(outcome.source, DispatchSource::None);
        assert!(outcome.result.text.is_empty());
    }

    #[test]
    fn dispatch_forwards_local_only_flag_into_settings() {
        // A cloud profile against `local_only=true` must fall back
        // through the postprocess pipeline's privacy gate rather than
        // reach an LLM — that behaviour is the postprocess module's
        // responsibility and we assert here that the flag was
        // threaded through.
        let mut profile = passthrough_profile();
        profile.processor = "openai".to_owned();
        profile.mode = "clean".to_owned();
        profile.base_url = "https://api.openai.com/v1".to_owned();
        // api_key is now resolved from env at dispatch time
        // (Claude-P1/Codex-P2 finding on #439). The local_only gate
        // trips before we ever look at the key, so the test does not
        // need to plant one — but if the resolver's env-var order ever
        // changes such that this test starts hitting the wire, that is
        // a signal to add an env guard here.
        let source = || Some("hello".to_owned());
        let outcome = dispatch_last_text(&profile, &source, None, true);
        assert!(outcome.result.fallback);
        assert!(outcome.result.error.contains("VOICEPI_LOCAL_ONLY=1"));
    }

    /// Codex-2 finding on #439: with `VOICEPI_HISTORY_DISABLED=1` the
    /// SQLite-backed source must NOT open the DB — the user opted out of
    /// history for privacy, so reading old rows behind their back defeats
    /// the whole point. This test guards the early-return that lands the
    /// caller on the fallback text (clipboard/last-inject) instead of the
    /// on-disk store.
    #[cfg(feature = "history-sqlite")]
    #[test]
    fn history_last_text_returns_none_when_history_is_disabled() {
        use crate::history::HISTORY_DISABLED_ENV;
        use crate::test_env_lock::ENV_LOCK;
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os(HISTORY_DISABLED_ENV);
        std::env::set_var(HISTORY_DISABLED_ENV, "1");

        let source = HistoryLastText;
        let got = source.last_text();

        match prev {
            Some(v) => std::env::set_var(HISTORY_DISABLED_ENV, v),
            None => std::env::remove_var(HISTORY_DISABLED_ENV),
        }

        assert!(
            got.is_none(),
            "HistoryLastText must short-circuit on VOICEPI_HISTORY_DISABLED",
        );
    }

    /// Integration test: hotkey coordinator -> postprocess pipeline via a
    /// mock cloud client (an ollama URL pointed at a closed port). This
    /// mirrors the runtime path the second hotkey will follow — the
    /// coordinator emits an "activate profile + dispatch" event, the host
    /// picks up the fallback text, and the pipeline runs.
    ///
    /// The postprocess pipeline's own fallback branch reports the network
    /// error so the assertion is that the whole chain wired end-to-end
    /// WITHOUT panicking or losing the source text.
    #[test]
    fn integration_hotkey_dispatch_with_mock_cloud_client_reports_fallback() {
        let profile = PostprocessProfile {
            name: "Mock cloud".to_owned(),
            processor: "ollama".to_owned(),
            mode: "clean".to_owned(),
            model: "qwen2.5:3b".to_owned(),
            // Guaranteed-closed loopback port — the pipeline's HTTP call
            // fails synchronously and we drop into the fallback path.
            base_url: "http://127.0.0.1:1".to_owned(),
            timeout_ms: 250,
            max_input_chars: 4_000,
            max_output_chars: 4_000,
            ..PostprocessProfile::default()
        };
        let source = || Some("integration source text".to_owned());
        let outcome = dispatch_last_text(&profile, &source, None, false);
        assert_eq!(outcome.source, DispatchSource::History);
        // Original text preserved on fallback (that's the pipeline contract).
        assert_eq!(outcome.result.text, "integration source text");
        assert_eq!(outcome.result.provider, "ollama");
        assert!(
            outcome.result.fallback,
            "unreachable ollama endpoint must trigger the fallback path"
        );
        assert!(!outcome.result.error.is_empty());
    }
}
