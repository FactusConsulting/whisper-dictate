//! Companion tests for [`super::SessionPostProcess`] -- the
//! [`crate::dictate::PostProcessBackend`] adapter that drives the LLM cleanup
//! pass for the in-process Rust engine.
//!
//! Split out of the inline `#[cfg(test)] mod session_backend_tests` block in
//! `mod.rs` so the regression-test-discipline scanner
//! (`src/tests/python/test_regression_test_discipline.py`) -- which resolves
//! `mod.rs` -> `mod_tests.rs` -- sees a matching companion file next to the
//! production module, and so `mod.rs` stays well under the 500-LOC ceiling.
use super::*;

fn settings(processor: &str) -> PostprocessSettings {
    // Default to a rewriting mode so `from_settings` attaches; the
    // `none`/`raw` gating is covered by dedicated tests below.
    let mut s = settings_from_env_with(|_| None);
    s.processor = processor.to_owned();
    s.mode = "clean".to_owned();
    s
}

#[test]
fn is_active_gates_on_processor_and_mode() {
    // #607: `from_settings` now always returns Self so the
    // profile-matcher can enable a `none` -> `ollama` swap mid-session.
    // The session gates the pass on `is_active` instead. Pins the
    // Python parity contract (`processor != "none" && mode != "raw"`).
    let none = SessionPostProcess::from_settings(settings("none"));
    assert!(!none.is_active(), "processor=none is inactive");

    let ollama = SessionPostProcess::from_settings(settings("ollama"));
    assert!(
        ollama.is_active(),
        "processor=ollama + mode=clean is active"
    );

    let mut raw = settings("ollama");
    raw.mode = "raw".to_owned();
    let raw = SessionPostProcess::from_settings(raw);
    assert!(
        !raw.is_active(),
        "mode=raw is inactive even with a processor"
    );
}

#[test]
fn post_process_is_passthrough_when_processor_none() {
    // A `none` processor never touches the network: `post_process`
    // returns the input verbatim. (The backend would normally be
    // skipped via `is_active() == false`, but constructing it directly
    // pins the passthrough contract.)
    let backend = SessionPostProcess::new(settings("none"));
    assert_eq!(
        backend.post_process("keep me exactly", "da").text,
        "keep me exactly"
    );
}

#[test]
fn apply_profile_overrides_flips_processor_and_model_and_url_for_one_utterance() {
    // A profile that carries `post_processor` /
    // `post_model` / `post_base_url` must reach the pass on the next
    // utterance. Also pins the RESET semantics: a subsequent empty
    // profile snapshot restores the base settings so per-utterance
    // overrides do not leak between presses.
    let backend = SessionPostProcess::from_settings(settings("ollama"));
    let base_url = backend.current_settings().base_url.clone();

    let mut profile = std::collections::BTreeMap::new();
    profile.insert("post_processor".to_owned(), "groq".to_owned());
    profile.insert("post_model".to_owned(), "custom-llama".to_owned());
    profile.insert(
        "post_base_url".to_owned(),
        "https://api.groq.com/openai/v1".to_owned(),
    );
    profile.insert("post_timeout_ms".to_owned(), "9000".to_owned());
    backend.apply_profile_overrides(&profile);

    let snap = backend.current_settings();
    assert_eq!(snap.processor, "groq");
    assert_eq!(snap.model, "custom-llama");
    assert_eq!(snap.base_url, "https://api.groq.com/openai/v1");
    assert_eq!(snap.timeout_ms, 9000);
    assert!(backend.is_active());

    // Empty profile map -> reset to base (no processor swap leaks).
    backend.apply_profile_overrides(&std::collections::BTreeMap::new());
    let snap = backend.current_settings();
    assert_eq!(snap.processor, "ollama");
    assert_eq!(snap.base_url, base_url);
    assert_eq!(snap.timeout_ms, settings("ollama").timeout_ms);
}

#[test]
fn apply_profile_overrides_switches_the_prompt_language() {
    // #685: the cleanup prompt now names the spoken language. A profile
    // that switches `lang` (e.g. an English work app while the base
    // config is Danish) must switch it in the pass too, or the prompt
    // would pin the WRONG language for that utterance. Reset semantics
    // apply as everywhere else: an empty profile restores the base.
    let mut base = settings("ollama");
    base.lang = "da".to_owned();
    let backend = SessionPostProcess::from_settings(base);
    assert_eq!(backend.current_settings().lang, "da");

    let mut profile = std::collections::BTreeMap::new();
    profile.insert("lang".to_owned(), " en ".to_owned());
    backend.apply_profile_overrides(&profile);
    assert_eq!(backend.current_settings().lang, "en");

    backend.apply_profile_overrides(&std::collections::BTreeMap::new());
    assert_eq!(backend.current_settings().lang, "da");
}

#[test]
fn utterance_language_wins_over_the_configured_one_in_the_prompt() {
    // The cleanup prompt must name the
    // language, sourced from the `VOICEPI_LANG` snapshot. But STT can run on
    // a different language for a single utterance -- `--lang` / a profile
    // `lang` key the transcribe backend resolved, or the language whisper
    // detected on auto-detect. Handing the pass the saved value then makes
    // the prompt ASSERT the wrong language while the transcript is in
    // another, which is worse than the silence #686 replaced. The language
    // the session hands over (`TranscribeResult::language`) must win.
    let mut base = settings("ollama");
    base.lang = "da".to_owned();
    let backend = SessionPostProcess::from_settings(base);

    let per_utterance = backend.utterance_settings("en");
    assert_eq!(
        per_utterance.lang, "en",
        "the language STT used must reach the prompt, not the saved config value"
    );
    assert!(
        crate::postprocess::build_prompt("1, 2, 3", &per_utterance.mode, &per_utterance.lang)
            .contains("the input is in en (ISO 639-1 code)"),
        "prompt must name the language STT actually used"
    );
    assert!(
        !crate::postprocess::build_prompt("1, 2, 3", &per_utterance.mode, &per_utterance.lang)
            .contains("the input is in da"),
        "prompt must not name the stale configured language"
    );

    // Whitespace-only is not a language -- treated as "nothing reported".
    assert_eq!(backend.utterance_settings("  ").lang, "da");
    // The stamp is per-utterance: the live snapshot is NOT mutated, so the
    // next utterance starts from the configured value again.
    assert_eq!(backend.current_settings().lang, "da");
}

#[test]
fn unreported_utterance_language_falls_back_to_the_configured_one() {
    // Auto-detect on a backend that surfaces no language (local whisper)
    // reports `""`. The configured hint is then the best information we
    // have -- and when that is empty too the prompt binds the reply to
    // "the same language as the input" rather than naming a code.
    let mut base = settings("ollama");
    base.lang = "da".to_owned();
    let backend = SessionPostProcess::from_settings(base);
    assert_eq!(backend.utterance_settings("").lang, "da");

    let auto = SessionPostProcess::from_settings(settings("ollama"));
    let per_utterance = auto.utterance_settings("");
    assert!(per_utterance.lang.is_empty());
    assert!(
        crate::postprocess::build_prompt("x", "clean", &per_utterance.lang)
            .contains("Language: reply in the same language as the input.")
    );
}

#[test]
fn apply_profile_overrides_enables_a_previously_disabled_backend() {
    // Session was constructed with `processor=none` (default), so
    // `is_active` starts false. A profile with `post_processor=ollama`
    // must flip it active for THIS utterance without rebuilding the
    // backend.
    let backend = SessionPostProcess::from_env();
    assert!(!backend.is_active(), "default env has processor=none");
    let mut profile = std::collections::BTreeMap::new();
    profile.insert("post_processor".to_owned(), "ollama".to_owned());
    profile.insert("post_mode".to_owned(), "clean".to_owned());
    backend.apply_profile_overrides(&profile);
    assert!(
        backend.is_active(),
        "profile must be able to enable the pass"
    );
}

#[test]
fn post_process_falls_back_to_input_on_unreachable_provider() {
    // Ollama pointed at a closed port fails fast and
    // `postprocess_text` falls back to the original text -- the seam
    // must never drop the user's dictation. Mirrors run.rs's
    // `ollama_failure_falls_back_to_original_text`.
    let mut s = settings("ollama");
    s.mode = "clean".to_owned();
    s.base_url = "http://127.0.0.1:1".to_owned();
    s.timeout_ms = 100;
    let backend = SessionPostProcess::new(s);
    let outcome = backend.post_process("dictated text", "da");
    assert_eq!(outcome.text, "dictated text");
    assert!(
        outcome.fallback,
        "unreachable provider must report fallback"
    );
    assert!(!outcome.error.is_empty());
}
