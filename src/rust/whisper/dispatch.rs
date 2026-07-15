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
//! Both modes resolve the model file from `VOICEPI_WHISPER_MODEL_PATH` first,
//! then fall back to the first verified entry in the user-cache
//! `whisper-models/` directory. Setting the env var explicitly always
//! wins so power users can pin a specific GGML file outside the catalog.
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

/// Entry point used by `main.rs` for the single-shot `transcribe-wav`
/// subcommand. Reads one JSON request from stdin, runs inference, and
/// prints one JSON response to stdout.
pub fn handle_transcribe_wav() -> Result<()> {
    let request = read_request_from_reader(&mut io::stdin().lock())?;
    let model_path = resolve_model_path_from_env()?;
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
    let model_path = resolve_model_path_from_env()?;
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

/// Resolve the model file path for inference.
///
/// Checks, in order:
/// 1. `VOICEPI_WHISPER_MODEL_PATH` — explicit override (unchanged behaviour).
/// 2. The user-cache whisper-models directory (populated by `models download`
///    or the Settings download UI) — tiny.en → base.en → small.en preference.
///    This means a user who downloaded a model via the UI can start with
///    `VOICEPI_TRANSCRIBE_BACKEND=rust` without a separate env-var step.
///
/// `pub(crate)` so the Wave 5 PR 5 in-process session sink can reuse the
/// same resolution rules when wiring [`WhisperLocalTranscribeBackend`]
/// behind `VOICEPI_DICTATE_BACKEND=rust-session` — keeping the
/// resolution logic single-sourced means the env-var / cache-lookup
/// contract is identical for the subprocess-per-utterance dispatcher
/// (`handle_transcribe_wav`), the long-running server
/// (`handle_transcribe_server`), and the in-process session sink.
pub(crate) fn resolve_model_path_from_env() -> Result<PathBuf> {
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
    // Fallback: first catalog model that exists in the cache directory AND
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
    Err(anyhow!(
        "{MODEL_PATH_ENV} is not set and no model was found in the \
         whisper-models cache directory; download a model via \
         `whisper-dictate models download tiny.en` or set \
         {MODEL_PATH_ENV} to point at a GGML whisper.cpp model file"
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
    use crate::test_env_lock::ENV_LOCK;

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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::remove_var(MODEL_PATH_ENV);
        // Pin cache to a non-existent dir so the cache fallback also finds nothing.
        let saved_cache = env::var_os(CACHE_ENV_VAR);
        env::set_var(CACHE_ENV_VAR, "/definitely/not/a/real/dir/xyz");

        let err = resolve_model_path_from_env().unwrap_err();
        assert!(
            err.to_string().contains(MODEL_PATH_ENV),
            "unexpected error: {err}"
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
    fn resolve_model_path_errors_when_env_blank() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::set_var(MODEL_PATH_ENV, "   ");

        let err = resolve_model_path_from_env().unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected error: {err}");

        match saved {
            Some(v) => env::set_var(MODEL_PATH_ENV, v),
            None => env::remove_var(MODEL_PATH_ENV),
        }
    }

    #[test]
    fn resolve_model_path_skips_corrupt_cached_file() {
        // P2: a file that exists but fails SHA-256 verification must be
        // skipped; the function must NOT return a corrupt cached path.
        // After the fix, `is_downloaded` gates the fallback, so a file with
        // wrong content is skipped and the function errors (no valid model).
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
        // Plant a file with wrong content — SHA-256 won't match the catalog.
        let corrupt_model = cache_subdir.join("ggml-tiny.en.bin");
        std::fs::write(&corrupt_model, b"corrupt-contents").unwrap();

        // Must fail: corrupt file is skipped, no valid model found.
        let err =
            resolve_model_path_from_env().expect_err("corrupt cached file must not be returned");
        assert!(
            err.to_string().contains(MODEL_PATH_ENV),
            "error must name the missing-model env var: {err}"
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
        let saved = env::var(MODEL_PATH_ENV).ok();
        env::set_var(MODEL_PATH_ENV, "/tmp/ggml-tiny.en.bin");

        let p = resolve_model_path_from_env().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/ggml-tiny.en.bin"));

        match saved {
            Some(v) => env::set_var(MODEL_PATH_ENV, v),
            None => env::remove_var(MODEL_PATH_ENV),
        }
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
