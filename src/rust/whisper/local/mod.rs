//! Local Whisper inference via the [`whisper-rs`] (whisper.cpp) bindings.
//!
//! Native local inference path. Compiled in only when the `whisper-rs-local`
//! cargo feature is enabled (the feature pulls whisper.cpp + CMake into the
//! build).
//!
//! **Model format:** only GGML (`ggml-*.bin` from the whisper.cpp release
//! index) is supported. whisper.cpp does not yet read llama.cpp's newer
//! GGUF container; loading a GGUF file fails at startup with a clean error
//! from [`LocalWhisper::new`] rather than a cryptic FFI message.
//!
//! Model files are pointed at via [`super::dispatch::MODEL_PATH_ENV`]
//! (`VOICEPI_WHISPER_MODEL_PATH`) or downloaded into the user-cache directory
//! via [`super::model_manager`].
//!
//! Enabling `whisper-rs-local` requires CMake and a C/C++ compiler on the
//! build host because whisper.cpp is compiled from source. See the README
//! "Local Whisper (experimental)" section.
//!
//! [`whisper-rs`]: https://crates.io/crates/whisper-rs
//!
//! ## Module layout
//! - [`wav`] — WAV decoding helpers (`decode_wav_16k_mono`, `WHISPER_SAMPLE_RATE_HZ`)
//! - This file — `LocalWhisper` struct, inference, GGUF guard, and
//!   `load_catch_unwind` wrapper for reporting whisper.cpp load failures.
//! - [`preload`] — background load primitive (`Preloader`, `LoadStatus`) used
//!   by the `whisper-load` self-test and other explicit diagnostic callers.
//! - [`log_tap`] — installs a whisper.cpp log callback so the
//!   `whisper_backend_init_gpu: ...` model-load lines become a
//!   machine-readable [`super::accel::Accel`] verdict instead of stderr
//!   text nobody can act on. This is what makes a silent GPU->CPU
//!   fallback visible on every utterance record.

mod log_tap;
pub mod preload;

pub use preload::{load_blocking, LoadStatus, Preloader};

// WAV decode helpers live in the unconditional `whisper::wav` module so they
// are compiled and tested without the `whisper-rs-local` CMake dependency.
// `decode_wav_16k_mono` is what `LocalWhisper::transcribe_wav` actually calls;
// the sample-rate constant is re-exported from `whisper::mod.rs` directly so
// callers don't need to go through `whisper::local`.
pub use super::wav::decode_wav_16k_mono;

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::sync::Arc;
use whisper_rs::{
    get_lang_str, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

use super::accel;
use super::gpu::{self, GpuPolicy};

/// Failure envelope returned by [`LocalWhisper::load_catch_unwind`].
///
/// Two variants because callers need different diagnostics:
/// - `Errored` is the normal Rust `Result::Err` shape — a missing file, a
///   GGUF-not-GGML mismatch, or a whisper-rs API error. Callers log the
///   actionable native failure.
/// - `Panicked` is a caught unwind from inside whisper.cpp (typically an
///   OOM on model load, or a `libc::abort()` that Rust's panic runtime
///   catches). Callers log at ERROR because this is worth a stack-trace-level
///   signal while keeping the native process alive.
#[derive(Debug)]
pub enum LoadFailure {
    /// A clean `anyhow::Error` from the load path. Wrapped in `Arc` so
    /// `LoadStatus::Failed` can be `Clone`d cheaply across preloader
    /// pollers without dragging in an `anyhow::Error: Clone` bound
    /// (which doesn't exist upstream).
    Errored(Arc<anyhow::Error>),
    /// Caught panic message from `catch_unwind`. Kept as a string
    /// because `Box<dyn Any>` isn't `Send + Sync`-friendly for storing
    /// in the shared preloader state. The typical shape is
    /// `"whisper.cpp: failed to allocate ..."` or the raw C++ message.
    Panicked(String),
}

impl LoadFailure {
    /// Convenience for callers building a `LoadFailure::Errored` from an
    /// `anyhow::Error` directly.
    pub fn errored(err: anyhow::Error) -> Self {
        LoadFailure::Errored(Arc::new(err))
    }

    /// Machine-readable label for JSON envelopes. Kept stable so log
    /// scrapers can grep for `"kind": "panicked"` when hunting the
    /// OOM class of bug this wrapper was added for.
    pub fn kind(&self) -> &'static str {
        match self {
            LoadFailure::Errored(_) => "errored",
            LoadFailure::Panicked(_) => "panicked",
        }
    }

    /// Rendered message for humans and JSON envelopes.
    pub fn message(&self) -> String {
        match self {
            LoadFailure::Errored(e) => format!("{e:#}"),
            LoadFailure::Panicked(msg) => msg.clone(),
        }
    }
}

impl Clone for LoadFailure {
    fn clone(&self) -> Self {
        match self {
            LoadFailure::Errored(e) => LoadFailure::Errored(Arc::clone(e)),
            LoadFailure::Panicked(msg) => LoadFailure::Panicked(msg.clone()),
        }
    }
}

impl std::fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind(), self.message())
    }
}

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

        let use_gpu = gpu::should_use_gpu(policy);
        let params = WhisperContextParameters {
            use_gpu,
            ..Default::default()
        };

        // Forget the PREVIOUS load's verdict, record what we EXPECT from
        // this one, then tap whisper.cpp's own log so what actually
        // happens can overrule it. All three must happen BEFORE
        // `new_with_params`: the backend-selection lines are emitted
        // inside that call and a callback installed afterwards would
        // miss the only evidence there is that a Vulkan-linked binary
        // fell back to CPU.
        //
        // `begin_model_load` (rather than `set_planned` alone) is what
        // makes an idle-unload + reload that LOSES the GPU visible: the
        // rank ratchet inside `record` would otherwise keep the first
        // load's `vulkan` forever. Claude + Codex P2 #687.
        let observer = accel::global();
        observer.begin_model_load(accel::planned_from_policy(policy));
        log_tap::install();

        let ctx = WhisperContext::new_with_params(model_str, params).with_context(|| {
            format!("failed to load whisper model from {}", model_path.display())
        })?;
        // One line per successful load naming the compute path whisper.cpp
        // actually took. Emitted here (not at startup) because this is the
        // first moment the answer exists -- the model loads lazily on the
        // first utterance, long after the startup banner.
        crate::diag::log!(
            "[whisper] model loaded: requested_gpu={} accel={} (planned={})",
            use_gpu,
            observer.resolved().as_str(),
            observer.planned().as_str()
        );
        Ok(Self { ctx })
    }

    /// Load a GGML whisper.cpp model with a caught unwind around the
    /// whisper.cpp allocation, so an OOM inside the C++ tensor allocator
    /// returns [`LoadFailure::Panicked`] instead of aborting the process.
    ///
    /// Runs on any thread — the diagnostic preloader spawns a dedicated
    /// background worker, but tests and the self-test verb can also invoke
    /// this directly for a synchronous "load with safety net" shape.
    ///
    /// Non-panic errors (missing file, GGUF file, whisper-rs API error)
    /// come back as [`LoadFailure::Errored`] preserving the original
    /// `anyhow` context. Panic messages are stringified via the standard
    /// `Any::downcast_ref::<&str>()` / `<String>()` dance so the caller
    /// gets *something* readable even for exotic panic payloads.
    pub fn load_catch_unwind(model_path: &Path) -> Result<Self, LoadFailure> {
        let path = model_path.to_path_buf();
        // AssertUnwindSafe is safe here: LocalWhisper::new does not
        // capture any shared mutable state — it takes an owned path and
        // produces a fresh WhisperContext. Even if whisper.cpp leaves
        // static/global state in an inconsistent shape after a caught
        // panic (unlikely for a load-time OOM), we treat the resulting
        // process as "must not use whisper-rs again this session" —
        // the caller surfaces the native failure with diagnostics.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || Self::new(&path)));
        match result {
            Ok(Ok(model)) => Ok(model),
            Ok(Err(err)) => Err(LoadFailure::errored(err)),
            Err(panic_payload) => {
                let msg = panic_payload_to_string(&panic_payload);
                Err(LoadFailure::Panicked(msg))
            }
        }
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
        self.transcribe_samples_with_language(samples, language, initial_prompt)
            .map(|(text, _)| text)
    }

    /// Run inference and return both text and Whisper's detected language.
    /// An explicit language hint is preserved; auto-detect reads the language
    /// ID from the completed Whisper state so downstream post-processing can
    /// retain the utterance's actual language.
    pub fn transcribe_samples_with_language(
        &self,
        samples: &[f32],
        language: Option<&str>,
        initial_prompt: Option<&str>,
    ) -> Result<(String, Option<String>)> {
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
        // Translation is an opt-in Whisper task. Pin transcription even when
        // auto-detection is enabled, so the output stays in the spoken language.
        params.set_translate(false);

        // Language hint: whisper-rs defaults to "en", which silently mis-
        // transcribes non-English audio on multilingual models. Pass the
        // caller's choice through; `None` and `Some("auto")` both mean
        // "auto-detect" per whisper.cpp's convention.
        let lang_for_whisper = normalize_language_hint(language);
        params.set_language(lang_for_whisper);
        // `language=None` already enables auto-detection in whisper.cpp and
        // then continues into transcription. Do not set
        // `detect_language=true` here: that flag is the detection-only API
        // and `whisper_full` returns immediately after logging the detected
        // language, leaving the segment list empty. This used to make every
        // Auto-language utterance appear as `status=no_text reason=empty`,
        // while an explicit language (for example `en`) worked normally.

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

        let detected_language =
            resolved_language(lang_for_whisper, state.full_lang_id_from_state());

        let mut out = String::new();
        for segment in state.as_iter() {
            let text = segment
                .to_str_lossy()
                .context("failed to read whisper segment text")?;
            out.push_str(&text);
        }
        Ok((out, detected_language))
    }
}

/// Normalize the public language setting to whisper.cpp's language hint.
///
/// `None` and `"auto"` deliberately remain a null hint. whisper.cpp uses
/// that null hint to auto-detect and then continue decoding; its separate
/// `detect_language` flag is detection-only and must stay disabled for a
/// transcription request.
fn normalize_language_hint(language: Option<&str>) -> Option<&str> {
    match language {
        None | Some("auto") => None,
        Some(other) => Some(other),
    }
}

fn resolved_language(language_hint: Option<&str>, detected_id: i32) -> Option<String> {
    language_hint
        .map(str::to_owned)
        .or_else(|| get_lang_str(detected_id).map(str::to_owned))
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
             models - download a `ggml-*.bin` from https://huggingface.co/ggerganov/whisper.cpp",
            model_path.display()
        ));
    }
    Ok(())
}

/// Best-effort stringification of a caught panic payload.
///
/// `catch_unwind` returns a `Box<dyn Any + Send>` — the shape of the
/// payload depends on how the panic was raised. `panic!("msg")` gives a
/// `String`; `panic!("{}", x)` also gives a `String`; a bare
/// `&'static str` panic gives `&str`. C++ code that unwinds into Rust
/// via a foreign-function-interface panic hook typically ends up as
/// `String`. Anything else falls back to a sentinel so the caller
/// always has a message to log.
fn panic_payload_to_string(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else {
        "whisper.cpp panic with non-string payload (likely an OOM inside \
         whisper.cpp's tensor allocator - install more RAM or pick a smaller \
         model)"
            .to_owned()
    }
}

#[cfg(test)]
#[path = "catch_unwind_tests.rs"]
mod catch_unwind_tests;

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
