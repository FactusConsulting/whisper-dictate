//! Subcommand entry points for `whisper-dictate transcribe-wav` (single-shot)
//! and `whisper-dictate transcribe-server` (long-running in-process worker).
//!
//! ## Why two modes
//!
//! `transcribe-wav` is the historical Phase 1.2 shape (one request per
//! process invocation). Every call reloads the GGML model from disk —
//! 75 MB to 1.5 GB depending on size — so a dictation session pays the
//! cold-start cost on every utterance. The Python wrapper
//! `vp_transcribe.py::RustWhisperShellModel` shells out per call.
//!
//! `transcribe-server` (Wave 8-A of #348) keeps the worker alive between
//! requests via a line-delimited JSON protocol on stdin/stdout. The model
//! is wrapped in [`IdleUnloadingModel`] so it lazy-loads on first
//! transcribe AND drops itself after `VOICEPI_WHISPER_IDLE_UNLOAD_S`
//! seconds of inactivity, returning the RAM. The Python wrapper spawns
//! the server ONCE per supervisor lifetime instead of once per utterance.
//!
//! Both modes share the same request/response envelope — defined in
//! [`super::protocol`] so the always-compiled JSON contract can be
//! unit-tested without whisper.cpp on the build host. The whisper-rs-local
//! feature is only required for the wiring layer here (the actual model
//! load + inference call).
//!
//! ## Model resolution
//!
//! Both modes resolve the model file in this order:
//! 1. `VOICEPI_WHISPER_MODEL_PATH` — explicit power-user override (unchanged).
//! 2. The catalog entry whose name matches `VOICEPI_MODEL` (materialised
//!    from `settings.model` by the schema-driven `worker_env_overrides`),
//!    when that entry's cache file is downloaded + SHA-verified. This
//!    honours the UI dropdown value (bug 2 of the multilingual-catalog PR).
//! 3. The first catalog entry whose file is downloaded + SHA-verified in
//!    the user-cache `whisper-models/` directory.
//! 4. Any user-supplied custom GGML file the auto-discovery pass found
//!    under the same cache directory (#332).
//!
//! ## Per-request error handling
//!
//! `transcribe-wav` (single-shot) exits non-zero on any error — the
//! historical contract; the Python wrapper treats a non-zero exit as
//! "fall back to faster-whisper".
//!
//! `transcribe-server` (long-running) emits a `{"error": "..."}` line on
//! per-request errors and CONTINUES serving — tearing the worker down on
//! a single bad request would defeat the whole point of caching the
//! loaded model. The per-request envelope shape lives in
//! [`super::protocol::error_envelope`].

use std::env;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use super::idle::{parse_idle_timeout_from_env, IdleUnloadingModel};
use super::local::LocalWhisper;
use super::protocol::{
    normalise_language, normalise_prompt, read_request_from_reader, serve_loop, ServerReady,
    TranscribeRequest, TranscribeResponse,
};

/// Env var the Python wiring sets to point at a downloaded GGML model file.
pub const MODEL_PATH_ENV: &str = "VOICEPI_WHISPER_MODEL_PATH";

/// Env var carrying the settings-picked Whisper model NAME (e.g.
/// `large-v3`, `tiny`, `medium`). Read by [`resolve_model_path_from_env`]
/// AFTER [`MODEL_PATH_ENV`] so an explicit power-user path still wins, but
/// BEFORE the "first catalog entry that happens to be cached" fallback so
/// the UI dropdown value is honoured instead of ignored (bug 2 of this
/// PR). Materialised into the worker env from `settings.model` by the
/// schema-driven `worker_env_overrides` under its Python-legacy name
/// `VOICEPI_MODEL`; kept identical here so no extra wiring is needed.
pub const MODEL_NAME_ENV: &str = "VOICEPI_MODEL";

/// Entry point used by `main.rs` for the single-shot `transcribe-wav`
/// subcommand. Reads one JSON request from stdin, runs inference, and
/// prints one JSON response to stdout.
pub fn handle_transcribe_wav() -> Result<()> {
    let request = read_request_from_reader(&mut io::stdin().lock())?;
    let preferred = preferred_model_name_from_env();
    let model_path = resolve_model_path_from_env(preferred.as_deref())?;
    let response = dispatch(&model_path, request)?;
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

/// Entry point used by `main.rs` for the long-running `transcribe-server`
/// subcommand (Wave 8-A of #348).
///
/// Loads the model once (lazily, on the first transcribe call) and keeps
/// it resident behind an [`IdleUnloadingModel`] that drops it after
/// `VOICEPI_WHISPER_IDLE_UNLOAD_S` seconds of inactivity. Reads one JSON
/// request per `\n`-terminated line from stdin, writes one JSON response
/// per line to stdout. Per-request errors stay in-protocol (encoded as
/// `{"error": "..."}` envelopes via [`super::protocol::error_envelope`])
/// so the long-running worker survives bad requests.
///
/// Emits a [`ServerReady`] line first so the Python wrapper can confirm
/// the binary supports the long-running mode and log the effective
/// model + idle config before sending its first request. The model is
/// NOT loaded at this point — first transcribe call triggers the load
/// via [`IdleUnloadingModel::with_model`]'s lazy loader.
pub fn handle_transcribe_server() -> Result<()> {
    let preferred = preferred_model_name_from_env();
    let model_path = resolve_model_path_from_env(preferred.as_deref())?;
    let idle = parse_idle_timeout_from_env()?;
    let model = IdleUnloadingModel::for_local_whisper(model_path.clone(), idle);

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    // Emit ready before doing anything else so the Python wrapper sees
    // life-signs even if the first transcribe call takes a while to
    // load the model. The wrapper greps for `"ready":true` on the first
    // line so the protocol stays stable.
    let ready = ServerReady {
        ready: true,
        model_path: model_path.display().to_string(),
        idle_unload_s: idle.map(|d| d.as_secs()).unwrap_or(0),
    };
    use std::io::Write;
    writeln!(stdout, "{}", serde_json::to_string(&ready)?)
        .context("failed to write transcribe-server ready line")?;
    stdout
        .flush()
        .context("failed to flush transcribe-server ready line")?;

    let stdin = io::stdin();
    let stdin = stdin.lock();
    let reader = BufReader::new(stdin);
    serve_loop(reader, stdout, |wav_path, language, initial_prompt| {
        model.with_model(|m| m.transcribe_wav(Path::new(wav_path), language, initial_prompt))
    })
}

/// Pure helper for the settings-preferred lookup step in
/// [`resolve_model_path_from_env`]: given a preferred name and a catalog
/// slice, return the cache path of the matching, downloaded, SHA-verified
/// entry — or `None` when the name is empty, doesn't match any entry, or
/// the matched entry isn't cached yet.
///
/// Split out so tests can drive the "preferred wins over first-cached"
/// invariant against a synthetic catalog whose SHA-256 values they can
/// forge (production `CATALOG` entries pin real upstream hashes, so
/// tests can't plant matching files without shipping the real GGML
/// bytes).
pub(crate) fn preferred_catalog_path(
    preferred: Option<&str>,
    catalog: &[crate::whisper::model_manager::ModelEntry],
) -> Option<PathBuf> {
    let name = preferred.map(str::trim).filter(|s| !s.is_empty())?;
    let entry = catalog.iter().find(|e| e.name == name)?;
    if !crate::whisper::model_manager::is_downloaded(entry) {
        return None;
    }
    crate::whisper::model_manager::model_path(entry).ok()
}

/// Read the settings-picked model name from the process env. Used by the
/// CLI dispatchers to feed [`resolve_model_path_from_env`]'s `preferred`
/// argument WITHOUT reaching back into `crate::config` (the worker
/// subprocess has no on-disk settings; the supervisor materialises them
/// into env vars via `worker_env_overrides` before spawning).
///
/// Returns `None` when the env var is unset OR set to an empty string —
/// both cases collapse to "no preference" so the caller falls back to
/// today's "first catalog entry that happens to be cached" behaviour.
pub(crate) fn preferred_model_name_from_env() -> Option<String> {
    env::var(MODEL_NAME_ENV)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Resolve the model file path for inference.
///
/// Checks, in order:
/// 1. `VOICEPI_WHISPER_MODEL_PATH` — explicit power-user override.
///    Returned as-is; existence is checked at load time. Wins over
///    everything else because it is the documented way to point at a
///    fine-tuned / out-of-catalog GGML file.
/// 2. `preferred` (typically `settings.model`, threaded through as
///    `VOICEPI_MODEL`). If the value matches a catalog entry AND that
///    entry's file is downloaded + SHA-256-verified in the cache, we
///    return its path. This is the fix for bug 2 of the multilingual-
///    catalog PR: the UI dropdown value is finally honoured instead of
///    silently ignored.
/// 3. The first catalog entry whose SHA-verified file exists in the cache
///    directory. Preserves the historical fallback for callers that
///    don't pass a `preferred` value.
/// 4. Auto-discovered custom GGML files the user dropped into the models
///    cache directory (#332). Sorted by filename; the first is returned.
///    We can't verify these against a known SHA-256 (they aren't in the
///    catalog), so the loader is the last line of defence against a
///    corrupt file — but a wonky filename shouldn't hide the model.
///
/// `pub(crate)` so the Wave 5 PR 5 in-process session sink can reuse the
/// same resolution rules when wiring [`WhisperLocalTranscribeBackend`]
/// behind `VOICEPI_DICTATE_BACKEND=rust-session` — keeping the
/// resolution logic single-sourced means the env-var / cache-lookup
/// contract is identical for the subprocess-per-utterance dispatcher
/// (`handle_transcribe_wav`), the long-running server
/// (`handle_transcribe_server`), and the in-process session sink.
pub(crate) fn resolve_model_path_from_env(preferred: Option<&str>) -> Result<PathBuf> {
    // Primary: explicit env var override.
    if let Ok(raw) = env::var(MODEL_PATH_ENV) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
        return Err(anyhow!(
            "{MODEL_PATH_ENV} is set but empty; point it at a GGML \
             whisper.cpp model file"
        ));
    }
    // Fallback A: the settings-preferred entry, if the user's picked
    // model name maps to a catalog entry AND that entry is downloaded.
    // A non-matching or not-yet-downloaded preference falls through to
    // the historical "first cached catalog" behaviour rather than
    // erroring — the user can still transcribe with whatever IS on
    // disk, and the Speech tab's "Missing" badge nudges them to
    // download the picked entry when they're ready.
    if let Some(path) = preferred_catalog_path(preferred, crate::whisper::model_manager::CATALOG) {
        return Ok(path);
    }
    // Fallback B: first catalog model that exists in the cache directory AND
    // whose SHA-256 matches the catalog.  Verifying before selecting means a
    // truncated or corrupt file is skipped (the next catalog entry is tried)
    // rather than being passed to whisper-rs where it produces a confusing
    // load error.  The OS page cache makes repeated reads of the same file
    // fast; this check runs once per process launch, not once per transcription.
    for entry in crate::whisper::model_manager::CATALOG {
        if crate::whisper::model_manager::is_downloaded(entry) {
            if let Ok(path) = crate::whisper::model_manager::model_path(entry) {
                return Ok(path);
            }
        }
    }
    // Fallback C: a user-supplied custom GGML file the auto-discovery pass
    // found under the models cache directory (#332). This is what lets a
    // power user drop in a fine-tuned or quantised model without editing
    // the catalog or exporting `VOICEPI_WHISPER_MODEL_PATH`.
    if let Ok(dir) = crate::whisper::model_manager::models_cache_dir() {
        let discovered = crate::whisper::local_discovery::discover_models(&dir);
        if let Some(first) = discovered.into_iter().next() {
            return Ok(first.path);
        }
    }
    Err(anyhow!(
        "{MODEL_PATH_ENV} is not set and no GGML model was found in the \
         whisper-models cache directory; download one via the Settings → \
         Speech tab or `whisper-dictate models download <name>` (catalog: \
         tiny / base / small / medium / large-v3-turbo / large-v3, plus \
         the English-only tiny.en / base.en / small.en), drop a custom \
         ggml-*.bin / *.gguf file into the models cache directory, or \
         set {MODEL_PATH_ENV} to point at a GGML whisper.cpp model file"
    ))
}

/// Pure helper used by `handle_transcribe_wav`. Split out so the CLI plumbing
/// (stdin read, stdout print, env-var resolve) stays separable from the
/// actual model load + inference call — which is the only piece that touches
/// whisper.cpp and is therefore awkward to unit-test without a real model.
fn dispatch(model_path: &Path, request: TranscribeRequest) -> Result<TranscribeResponse> {
    match request {
        TranscribeRequest::TranscribeWav {
            wav_path,
            language,
            initial_prompt,
        } => {
            let whisper = LocalWhisper::new(model_path).with_context(|| {
                format!("failed to load whisper model from {}", model_path.display())
            })?;
            // Normalise the language and prompt at the dispatch boundary so
            // the library layer below only sees the states it cares about.
            // The mirror of this normalisation in `LocalWhisper::transcribe_*`
            // is belt-and-braces — keeping both means a direct library caller
            // (the example binary, future test code) still gets the right
            // behaviour on an empty-string prompt.
            //
            // The two fields normalise differently: `language` has an `auto`
            // sentinel meaning "let the model detect" (mapped to None), but
            // `initial_prompt` is free-form user text — collapsing a literal
            // "auto" prompt to None would silently drop a valid user prompt
            // that happens to equal that word (it would still be applied on
            // the faster-whisper path).
            let lang_for_call = normalise_language(language.as_deref());
            let prompt_for_call = normalise_prompt(initial_prompt.as_deref());
            let text =
                whisper.transcribe_wav(Path::new(&wav_path), lang_for_call, prompt_for_call)?;
            Ok(TranscribeResponse { text })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::{EnvVarGuard, ENV_LOCK};

    /// Pick the env var `user_cache_dir` consults on the current platform.
    const CACHE_ENV_VAR: &str = if cfg!(windows) {
        "LOCALAPPDATA"
    } else if cfg!(target_os = "macos") {
        "HOME"
    } else {
        "XDG_CACHE_HOME"
    };

    #[test]
    fn resolve_model_path_errors_when_env_missing_and_no_cache() {
        // RAII guards restore the originals even if the assert below panics
        // — without them a failed assertion would leak MODEL_PATH_ENV-unset
        // + a bogus CACHE_ENV_VAR into every later test (Codex P2 #415).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _model = EnvVarGuard::remove(MODEL_PATH_ENV);
        // Pin cache to a non-existent dir so the cache fallback also finds nothing.
        let _cache = EnvVarGuard::set(CACHE_ENV_VAR, "/definitely/not/a/real/dir/xyz");

        let err = resolve_model_path_from_env(None).unwrap_err();
        assert!(
            err.to_string().contains(MODEL_PATH_ENV),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_model_path_errors_when_env_blank() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _model = EnvVarGuard::set(MODEL_PATH_ENV, "   ");

        let err = resolve_model_path_from_env(None).unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_model_path_skips_corrupt_cached_file() {
        // P2: a file that exists but fails SHA-256 verification must be
        // skipped; the function must NOT return a corrupt cached path.
        // After the fix, `is_downloaded` gates the fallback, so a file with
        // wrong content is skipped and the function errors (no valid model).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _model = EnvVarGuard::remove(MODEL_PATH_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let _cache = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());

        let cache_subdir = if cfg!(target_os = "macos") {
            tmp.path()
                .join("Library/Caches/whisper-dictate/whisper-models")
        } else {
            tmp.path().join("whisper-dictate/whisper-models")
        };
        std::fs::create_dir_all(&cache_subdir).unwrap();
        // Plant a file with wrong content — SHA-256 won't match the catalog.
        let corrupt_model = cache_subdir.join("ggml-tiny.en.bin");
        std::fs::write(&corrupt_model, b"corrupt-contents").unwrap();

        // Must fail: corrupt file is skipped, no valid model found.
        let err = resolve_model_path_from_env(None)
            .expect_err("corrupt cached file must not be returned");
        assert!(
            err.to_string().contains(MODEL_PATH_ENV),
            "error must name the missing-model env var: {err}"
        );
    }

    #[test]
    fn resolve_model_path_returns_discovered_custom_model() {
        // #332: when neither the env var nor a catalog model is present,
        // the resolver must fall back to any user-supplied GGML file the
        // discovery pass finds under the models cache directory.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::remove_var(MODEL_PATH_ENV);
        let tmp = tempfile::tempdir().unwrap();
        let saved_cache = env::var_os(CACHE_ENV_VAR);
        env::set_var(CACHE_ENV_VAR, tmp.path());

        let cache_subdir = if cfg!(target_os = "macos") {
            tmp.path()
                .join("Library/Caches/whisper-dictate/whisper-models")
        } else {
            tmp.path().join("whisper-dictate/whisper-models")
        };
        std::fs::create_dir_all(&cache_subdir).unwrap();
        let custom = cache_subdir.join("my-custom-large.bin");
        // Needs to clear the 1 MiB MIN_MODEL_BYTES threshold in discovery.
        let f = std::fs::File::create(&custom).unwrap();
        f.set_len(2 * 1024 * 1024).unwrap();
        drop(f);

        let resolved =
            resolve_model_path_from_env(None).expect("must fall back to discovered file");
        assert_eq!(
            resolved, custom,
            "resolver must return the auto-discovered custom model"
        );

        match saved_cache {
            Some(v) => env::set_var(CACHE_ENV_VAR, v),
            None => env::remove_var(CACHE_ENV_VAR),
        }
        if let Some(v) = saved {
            env::set_var(MODEL_PATH_ENV, v);
        }
    }

    #[test]
    fn resolve_model_path_returns_value_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _model = EnvVarGuard::set(MODEL_PATH_ENV, "/tmp/ggml-tiny.en.bin");

        let p = resolve_model_path_from_env(None).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ggml-tiny.en.bin"));
    }

    #[test]
    fn preferred_model_name_from_env_reads_voicepi_model() {
        // Bug 2 wiring: the resolver reads the settings-derived VOICEPI_MODEL
        // env var. Empty / unset -> None so the caller falls through to
        // today's first-cached behaviour.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let _g_unset = EnvVarGuard::remove(MODEL_NAME_ENV);
        assert_eq!(
            preferred_model_name_from_env(),
            None,
            "unset must map to None"
        );
        drop(_g_unset);

        let _g_blank = EnvVarGuard::set(MODEL_NAME_ENV, "   ");
        assert_eq!(
            preferred_model_name_from_env(),
            None,
            "whitespace-only must map to None"
        );
        drop(_g_blank);

        let _g_set = EnvVarGuard::set(MODEL_NAME_ENV, "  large-v3  ");
        assert_eq!(
            preferred_model_name_from_env().as_deref(),
            Some("large-v3"),
            "value must be trimmed"
        );
    }

    #[test]
    fn preferred_catalog_path_prefers_named_over_first_cached() {
        // Bug 2 regression: with MULTIPLE cached entries the resolver must
        // return the settings-picked one, not the first catalog entry that
        // happens to be cached. We can't plant real GGML bytes (catalog
        // SHA-256s pin upstream files), so we drive `preferred_catalog_path`
        // against a synthetic catalog whose hashes we forge and plant to
        // disk under a temp cache. The production `resolve_model_path_from_env`
        // path uses the SAME helper against the real CATALOG, so this proves
        // the pick-wins-over-first-cached invariant.
        use crate::whisper::model_manager::{model_path, ModelEntry};
        use sha2::{Digest, Sha256};

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _cache = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());

        let cache_subdir = if cfg!(target_os = "macos") {
            tmp.path()
                .join("Library/Caches/whisper-dictate/whisper-models")
        } else {
            tmp.path().join("whisper-dictate/whisper-models")
        };
        std::fs::create_dir_all(&cache_subdir).unwrap();

        // Plant two synthetic catalog entries with forged (but internally
        // valid) SHA-256s. Both are "downloaded" — the "second" (preferred)
        // must win over the "first" (which would otherwise be picked by
        // fallback B's first-cached loop).
        fn plant(cache: &Path, name: &'static str, body: &[u8]) -> ModelEntry {
            let mut h = Sha256::new();
            h.update(body);
            let hex = format!("{:x}", h.finalize());
            let hash: &'static str = Box::leak(hex.into_boxed_str());
            let filename: &'static str = Box::leak(format!("ggml-{name}.bin").into_boxed_str());
            std::fs::write(cache.join(filename), body).unwrap();
            ModelEntry {
                name,
                filename,
                url: "https://example.invalid/",
                sha256: hash,
                size_bytes: body.len() as u64,
                description: "synthetic test entry",
            }
        }
        let first = plant(&cache_subdir, "test-first", b"first-catalog-bytes");
        let second = plant(&cache_subdir, "test-second", b"second-catalog-bytes");
        let catalog = [first, second];

        // Preferred = second must return second's path, NOT first's.
        let resolved = preferred_catalog_path(Some("test-second"), &catalog)
            .expect("preferred entry must resolve when cached");
        assert_eq!(resolved, model_path(&catalog[1]).unwrap());

        // Empty / unknown / not-downloaded preference falls through to None
        // so the caller's fallback B (first-cached) still runs.
        assert!(preferred_catalog_path(None, &catalog).is_none());
        assert!(preferred_catalog_path(Some(""), &catalog).is_none());
        assert!(preferred_catalog_path(Some("   "), &catalog).is_none());
        assert!(preferred_catalog_path(Some("not-in-catalog"), &catalog).is_none());
    }

    #[test]
    fn preferred_catalog_path_none_when_entry_not_downloaded() {
        // A named-but-not-yet-cached preference must return None so the
        // resolver falls through to fallback B rather than erroring — this
        // is what lets a user transcribe with whatever IS on disk while the
        // "Missing" badge nudges them to download the picked entry.
        use crate::whisper::model_manager::ModelEntry;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let _cache = EnvVarGuard::set(CACHE_ENV_VAR, tmp.path());

        let bogus_hash: &'static str = Box::leak("00".repeat(32).into_boxed_str());
        let missing = ModelEntry {
            name: "test-missing",
            filename: "ggml-test-missing.bin",
            url: "https://example.invalid/",
            // Any hex — nothing on disk to verify against.
            sha256: bogus_hash,
            size_bytes: 1024,
            description: "synthetic test entry (not on disk)",
        };
        assert!(preferred_catalog_path(Some("test-missing"), &[missing]).is_none());
    }

    /// End-to-end: with a real model + WAV (env-gated, same as the
    /// `transcribes_hello_world_when_model_available` test in mod.rs),
    /// run the CLI dispatch path and assert the JSON envelope is well-formed
    /// and contains the expected substring. Skipped unless the developer
    /// opts in via the two env vars below.
    #[test]
    fn dispatches_real_transcription_when_model_available() {
        let (Ok(model), Ok(wav)) = (
            env::var("WHISPER_TEST_MODEL_PATH"),
            env::var("WHISPER_TEST_WAV_PATH"),
        ) else {
            eprintln!(
                "skipping: set WHISPER_TEST_MODEL_PATH (GGML whisper model) and \
                 WHISPER_TEST_WAV_PATH (16 kHz mono 'hello world' WAV) to run"
            );
            return;
        };

        let request = TranscribeRequest::TranscribeWav {
            wav_path: wav,
            language: None,
            initial_prompt: None,
        };
        let response = dispatch(Path::new(&model), request).expect("dispatch ok");
        assert!(
            response.text.to_lowercase().contains("hello"),
            "transcript missing 'hello': {:?}",
            response.text
        );
    }
}
