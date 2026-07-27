//! Cloud API-key wiring for the spawned worker.
//!
//! Split out of `runtime/mod.rs` in the 500-LOC modularity refactor, matching
//! the precedent set by `worker_command.rs`. The logic is unchanged; it lives
//! here so `mod.rs` stays under the AGENTS.md per-file limit and so the
//! credential wiring reads as one unit rather than as an aside in the middle
//! of process management.

use super::worker_command::WorkerCommand;

/// Give the worker the cloud API keys the user already saved in Settings.
///
/// Until this existed only the UI could read the credential store, so it was
/// the only entry point that could start a cloud-configured worker. A bare
/// `whisper-dictate run` -- including the terminal test documented in
/// `docs/testing-rust-engine-v1.22.md` -- died at startup with
/// "openai API requires OPENAI_API_KEY, GROQ_API_KEY, or
/// VOICEPI_STT_API_KEY/VOICEPI_POST_API_KEY" on a machine where the key was
/// saved and working in the UI.
///
/// The key travels in the child's ENVIRONMENT, never argv: a command line is
/// readable by other local users (the leak fixed in #588).
pub(super) fn attach_cloud_api_keys(command: &mut WorkerCommand) {
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        // No readable config: nothing to resolve a provider from. The worker
        // reports the missing key itself, which is a better message than
        // anything invented here.
        Err(_) => return,
    };

    // Classify the credential against the endpoint AND the effective mode the
    // WORKER will actually run in, not the raw config values.
    // `worker_env_overrides()` has already baked env-var overrides into
    // `command.env` (env > config > default), so resolving against
    // `command.env` is what keeps the credential lookup aligned with the
    // transcribe layer. Ignoring the endpoint override leads to
    // `VOICEPI_STT_BASE_URL=https://api.openai.com/v1 whisper-dictate run`
    // reaching for the Groq key saved for the config value; ignoring the
    // BACKEND override (Codex P1 #615: `VOICEPI_STT_BACKEND=openai` /
    // `VOICEPI_POST_PROCESSOR=groq` set only in the shell) makes the gates in
    // `stt_credential_for` / `post_credential_for` short-circuit against the
    // saved `whisper` / `none` defaults and never read the store at all --
    // the worker then starts without the key that was saved through Settings.
    let stt_endpoint =
        effective_endpoint(&command.env, "VOICEPI_STT_BASE_URL", &settings.stt_base_url);
    let post_endpoint = effective_endpoint(
        &command.env,
        "VOICEPI_POST_BASE_URL",
        &settings.post_base_url,
    );
    let stt_backend = effective_setting(
        &command.env,
        crate::dictate::backends::cloud_transcribe::STT_BACKEND_ENV,
        &settings.stt_backend,
    );
    let post_processor = effective_setting(
        &command.env,
        crate::postprocess::POST_PROCESSOR_ENV,
        &settings.post_processor,
    );

    let additions = cloud_api_key_env_additions(
        &command.env,
        |name| std::env::var(name).ok(),
        stt_credential_for(&stt_backend, &stt_endpoint),
        post_credential_for(&post_processor, &post_endpoint),
    );
    command.env.extend(additions);
}

/// The base URL the worker will resolve to, given the env the spawner has
/// already assembled and the config's own value. Split from
/// [`attach_cloud_api_keys`] so the precedence is unit-testable without a
/// config file or a credential store.
fn effective_endpoint(env: &[(String, String)], name: &str, config_value: &str) -> String {
    effective_setting(env, name, config_value)
}

/// Generalised env-first setting resolver: prefer a non-blank value already
/// in `command.env` (the spawner has already applied env > config > default
/// via [`crate::config::schema::worker_env_overrides`]), otherwise fall back
/// to the raw config value. Kept as a separate helper so the credential
/// wiring can look up ANY effective mode (backend, processor, base URL) with
/// the same precedence rule -- Codex P1 #615.
fn effective_setting(env: &[(String, String)], name: &str, config_value: &str) -> String {
    env.iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| config_value.to_owned())
}

/// Only fetch an STT credential when a cloud backend is actually active. A
/// local-Whisper user has nothing to look up, and skipping the read avoids
/// gratuitous keyring prompts on some Windows setups. Kept exactly aligned
/// with the schema's `stt_backend` values: `whisper` (local) vs. anything
/// cloud-shaped -- currently only `openai`.
fn stt_credential_for(stt_backend: &str, endpoint: &str) -> Option<String> {
    (stt_backend == "openai")
        .then(|| crate::credentials::resolve_stt_api_key(endpoint))
        .flatten()
}

/// Only fetch a post-processing credential when a cloud post-processor is
/// active. `none` and `ollama` are both local (no cloud endpoint), so the
/// credential lookup is skipped. Matches the schema's `post_processor`
/// values: `none` / `ollama` / `openai` / `groq`.
fn post_credential_for(post_processor: &str, endpoint: &str) -> Option<String> {
    matches!(post_processor, "openai" | "groq")
        .then(|| crate::credentials::resolve_post_api_key(endpoint))
        .flatten()
}

/// Which key variables to add to the worker's env, given what is already
/// there. Split from [`attach_cloud_api_keys`] so the PRECEDENCE of the
/// wiring is unit-testable without a config file, a credential store, or a
/// spawned process -- the resolver having correct precedence says nothing
/// about whether the caller wired it up correctly, and it was the wiring that
/// was missing entirely.
///
/// An existing value always wins, whether it came from the caller-built
/// command or the ambient environment, so
/// `VOICEPI_STT_API_KEY=... whisper-dictate run` still overrides the store.
fn cloud_api_key_env_additions<E>(
    existing: &[(String, String)],
    env_lookup: E,
    stt: Option<String>,
    post: Option<String>,
) -> Vec<(String, String)>
where
    E: Fn(&str) -> Option<String>,
{
    let mut out = Vec::new();
    for (name, resolved) in [("VOICEPI_STT_API_KEY", stt), ("VOICEPI_POST_API_KEY", post)] {
        if existing.iter().any(|(k, _)| k == name) {
            continue;
        }
        if env_lookup(name).is_some_and(|v| !v.trim().is_empty()) {
            continue;
        }
        if let Some(value) = resolved {
            out.push((name.to_owned(), value));
        }
    }
    out
}

#[cfg(test)]
mod cloud_api_key_wiring_tests {
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
        let endpoint =
            effective_endpoint(&e, "VOICEPI_STT_BASE_URL", "https://api.groq.com/openai/v1");
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
}
