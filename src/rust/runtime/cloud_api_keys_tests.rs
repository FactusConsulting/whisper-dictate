//! Unit tests for [`super`] -- the cloud API-key wiring.
//!
//! Separate file per the repo convention (`*_tests.rs` alongside the module),
//! which the AGENTS.md test-discipline scanner also looks for.

use super::super::worker_command::default_worker_command;
use super::{
    cloud_api_key_env_additions, effective_endpoint, post_credential_and_endpoint_with,
    post_credential_for, post_credential_with, stamp_post_api_key_endpoint_marker,
    stt_credential_for,
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
        Some("https://api.groq.com/openai/v1".to_owned()),
    );
    assert_eq!(
        names(&got),
        vec![
            "VOICEPI_STT_API_KEY",
            "VOICEPI_POST_API_KEY",
            "VOICEPI_POST_API_KEY_ENDPOINT",
        ]
    );
    assert_eq!(got[0].1, "stt-from-store");
    // The marker records the endpoint the post credential was resolved for,
    // so the worker can refuse to send that key to a different provider
    // after a live `post_processor` / `post_base_url` change (Codex P1 #642).
    assert_eq!(got[2].1, "https://api.groq.com/openai/v1");
}

#[test]
fn ambient_environment_wins_over_the_store() {
    let got = cloud_api_key_env_additions(
        &[],
        |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
        Some("from-store".to_owned()),
        None,
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
        None,
    );
    assert_eq!(names(&got), vec!["VOICEPI_STT_API_KEY"]);
}

#[test]
fn a_key_already_on_the_command_is_left_alone() {
    let existing = vec![("VOICEPI_STT_API_KEY".to_owned(), "caller".to_owned())];
    let got =
        cloud_api_key_env_additions(&existing, none, Some("from-store".to_owned()), None, None);
    assert!(
        got.is_empty(),
        "must not duplicate an existing entry: {got:?}"
    );
}

#[test]
fn unresolvable_keys_add_nothing() {
    // A local-Whisper user has no cloud key at all; the worker must not
    // be handed an empty variable that looks configured.
    assert!(cloud_api_key_env_additions(&[], none, None, None, None).is_empty());
}

#[test]
fn the_two_keys_are_decided_independently() {
    // STT exported, post only in the store: exactly one addition.
    let got = cloud_api_key_env_additions(
        &[],
        |name| (name == "VOICEPI_STT_API_KEY").then(|| "from-env".to_owned()),
        Some("stt-store".to_owned()),
        Some("post-store".to_owned()),
        Some("https://api.openai.com/v1".to_owned()),
    );
    assert_eq!(
        names(&got),
        vec!["VOICEPI_POST_API_KEY", "VOICEPI_POST_API_KEY_ENDPOINT"]
    );
    // The marker travels with the injected post key -- STT injection is
    // decided independently and never emits it.
    assert_eq!(got[1].1, "https://api.openai.com/v1");
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

#[test]
fn post_credential_lookup_receives_the_normalised_endpoint() {
    // Observes the CHANGED path: capture the endpoint `post_credential_for`
    // actually hands the store. A test that only exercised
    // `normalized_base_url` would stay green if the production call were
    // reverted -- that helper is already covered in `postprocess::settings`
    // -- and the saved-key failure would come straight back.
    use std::cell::RefCell;
    let seen = RefCell::new(Vec::<String>::new());
    let spy = |endpoint: &str| -> Option<String> {
        seen.borrow_mut().push(endpoint.to_owned());
        Some("key".to_owned())
    };

    // A cloud post-processor with the URL left at the schema's local Ollama
    // default is the DEFAULT path: the UI does not rewrite the URL when the
    // processor changes.
    assert_eq!(
        post_credential_with("groq", "http://localhost:11434", spy),
        Some("key".to_owned())
    );
    assert_eq!(
        seen.borrow().last().map(String::as_str),
        Some("https://api.groq.com/openai/v1"),
        "the store must be queried for the endpoint the WORKER resolves"
    );

    assert!(post_credential_with("openai", "", spy).is_some());
    assert_eq!(
        seen.borrow().last().map(String::as_str),
        Some("https://api.openai.com/v1")
    );

    // A genuinely custom endpoint is passed through untouched -- a saved
    // provider key must never be offered to a self-hosted host.
    assert!(post_credential_with("groq", "https://llm.internal.example/v1", spy).is_some());
    assert_eq!(
        seen.borrow().last().map(String::as_str),
        Some("https://llm.internal.example/v1")
    );

    // Local processors never reach the store at all.
    let before = seen.borrow().len();
    assert!(post_credential_with("ollama", "http://localhost:11434", spy).is_none());
    assert!(post_credential_with("none", "https://api.groq.com/openai/v1", spy).is_none());
    assert_eq!(
        seen.borrow().len(),
        before,
        "local processors must not query the store"
    );
}

#[test]
fn post_credential_reports_the_normalised_endpoint_alongside_the_key() {
    // Codex P1 #642: the launcher must stamp the endpoint it resolved the key
    // for so the worker can refuse to send that key to a different endpoint
    // after a live setting change. Groq processor + default Ollama URL is the
    // DEFAULT-config path the finding calls out.
    let (key, endpoint) =
        post_credential_and_endpoint_with("groq", "http://localhost:11434", |_| {
            Some("groq-key".to_owned())
        });
    assert_eq!(key.as_deref(), Some("groq-key"));
    assert_eq!(
        endpoint.as_deref(),
        Some("https://api.groq.com/openai/v1"),
        "endpoint must be the NORMALISED URL the resolver was handed"
    );
}

#[test]
fn post_credential_reports_no_endpoint_when_no_key_resolved() {
    // A missing key must NOT stamp a marker: a stale marker without a
    // matching key would block a user who later exports
    // `VOICEPI_POST_API_KEY` for the SAME endpoint from the shell.
    let (key, endpoint) = post_credential_and_endpoint_with("groq", "", |_| None);
    assert!(key.is_none());
    assert!(endpoint.is_none(), "no key => no marker: {endpoint:?}");
}

#[test]
fn post_credential_reports_no_endpoint_for_local_processors() {
    // ollama / none must not stamp a marker either -- they never hit the
    // credential store, so there is no endpoint to record.
    let (key, endpoint) =
        post_credential_and_endpoint_with("ollama", "http://localhost:11434", |_| {
            Some("should-not-be-used".to_owned())
        });
    assert!(key.is_none());
    assert!(endpoint.is_none());
}

#[test]
fn endpoint_marker_is_only_emitted_when_the_launcher_injects_the_post_key() {
    // Sanity: the marker is skipped when the caller/env already has
    // VOICEPI_POST_API_KEY set -- that user owns the resolution and no
    // launcher-side marker applies to their key.
    let existing = vec![("VOICEPI_POST_API_KEY".to_owned(), "user-set".to_owned())];
    let got = cloud_api_key_env_additions(
        &existing,
        none,
        None,
        Some("from-store".to_owned()),
        Some("https://api.groq.com/openai/v1".to_owned()),
    );
    assert!(
        got.is_empty(),
        "user-set post key must NOT get a launcher marker: {got:?}"
    );
}

#[test]
fn existing_endpoint_marker_on_the_command_wins() {
    // If the caller has already stamped VOICEPI_POST_API_KEY_ENDPOINT (e.g. a
    // nested spawn / test harness), the launcher must not clobber it. The
    // key is still injected -- but its ownership marker stays with whoever
    // placed it there first.
    let existing = vec![(
        "VOICEPI_POST_API_KEY_ENDPOINT".to_owned(),
        "https://custom.example/v1".to_owned(),
    )];
    let got = cloud_api_key_env_additions(
        &existing,
        none,
        None,
        Some("from-store".to_owned()),
        Some("https://api.groq.com/openai/v1".to_owned()),
    );
    assert_eq!(
        names(&got),
        vec!["VOICEPI_POST_API_KEY"],
        "endpoint marker on the caller command wins: {got:?}"
    );
}

#[test]
fn stamp_marker_shim_covers_ui_worker_command_post_processor_cloud() {
    // Codex P1 #666 #1 (`PRRT_kwDOSfNjQs6UXpn-`): the UI Start button
    // builds the worker command directly and used to push the post key
    // without stamping the endpoint marker -- exactly the leak the P1
    // #642 fix was supposed to close for the shipping default path.
    // The shim replicates the launcher's stamping rule so both entry
    // points behave the same.
    let mut command = default_worker_command();
    command
        .env
        .push(("VOICEPI_POST_API_KEY".to_owned(), "groq-key".to_owned()));
    stamp_post_api_key_endpoint_marker(
        &mut command,
        "groq",
        "https://api.groq.com/openai/v1",
        "whisper",
        "",
    );
    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(marker, Some("https://api.groq.com/openai/v1"));
}

#[test]
fn stamp_marker_shim_uses_stt_endpoint_for_stt_as_post_fallback() {
    // UI variant of the STT-fallback case (Codex P1 #666 #2). When the
    // UI pushes only the STT key (cloud STT + local post-processor at
    // spawn), the shim must still stamp the marker with the STT endpoint
    // so a later live change to a cloud post-processor is guarded.
    let mut command = default_worker_command();
    command
        .env
        .push(("VOICEPI_STT_API_KEY".to_owned(), "groq-stt".to_owned()));
    stamp_post_api_key_endpoint_marker(
        &mut command,
        "none", // post is local at spawn
        "http://localhost:11434",
        "openai", // stt is cloud
        "https://api.groq.com/openai/v1",
    );
    let marker = command
        .env
        .iter()
        .find(|(k, _)| k == "VOICEPI_POST_API_KEY_ENDPOINT")
        .map(|(_, v)| v.as_str());
    assert_eq!(marker, Some("https://api.groq.com/openai/v1"));
}

#[test]
fn stamp_marker_shim_no_op_when_neither_key_is_present() {
    // Local-Whisper install path: no keys, no marker. The shim must not
    // add a spurious marker or the worker will refuse to run cloud calls
    // it never intended to make.
    let mut command = default_worker_command();
    let before = command.env.len();
    stamp_post_api_key_endpoint_marker(
        &mut command,
        "none",
        "http://localhost:11434",
        "whisper",
        "",
    );
    assert_eq!(command.env.len(), before);
}

#[test]
fn stamp_marker_shim_leaves_existing_marker_alone() {
    // If a caller has already stamped a marker (nested spawn, test
    // harness), the shim must not clobber it. The launcher's own marker
    // rule follows the same "caller ownership wins" pattern.
    let mut command = default_worker_command();
    command
        .env
        .push(("VOICEPI_POST_API_KEY".to_owned(), "groq-key".to_owned()));
    command.env.push((
        "VOICEPI_POST_API_KEY_ENDPOINT".to_owned(),
        "https://custom.example/v1".to_owned(),
    ));
    stamp_post_api_key_endpoint_marker(
        &mut command,
        "groq",
        "https://api.groq.com/openai/v1",
        "whisper",
        "",
    );
    let markers: Vec<&str> = command
        .env
        .iter()
        .filter_map(|(k, v)| (k == "VOICEPI_POST_API_KEY_ENDPOINT").then_some(v.as_str()))
        .collect();
    assert_eq!(
        markers,
        vec!["https://custom.example/v1"],
        "caller-owned marker must not be duplicated / overwritten"
    );
}

#[test]
fn stt_only_injection_still_stamps_the_endpoint_marker() {
    // Codex P1 #666 #2 (`PRRT_kwDOSfNjQs6UXpnu`): when only the STT key
    // is injected (post_processor=`none`/`ollama` at spawn), both settings
    // loaders accept `VOICEPI_STT_API_KEY` as a post-key fallback. After a
    // live change to a cloud post-processor, that STT key becomes the
    // post bearer -- with no marker under the previous logic the check
    // was skipped and the leak stood. The marker must therefore also be
    // emitted for the STT injection so the fallback is guarded.
    let got = cloud_api_key_env_additions(
        &[],
        none,
        Some("stt-from-store".to_owned()),
        None, // no post key
        Some("https://api.groq.com/openai/v1".to_owned()),
    );
    assert_eq!(
        names(&got),
        vec!["VOICEPI_STT_API_KEY", "VOICEPI_POST_API_KEY_ENDPOINT"],
        "STT-only injection must still stamp the marker so the STT-as-post \
         fallback is guarded after a live change: {got:?}"
    );
    assert_eq!(got[1].1, "https://api.groq.com/openai/v1");
}

#[test]
fn stt_only_injection_omits_marker_when_no_endpoint_supplied() {
    // Symmetry with the post-key case: no endpoint => no marker => backward
    // compatible. A local-Whisper spawn injects nothing and there is
    // nothing to guard.
    let got = cloud_api_key_env_additions(&[], none, Some("stt-from-store".to_owned()), None, None);
    assert_eq!(names(&got), vec!["VOICEPI_STT_API_KEY"]);
}

#[test]
fn ambient_endpoint_marker_is_left_alone() {
    // Same for a marker exported into the ambient environment -- treat it as
    // authoritative and do not overwrite it with the launcher's own
    // resolution endpoint.
    let got = cloud_api_key_env_additions(
        &[],
        |name| {
            (name == "VOICEPI_POST_API_KEY_ENDPOINT")
                .then(|| "https://ambient.example/v1".to_owned())
        },
        None,
        Some("from-store".to_owned()),
        Some("https://api.groq.com/openai/v1".to_owned()),
    );
    assert_eq!(names(&got), vec!["VOICEPI_POST_API_KEY"]);
}
