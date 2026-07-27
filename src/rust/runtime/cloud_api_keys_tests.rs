//! Unit tests for [`super`] -- the cloud API-key wiring.
//!
//! Separate file per the repo convention (`*_tests.rs` alongside the module),
//! which the AGENTS.md test-discipline scanner also looks for.

use super::{
    cloud_api_key_env_additions, effective_endpoint, post_credential_for, stt_credential_for,
};

fn none(_: &str) -> Option<String> {
    None
}

fn names(v: &[(String, String)]) -> Vec<&str> {
    v.iter().map(|(k, _)| k.as_str()).collect()
}

#[test]
fn store_keys_are_added_when_nothing_is_set() {
    // The actual bug: key saved in the UI, no env exported, worker
    // started without it and died at startup.
    let got = cloud_api_key_env_additions(
        &[],
        none,
        Some("stt-from-store".to_owned()),
        Some("post-from-store".to_owned()),
    );
    assert_eq!(
        names(&got),
        vec!["VOICEPI_STT_API_KEY", "VOICEPI_POST_API_KEY"]
    );
    assert_eq!(got[0].1, "stt-from-store");
}

#[test]
fn ambient_environment_wins_over_the_store() {
    let got = cloud_api_key_env_additions(
        &[],
        |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
        Some("from-store".to_owned()),
        None,
    );
    assert!(
        got.is_empty(),
        "an exported key must not be overridden by the store: {got:?}"
    );
}

#[test]
fn blank_ambient_value_does_not_block_the_store() {
    // `export VOICEPI_STT_API_KEY=` is a leftover, not a choice.
    let got = cloud_api_key_env_additions(
        &[],
        |name| (name == "VOICEPI_STT_API_KEY").then(|| "   ".to_owned()),
        Some("from-store".to_owned()),
        None,
    );
    assert_eq!(names(&got), vec!["VOICEPI_STT_API_KEY"]);
}

#[test]
fn a_key_already_on_the_command_is_left_alone() {
    let existing = vec![("VOICEPI_STT_API_KEY".to_owned(), "caller".to_owned())];
    let got = cloud_api_key_env_additions(&existing, none, Some("from-store".to_owned()), None);
    assert!(
        got.is_empty(),
        "must not duplicate an existing entry: {got:?}"
    );
}

#[test]
fn unresolvable_keys_add_nothing() {
    // A local-Whisper user has no cloud key at all; the worker must not
    // be handed an empty variable that looks configured.
    assert!(cloud_api_key_env_additions(&[], none, None, None).is_empty());
}

#[test]
fn the_two_keys_are_decided_independently() {
    // STT exported, post only in the store: exactly one addition.
    let got = cloud_api_key_env_additions(
        &[],
        |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
        Some("stt-store".to_owned()),
        Some("post-store".to_owned()),
    );
    assert_eq!(names(&got), vec!["VOICEPI_POST_API_KEY"]);
}

fn env(entries: &[(&str, &str)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn effective_endpoint_prefers_the_command_env_over_the_config() {
    // The regression the P1 review flagged: the schema materialises
    // `VOICEPI_STT_BASE_URL` into `command.env` (env > config > default),
    // and the credential lookup must honour that -- otherwise a runtime
    // env override sends the worker to one provider while we hand it
    // another provider's saved key.
    let e = env(&[("VOICEPI_STT_BASE_URL", "https://api.openai.com/v1")]);
    let got = effective_endpoint(&e, "VOICEPI_STT_BASE_URL", "https://api.groq.com/openai/v1");
    assert_eq!(got, "https://api.openai.com/v1");
}

#[test]
fn effective_endpoint_falls_back_to_the_config_when_env_missing_or_blank() {
    // Nothing in command.env -> settings value wins.
    let got = effective_endpoint(
        &env(&[]),
        "VOICEPI_STT_BASE_URL",
        "https://api.groq.com/openai/v1",
    );
    assert_eq!(got, "https://api.groq.com/openai/v1");
    // Whitespace-only env value is a leftover, not an override.
    let got = effective_endpoint(
        &env(&[("VOICEPI_STT_BASE_URL", "   ")]),
        "VOICEPI_STT_BASE_URL",
        "https://api.groq.com/openai/v1",
    );
    assert_eq!(got, "https://api.groq.com/openai/v1");
}

#[test]
fn env_override_of_endpoint_reclassifies_the_provider() {
    // The end-to-end shape of the P1 finding: `Provider::from_base_url`
    // must be applied AFTER `effective_endpoint`, so the credential is
    // looked up against the endpoint the worker will actually reach.
    // Two config-vs-env combinations map to two different stored
    // accounts; the assertion is on the classification, which is what
    // decides which account is read.
    use crate::credentials::Provider;
    let e = env(&[("VOICEPI_STT_BASE_URL", "https://api.openai.com/v1")]);
    let endpoint = effective_endpoint(&e, "VOICEPI_STT_BASE_URL", "https://api.groq.com/openai/v1");
    assert_eq!(Provider::from_base_url(&endpoint), Provider::OpenAi);
    // And without the env override, we would have gone to Groq -- proving
    // the two branches actually diverge.
    let cfg_only = effective_endpoint(
        &env(&[]),
        "VOICEPI_STT_BASE_URL",
        "https://api.groq.com/openai/v1",
    );
    assert_eq!(Provider::from_base_url(&cfg_only), Provider::Groq);
}

#[test]
fn stt_credential_skipped_for_local_whisper_backend() {
    // Local Whisper has no cloud key. Even if the store WOULD return
    // something, `stt_credential_for` must not consult it -- the wiring
    // stays out of the credential store entirely.
    assert!(stt_credential_for("whisper", "https://api.groq.com/openai/v1").is_none());
    // Sanity: an unknown backend also skips (fail-closed).
    assert!(stt_credential_for("mystery", "https://api.groq.com/openai/v1").is_none());
}

#[test]
fn post_credential_skipped_for_local_post_processors() {
    // `none` and `ollama` are both local -- no cloud endpoint, no key.
    assert!(post_credential_for("none", "https://api.openai.com/v1").is_none());
    assert!(post_credential_for("ollama", "http://localhost:11434").is_none());
}

#[test]
fn effective_setting_prefers_the_command_env_over_the_config() {
    // Codex P1 #615: `attach_cloud_api_keys` must derive the effective
    // stt_backend / post_processor from `command.env` (the schema has
    // already applied env > config > default), not the raw saved
    // settings -- otherwise the credential-lookup gates in
    // `stt_credential_for` / `post_credential_for` short-circuit against
    // the config's `whisper` / `none` defaults and never touch the store.
    let e = env(&[("VOICEPI_STT_BACKEND", "openai")]);
    assert_eq!(
        super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper"),
        "openai",
        "env override must win"
    );
    let e = env(&[("VOICEPI_POST_PROCESSOR", "groq")]);
    assert_eq!(
        super::effective_setting(&e, "VOICEPI_POST_PROCESSOR", "none"),
        "groq"
    );
    // No env override -> fall back to the raw settings value.
    assert_eq!(
        super::effective_setting(&env(&[]), "VOICEPI_STT_BACKEND", "openai"),
        "openai"
    );
    // Whitespace-only env value is a leftover; the config wins.
    let e = env(&[("VOICEPI_STT_BACKEND", "   ")]);
    assert_eq!(
        super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper"),
        "whisper"
    );
}

#[test]
fn env_override_of_backend_activates_the_credential_gate() {
    // End-to-end shape of the P1 finding: the config still says
    // `stt_backend=whisper`, but the launcher sees
    // `VOICEPI_STT_BACKEND=openai` in the effective command env. The
    // effective backend must be `openai` so `stt_credential_for` opens
    // the store; using the raw settings value would keep it closed and
    // start the worker without the saved key.
    let e = env(&[("VOICEPI_STT_BACKEND", "openai")]);
    let effective = super::effective_setting(&e, "VOICEPI_STT_BACKEND", "whisper");
    assert_eq!(effective, "openai");
    // The gate itself is exercised in
    // `stt_credential_skipped_for_local_whisper_backend`; here we assert
    // the input plumbing that decides which branch that gate takes.
}
