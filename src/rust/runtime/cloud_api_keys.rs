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

    // Resolve the post key AND capture the endpoint it was resolved against,
    // so the worker can later refuse to send it to a different endpoint --
    // the leak fixed by this module's marker (Codex P1 #642). The credential
    // is fetched against the NORMALISED endpoint (same helper the store lookup
    // uses), so `POST_API_KEY_ENDPOINT` records the exact URL the resolver saw.
    let (post_key, post_key_endpoint) =
        post_credential_and_endpoint(&post_processor, &post_endpoint);
    let additions = cloud_api_key_env_additions(
        &command.env,
        |name| std::env::var(name).ok(),
        stt_credential_for(&stt_backend, &stt_endpoint),
        post_key,
        post_key_endpoint,
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
///
/// The endpoint is NORMALISED before classification, through the same
/// `postprocess::normalized_base_url` the worker itself applies. Without that
/// step a config with `post_processor=groq` and `post_base_url` left at the
/// schema's local Ollama default (or empty) classified as a CUSTOM host here
/// and loaded no credential -- while the worker substituted the provider's
/// real endpoint and then needed the key. Selecting a cloud post-processor in
/// Settings without also editing the URL is the normal path, so that gap hit
/// the default configuration rather than an exotic one.
#[cfg(test)]
fn post_credential_for(post_processor: &str, endpoint: &str) -> Option<String> {
    post_credential_with(
        post_processor,
        endpoint,
        crate::credentials::resolve_post_api_key,
    )
}

/// Testable core of [`post_credential_for`]: the store lookup is injected so
/// a test can observe WHICH endpoint the resolver is handed. Asserting on the
/// normaliser alone would not do -- that helper is already covered by
/// `postprocess::settings`, so reverting the normalisation here would leave
/// such a test green while the saved-key failure came straight back.
#[cfg(test)]
fn post_credential_with<R>(post_processor: &str, endpoint: &str, resolve: R) -> Option<String>
where
    R: Fn(&str) -> Option<String>,
{
    post_credential_and_endpoint_with(post_processor, endpoint, resolve).0
}

/// Resolve the post key AND report the NORMALISED endpoint the resolver was
/// handed. The endpoint travels back to the worker as
/// `VOICEPI_POST_API_KEY_ENDPOINT` so the postprocess pipeline can refuse to
/// send the injected key to a different provider after a live
/// `post_processor` / `post_base_url` change (Codex P1 #642). The endpoint is
/// `Some(...)` only when a key was actually resolved, so a launcher that
/// finds no cloud credential does not stamp a misleading marker.
fn post_credential_and_endpoint(
    post_processor: &str,
    endpoint: &str,
) -> (Option<String>, Option<String>) {
    post_credential_and_endpoint_with(
        post_processor,
        endpoint,
        crate::credentials::resolve_post_api_key,
    )
}

/// Testable core of [`post_credential_and_endpoint`]. Returns
/// `(key, endpoint)` where `endpoint` is the same value the injected `resolve`
/// was called with -- so a test can pin BOTH the store lookup and the marker
/// against the same normalised endpoint.
fn post_credential_and_endpoint_with<R>(
    post_processor: &str,
    endpoint: &str,
    resolve: R,
) -> (Option<String>, Option<String>)
where
    R: Fn(&str) -> Option<String>,
{
    if !matches!(post_processor, "openai" | "groq") {
        return (None, None);
    }
    let effective = crate::postprocess::normalized_base_url(post_processor, endpoint);
    match resolve(&effective) {
        Some(key) => (Some(key), Some(effective)),
        // No key resolved -> do NOT stamp a marker. A stale marker without a
        // key would still block the worker from picking up an explicit
        // `VOICEPI_POST_API_KEY` for the SAME endpoint that a user exported
        // manually after the launcher decided the store was empty.
        None => (None, None),
    }
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
    post_endpoint: Option<String>,
) -> Vec<(String, String)>
where
    E: Fn(&str) -> Option<String>,
{
    let mut out = Vec::new();
    let mut wrote_post_key = false;
    for (name, resolved) in [("VOICEPI_STT_API_KEY", stt), ("VOICEPI_POST_API_KEY", post)] {
        if existing.iter().any(|(k, _)| k == name) {
            continue;
        }
        if env_lookup(name).is_some_and(|v| !v.trim().is_empty()) {
            continue;
        }
        if let Some(value) = resolved {
            if name == "VOICEPI_POST_API_KEY" {
                wrote_post_key = true;
            }
            out.push((name.to_owned(), value));
        }
    }
    // Stamp the endpoint the injected post key was resolved against so the
    // worker's postprocess pipeline can refuse to send it to a different
    // provider after a live setting change (Codex P1 #642). Only emitted when
    // we actually added the key ourselves -- if the caller/env already had
    // VOICEPI_POST_API_KEY set, they own the resolution and no marker applies.
    // The marker is ADVISORY: if the parent env already carried a marker (e.g.
    // a nested spawn), it wins; only clear-and-set for our own injection.
    if wrote_post_key {
        if let Some(endpoint) = post_endpoint {
            let marker = "VOICEPI_POST_API_KEY_ENDPOINT";
            let already_on_command = existing.iter().any(|(k, _)| k == marker);
            let already_in_env = env_lookup(marker).is_some_and(|v| !v.trim().is_empty());
            if !already_on_command && !already_in_env {
                out.push((marker.to_owned(), endpoint));
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "cloud_api_keys_tests.rs"]
mod cloud_api_keys_tests;
