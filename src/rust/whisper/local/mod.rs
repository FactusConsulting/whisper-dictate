//! Local Whisper inference via the [`whisper-rs`] (whisper.cpp) bindings.
//!
//! CPU-only inference path for roadmap issue #317. Compiled in only when the
//! `whisper-rs-local` cargo feature is enabled (the feature pulls
//! whisper.cpp + CMake into the build).
//!
//! **Model format:** only GGML (`ggml-*.bin` from the whisper.cpp release
//! index) is supported. whisper.cpp does not yet read llama.cpp's newer
//! GGUF container; loading a GGUF file fails at startup with a clean error
//! from [`LocalWhisper::new`] rather than a cryptic FFI message.
//!
//! Model files are pointed at via [`super::dispatch::MODEL_PATH_ENV`]
//! (`VOICEPI_WHISPER_MODEL_PATH`) or downloaded into the user-cache directory
//! via [`super::model_manager`] (Wave 7-B).
//!
//! Enabling `whisper-rs-local` requires CMake and a C/C++ compiler on the
//! build host because whisper.cpp is compiled from source. See the README
//! "Local Whisper (experimental)" section.
//!
//! [`whisper-rs`]: https://crates.io/crates/whisper-rs
//!
//! ## Module layout
//! - [`wav`] — WAV decoding helpers (`decode_wav_16k_mono`, `WHISPER_SAMPLE_RATE_HZ`)
//! - This file — `LocalWhisper` struct, inference, GGUF guard

// WAV decode helpers live in the unconditional `whisper::wav` module so they
// are compiled and tested without the `whisper-rs-local` CMake dependency.
// `decode_wav_16k_mono` is what `LocalWhisper::transcribe_wav` actually calls;
// the sample-rate constant is re-exported from `whisper::mod.rs` directly so
// callers don't need to go through `whisper::local`.
pub use super::wav::decode_wav_16k_mono;

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use super::gpu::{self, GpuPolicy};

/// Loaded whisper.cpp model + a per-call inference helper.
///
/// One `LocalWhisper` owns the model weights; each `transcribe_wav` call
/// allocates a fresh whisper state so it is safe to reuse the instance for
/// multiple files (sequentially — the spike does not concern itself with
/// concurrent inference yet).
pub struct LocalWhisper {
    ctx: WhisperContext,
}

impl LocalWhisper {
    /// Load a GGML whisper.cpp model from disk, picking GPU vs CPU according
    /// to `VOICEPI_WHISPER_GPU` (see [`super::gpu::GPU_ENV`]).
    ///
    /// `model_path` should point to a file such as `ggml-small.en.bin` from
    /// the [ggerganov/whisper.cpp releases]. The effective `use_gpu` is the
    /// product of three things: the env-var policy, the compiled-in backend
    /// feature (`whisper-rs-vulkan` as of Wave 7-C), and the resolution
    /// rules in [`super::gpu::should_use_gpu`].
    ///
    /// For deterministic tests use [`Self::with_policy`] which takes the
    /// policy explicitly instead of reading the env.
    ///
    /// **Only GGML is supported.** whisper.cpp has not yet picked up
    /// llama.cpp's GGUF container, so a GGUF file is rejected up front by a
    /// magic-bytes check rather than failing with a cryptic FFI error.
    ///
    /// [ggerganov/whisper.cpp releases]: https://huggingface.co/ggerganov/whisper.cpp
    pub fn new(model_path: &Path) -> Result<Self> {
        let policy = gpu::parse_gpu_policy_from_env()?;
        Self::with_policy(model_path, policy)
    }

    /// Load a GGML whisper.cpp model with an explicit [`GpuPolicy`].
    ///
    /// Bypasses the env-var read in [`Self::new`] so tests and callers that
    /// already know the desired policy can pin it. The same compiled-in
    /// feature gate from [`super::gpu::should_use_gpu`] still applies, so a
    /// `Vulkan` policy on a build without `whisper-rs-vulkan` silently falls
    /// back to CPU rather than erroring (matching the runtime "best effort"
    /// promise of the env-var path).
    pub fn with_policy(model_path: &Path, policy: GpuPolicy) -> Result<Self> {
        if !model_path.is_file() {
            return Err(anyhow!(
                "whisper model file not found: {}",
                model_path.display()
            ));
        }
        reject_gguf_model(model_path)?;
        // whisper-rs takes the path as a &str (it hands it to whisper.cpp
        // which uses C strings internally). Surface a clean error rather
        // than panicking on non-UTF-8 paths. On Windows that means file
        // names with unpaired surrogates can't be loaded — whisper.cpp's
        // C API has the same limitation, so this is documenting reality,
        // not narrowing it.
        let model_str = model_path.to_str().ok_or_else(|| {
            anyhow!(
                "whisper model path is not valid UTF-8: {}",
                model_path.display()
            )
        })?;

        let params = WhisperContextParameters {
            use_gpu: gpu::should_use_gpu(policy),
            ..Default::default()
        };

        let ctx = WhisperContext::new_with_params(model_str, params).with_context(|| {
            format!("failed to load whisper model from {}", model_path.display())
        })?;
        Ok(Self { ctx })
    }

    /// Decode a 16 kHz mono PCM WAV file and run Whisper inference on it.
    ///
    /// The WAV must be exactly 16 kHz, single-channel, integer or float PCM
    /// (we convert to `f32` in [-1.0, 1.0]). Any other shape is rejected
    /// with a descriptive error rather than being silently resampled —
    /// resampling is a runtime-wiring concern and out of scope for the
    /// library-level spike.
    ///
    /// `language` controls whisper.cpp's language hint:
    /// - `None` or `Some("auto")` — let whisper.cpp auto-detect (multilingual
    ///   models only; the `.en` models are English-only regardless).
    /// - `Some("en")`, `Some("da")`, … — force the given BCP-47-ish code
    ///   that whisper.cpp recognises. Invalid codes surface as a clean
    ///   inference error from whisper.cpp rather than silently transcribing
    ///   as English.
    ///
    /// `initial_prompt` is an optional context hint fed to whisper.cpp before
    /// the first decode window (the same `--prompt` knob the whisper.cpp CLI
    /// exposes). Pass `None` to disable; an empty `Some("")` is also treated as
    /// `None` so the caller can plumb an unconditional `Option<&str>` derived
    /// from upstream config without an explicit empty-string check. Used by
    /// the Python wiring layer to feed the dictionary-derived term hint that
    /// `vp_transcribe.py` already builds.
    pub fn transcribe_wav(
        &self,
        wav_path: &Path,
        language: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        let samples = decode_wav_16k_mono(wav_path)?;
        self.transcribe_samples(&samples, language, initial_prompt)
    }

    /// Run inference on an already-decoded f32 PCM buffer (16 kHz mono,
    /// `[-1.0, 1.0]` range). Exposed for tests and the runnable example so
    /// they can build buffers without round-tripping through a WAV file.
    ///
    /// See [`Self::transcribe_wav`] for `language` and `initial_prompt`
    /// semantics.
    pub fn transcribe_samples(
        &self,
        samples: &[f32],
        language: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<String> {
        if samples.is_empty() {
            return Err(anyhow!("cannot transcribe an empty audio buffer"));
        }

        let mut state = self
            .ctx
            .create_state()
            .context("failed to create whisper state")?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        // Keep the spike output minimal: no progress / realtime / timestamp
        // prints to stderr from whisper.cpp itself. The caller can re-enable
        // these once we have runtime telemetry plumbing.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        // Language hint: whisper-rs defaults to "en", which silently mis-
        // transcribes non-English audio on multilingual models. Pass the
        // caller's choice through; `None` and `Some("auto")` both mean
        // "auto-detect" per whisper.cpp's convention.
        let lang_for_whisper = match language {
            None => None,
            Some("auto") => None,
            Some(other) => Some(other),
        };
        params.set_language(lang_for_whisper);
        if lang_for_whisper.is_none() {
            // Belt and braces: setting language to None already triggers
            // auto-detect, but enabling the explicit flag matches whisper.cpp
            // examples and is safer if the upstream default ever changes.
            params.set_detect_language(true);
        }

        // Optional initial prompt. whisper.cpp tokenises this and seeds the
        // decoder with the tokens, biasing rare-word recognition (jargon,
        // names, custom dictionary terms). Empty strings are dropped so a
        // caller plumbing an unconditional `Option<&str>` from config doesn't
        // ship a useless empty prompt that wastes a tokenisation pass.
        if let Some(prompt) = initial_prompt {
            if !prompt.is_empty() {
                params.set_initial_prompt(prompt);
            }
        }

        state
            .full(params, samples)
            .context("whisper inference (state.full) failed")?;

        let mut out = String::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .context("failed to read whisper segment text")?;
            out.push_str(&text);
        }
        Ok(out)
    }
}

/// Reject GGUF model files with a friendly error.
///
/// whisper.cpp's loader expects the GGML container; GGUF (llama.cpp's newer
/// format) starts with the magic bytes `GGUF` and currently fails inside
/// the FFI with an opaque message. Catching it here lets us point users at
/// the right model index.
fn reject_gguf_model(model_path: &Path) -> Result<()> {
    use std::fs::File;
    use std::io::Read;

    let mut head = [0u8; 4];
    let mut f = File::open(model_path)
        .with_context(|| format!("failed to open whisper model file {}", model_path.display()))?;
    // A short read here means the file is smaller than the magic header —
    // not a GGUF, let whisper.cpp produce its own (perhaps clearer) error.
    if f.read(&mut head).unwrap_or(0) == 4 && &head == b"GGUF" {
        return Err(anyhow!(
            "{} is a GGUF model; whisper.cpp (via whisper-rs) only loads GGML \
             models — download a `ggml-*.bin` from https://huggingface.co/ggerganov/whisper.cpp",
            model_path.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
