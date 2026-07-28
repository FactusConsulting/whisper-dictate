//! Cloud API-key wiring for the spawned worker.
//!
//! Split out of `runtime/mod.rs` in the 500-LOC modularity refactor, matching
//! the precedent set by `worker_command.rs`. The logic is unchanged; it lives
//! here so `mod.rs` stays under the AGENTS.md per-file limit and so the
//! credential wiring reads as one unit rather than as an aside in the middle
//! of process management.

use super::worker_command::WorkerCommand;

/// Which key the caller has pushed as `VOICEPI_POST_API_KEY`.
///
/// Codex P1 #666-round2 #1 (`PRRT_kwDOSfNjQs6UXpn-` cmt 3665199618): the
/// UI's `App::worker_command` mirrors the STT key into `VOICEPI_POST_API_KEY`
/// when the user has NO post-specific key but wants a cloud post-processor.
/// The shim previously classified any `has_post` presence as "post-key
/// provenance" and stamped the POST endpoint -- so a Groq-STT + OpenAI-post
/// setup got an OpenAI marker for a key that was actually a Groq STT key,
/// approving a cross-provider send. The provenance now travels IN so the
/// marker binds to the endpoint the underlying key was actually resolved for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKeyProvenance {
    /// No `VOICEPI_POST_API_KEY` on the command.
    None,
    /// The caller pushed a key that came from the user's post-key input
    /// (or was resolved for the post endpoint by the launcher). Marker
    /// binds to the post endpoint.
    PostSpecific,
    /// The caller pushed the STT key AS the post key (UI mirror
    /// behaviour, or STT-as-post-fallback). Marker binds to the STT
    /// endpoint so a live post-endpoint change is refused.
    SttMirror,
}

/// Public seam for callers that build a [`WorkerCommand`] outside the
/// [`attach_cloud_api_keys`] flow -- notably `ui::app::App::worker_command`,
/// which pushes the API-key envs directly from the user's Settings input.
///
/// Codex P1 #666 #1 (`PRRT_kwDOSfNjQs6UXpn-`): the primary Windows tray
/// launcher builds the command WITHOUT going through
/// [`attach_cloud_api_keys`], so before this shim existed the marker was
/// stamped only for the terminal `whisper-dictate run` path -- the UI's Start
/// button was leaking exactly the way the original finding described. This
/// helper stamps `VOICEPI_POST_API_KEY_ENDPOINT` on `command` when it should
/// apply, mirroring the rules `attach_cloud_api_keys` uses:
///
/// * Marker only stamped when either `VOICEPI_STT_API_KEY` or
///   `VOICEPI_POST_API_KEY` is present on `command.env` -- either because
///   the caller just pushed it or because a prior helper added it.
/// * `provenance = PostSpecific`: marker = normalised post endpoint.
/// * `provenance = SttMirror` OR `None` + cloud STT: marker = normalised
///   STT endpoint (STT key is what will serve post-processing). Codex
///   round-2 #1 fix: without provenance the shim used to stamp the POST
///   endpoint for a mirrored STT key, approving cross-provider sends.
/// * Never overwrites an existing marker already on `command.env` -- caller
///   ownership stays intact, matching the `attach_cloud_api_keys` rule.
///
/// A no-op when neither key is on the command or when both processors are
/// local; keeps the local-Whisper install path zero-cost.
pub fn stamp_post_api_key_endpoint_marker(
    command: &mut WorkerCommand,
    post_key_provenance: PostKeyProvenance,
    post_processor: &str,
    post_base_url: &str,
    stt_backend: &str,
    stt_base_url: &str,
) {
    stamp_post_api_key_endpoint_marker_with(
        command,
        post_key_provenance,
        post_processor,
        post_base_url,
        stt_backend,
        stt_base_url,
        |name| std::env::var(name).ok(),
    );
}

/// Testable core of [`stamp_post_api_key_endpoint_marker`]. Injecting the
/// env lookup lets the ownership-preservation test model an ambient
/// `VOICEPI_POST_API_KEY` without touching (and racing on) `std::env`.
pub(crate) fn stamp_post_api_key_endpoint_marker_with(
    command: &mut WorkerCommand,
    post_key_provenance: PostKeyProvenance,
    post_processor: &str,
    post_base_url: &str,
    stt_backend: &str,
    stt_base_url: &str,
    env_lookup: impl Fn(&str) -> Option<String>,
) {
    const MARKER: &str = "VOICEPI_POST_API_KEY_ENDPOINT";
    if command.env.iter().any(|(k, _)| k == MARKER) {
        return; // caller owns the marker
    }
    let has_stt = command
        .env
        .iter()
        .any(|(k, v)| k == "VOICEPI_STT_API_KEY" && !v.trim().is_empty());
    let has_post_env = command
        .env
        .iter()
        .any(|(k, v)| k == "VOICEPI_POST_API_KEY" && !v.trim().is_empty());
    // Codex P2 round-2 #3 + P1 round-3 (`PRRT_kwDOSfNjQs6UZLOy` cmt
    // 3665404566): "explicit env keys own their resolution" only holds
    // when the ambient key is what the CHILD will actually see. The
    // supervisor spawns the worker via `Command::envs(&command.env)`,
    // which OVERRIDES the ambient environment -- so if the UI has
    // already pushed a `VOICEPI_POST_API_KEY` into `command.env`, the
    // child sees the command-env value, NOT the ambient one. In that
    // inverse-precedence case the marker MUST be stamped for the
    // command-env key; skipping only for a genuine ambient override
    // (no command-env post key present) is what preserves both
    // properties.
    let ambient_post_key = env_lookup("VOICEPI_POST_API_KEY").is_some_and(|v| !v.trim().is_empty());
    if ambient_post_key && !has_post_env {
        // Ambient wins: user-owned resolution, no launcher marker.
        return;
    }
    if !(has_stt || has_post_env) {
        return;
    }
    // Both branches strip trailing `/` BEFORE normalising so the launcher
    // and worker derive the same effective endpoint (Codex P2 #666 #8):
    // both worker loaders do `raw.rstrip("/")` before their local-default
    // substitution table, and a mismatched marker vs. worker URL causes the
    // revalidation check to reject a legitimate key.
    let is_post_specific = matches!(post_key_provenance, PostKeyProvenance::PostSpecific);
    // Pick the endpoint based on PROVENANCE, not just presence. A
    // mirrored STT key OR an STT-only injection falls through to the
    // `has_stt` branch (STT endpoint); a genuine post key uses the
    // post endpoint.
    let endpoint = if is_post_specific && matches!(post_processor, "openai" | "groq") {
        // `normalized_base_url` swaps the URL when the saved value is a
        // DIFFERENT processor's default -- the same substitution the
        // post-processing pipeline itself applies.
        Some(crate::postprocess::normalized_base_url(
            post_processor,
            post_base_url.trim_end_matches('/'),
        ))
    } else if has_stt && stt_backend == "openai" {
        // STT base URL is used AS-IS (no post-processor default swap): the
        // STT `openai` backend already points at the exact provider URL
        // the user configured, and the credential was resolved against
        // THAT URL. Trailing slash still stripped for origin parity.
        Some(stt_base_url.trim_end_matches('/').to_owned())
    } else {
        None
    };
    if let Some(ep) = endpoint {
        command.env.push((MARKER.to_owned(), ep));
    }
}

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
    let stt_key = stt_credential_for(&stt_backend, &stt_endpoint);
    // STT-as-post-fallback marker (Codex P1 #666 #2, `PRRT_kwDOSfNjQs6UXpnu`):
    // both settings loaders accept `VOICEPI_STT_API_KEY` as a post-key
    // fallback (Rust `postprocess/settings.rs`,
    // Python `vp_postprocess._postprocess_api_key`). An STT-only injection
    // (spawn-time `post_processor` = `none`/`ollama`) therefore leaves the
    // STT credential AVAILABLE for post-processing after a live change.
    // Without a marker, that fallback would send the STT key to whatever
    // endpoint the worker later resolves. If we have no post-specific
    // marker AND we're injecting an STT credential against a cloud STT
    // backend, stamp the marker with the STT endpoint so
    // `require_endpoint_matches_marker` guards the fallback too.
    let effective_marker = post_key_endpoint.or_else(|| {
        // STT base URL used as-is; see `stamp_post_api_key_endpoint_marker`
        // for the reasoning (no post-processor default swap for STT).
        // Trailing slash stripped so a saved `https://api.groq.com/openai/v1/`
        // marker matches the worker's post-fallback origin (Codex P2 #666 #8).
        (stt_backend == "openai" && stt_key.is_some())
            .then(|| stt_endpoint.trim_end_matches('/').to_owned())
    });
    let additions = cloud_api_key_env_additions(
        &command.env,
        |name| std::env::var(name).ok(),
        stt_key,
        post_key,
        effective_marker,
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
    // Codex P2 #666 #8 (`PRRT_kwDOSfNjQs6UYNkF`): strip the trailing slash
    // BEFORE normalising. Both worker settings loaders
    // (`postprocess/settings.rs` and `vp_postprocess.load_postprocess_settings`)
    // do `raw.rstrip("/")` before comparing against the local-default
    // substitution table, so a saved `http://localhost:11434/` matches the
    // Ollama default AT THE WORKER and gets substituted to the provider's
    // real endpoint. Without the same strip here, the launcher classifies
    // the raw URL as Custom -> stamps a Custom marker, then the worker
    // hits the substituted provider URL and the marker check rejects a
    // legitimate key as a Custom-to-Groq mismatch. Launcher and worker
    // MUST derive the same effective endpoint.
    let endpoint_stripped = endpoint.trim_end_matches('/');
    let effective = crate::postprocess::normalized_base_url(post_processor, endpoint_stripped);
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
    let mut wrote_stt_key = false;
    for (name, resolved) in [("VOICEPI_STT_API_KEY", stt), ("VOICEPI_POST_API_KEY", post)] {
        if existing.iter().any(|(k, _)| k == name) {
            continue;
        }
        if env_lookup(name).is_some_and(|v| !v.trim().is_empty()) {
            continue;
        }
        if let Some(value) = resolved {
            match name {
                "VOICEPI_POST_API_KEY" => wrote_post_key = true,
                "VOICEPI_STT_API_KEY" => wrote_stt_key = true,
                _ => {}
            }
            out.push((name.to_owned(), value));
        }
    }
    // Stamp the endpoint the injected key was resolved against so the
    // worker's postprocess pipeline can refuse to send it to a different
    // provider after a live setting change (Codex P1 #642). Emitted when we
    // added EITHER the post key OR the STT key ourselves -- the STT key can
    // serve as a post-key fallback (Codex P1 #666 #2), so it needs the same
    // endpoint guard. If the caller/env already had `VOICEPI_POST_API_KEY`
    // set, they own the resolution and no launcher marker applies. The
    // marker is ADVISORY: a marker already on the caller command / env wins.
    if wrote_post_key || wrote_stt_key {
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
