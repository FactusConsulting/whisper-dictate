//! Local Whisper inference via the [`whisper-rs`] (whisper.cpp) bindings.
//!
//! This is the **CPU-only spike** for roadmap issue #317 sub-task 1: prove
//! the integration works end-to-end (load a GGML/GGUF model, transcribe a
//! 16 kHz mono WAV, return the concatenated text) behind the
//! `whisper-rs-local` cargo feature.
//!
//! **Out of scope for this spike:** GPU backends, model download UI, idle
//! unload, Python-pipeline parity, runtime wiring. The runtime (`runtime.rs`
//! / `main.rs`) is intentionally untouched — those land in later sub-tasks.
//!
//! Enabling `whisper-rs-local` requires CMake and a C/C++ compiler on the
//! build host because whisper.cpp is compiled from source. See the README
//! "Local Whisper (experimental)" section.
//!
//! [`whisper-rs`]: https://crates.io/crates/whisper-rs

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Sample rate Whisper expects on its input PCM buffer (16 kHz mono).
pub const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

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
    /// Load a GGML/GGUF whisper.cpp model from disk (CPU-only).
    ///
    /// `model_path` should point to a file such as `ggml-small.en.bin` from
    /// the [ggerganov/whisper.cpp releases]. GPU is explicitly disabled in
    /// this spike — that lands in a later roadmap sub-task.
    ///
    /// [ggerganov/whisper.cpp releases]: https://huggingface.co/ggerganov/whisper.cpp
    pub fn new(model_path: &Path) -> Result<Self> {
        if !model_path.is_file() {
            return Err(anyhow!(
                "whisper model file not found: {}",
                model_path.display()
            ));
        }
        // whisper-rs takes the path as a &str (it hands it to whisper.cpp
        // which uses C strings internally). Surface a clean error rather
        // than panicking on non-UTF-8 paths.
        let model_str = model_path.to_str().ok_or_else(|| {
            anyhow!(
                "whisper model path is not valid UTF-8: {}",
                model_path.display()
            )
        })?;

        let mut params = WhisperContextParameters::default();
        params.use_gpu = false;

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
    pub fn transcribe_wav(&self, wav_path: &Path) -> Result<String> {
        let samples = decode_wav_16k_mono(wav_path)?;
        self.transcribe_samples(&samples)
    }

    /// Run inference on an already-decoded f32 PCM buffer (16 kHz mono,
    /// `[-1.0, 1.0]` range). Exposed for tests and the runnable example so
    /// they can build buffers without round-tripping through a WAV file.
    pub fn transcribe_samples(&self, samples: &[f32]) -> Result<String> {
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

/// Decode a WAV file into f32 mono samples, enforcing 16 kHz / 1 channel.
///
/// Public so the example binary and tests can use it without going through
/// `LocalWhisper`.
pub fn decode_wav_16k_mono(wav_path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(wav_path)
        .with_context(|| format!("failed to open WAV file {}", wav_path.display()))?;
    let spec = reader.spec();

    if spec.channels != 1 {
        return Err(anyhow!(
            "WAV must be mono (1 channel); {} has {} channels",
            wav_path.display(),
            spec.channels
        ));
    }
    if spec.sample_rate != WHISPER_SAMPLE_RATE_HZ {
        return Err(anyhow!(
            "WAV must be {} Hz; {} is {} Hz",
            WHISPER_SAMPLE_RATE_HZ,
            wav_path.display(),
            spec.sample_rate
        ));
    }

    // Normalize whatever the file stores into f32 in [-1.0, 1.0]. hound
    // exposes integer PCM as i32 (sign-extended from the actual bit depth)
    // and float PCM as f32; we cover the two we are likely to encounter
    // from any standard recorder.
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read float samples from {}", wav_path.display()))?,
        hound::SampleFormat::Int => {
            let max = i32::pow(2, spec.bits_per_sample as u32 - 1) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| {
                    format!("failed to read int samples from {}", wav_path.display())
                })?
        }
    };

    if samples.is_empty() {
        return Err(anyhow!(
            "WAV file {} contains no samples",
            wav_path.display()
        ));
    }
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Env vars pointing at a downloaded whisper.cpp model file and a
    /// matching "hello world" WAV. CI does not set these, so the heavy
    /// end-to-end test is skipped there; a developer running
    ///
    /// ```sh
    /// WHISPER_TEST_MODEL_PATH=/path/to/ggml-tiny.en.bin \
    /// WHISPER_TEST_WAV_PATH=/path/to/hello.wav \
    /// cargo test --features whisper-rs-local
    /// ```
    ///
    /// exercises the full path. We don't ship the model (30+ MB) or a real
    /// "hello world" recording in the repo — see the README "Local
    /// Whisper" section for where to grab one.
    const MODEL_ENV: &str = "WHISPER_TEST_MODEL_PATH";
    const WAV_ENV: &str = "WHISPER_TEST_WAV_PATH";

    #[test]
    fn decode_wav_rejects_wrong_sample_rate() {
        // Synthesize a 1-sample 8 kHz mono WAV in a tempdir and confirm the
        // decoder refuses it rather than silently feeding bogus data to
        // whisper.cpp.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("wrong_rate.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();

        let err = decode_wav_16k_mono(&path).unwrap_err();
        assert!(
            err.to_string().contains("16000 Hz"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decode_wav_rejects_stereo() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        w.write_sample(0i16).unwrap();
        w.write_sample(0i16).unwrap();
        w.finalize().unwrap();

        let err = decode_wav_16k_mono(&path).unwrap_err();
        assert!(err.to_string().contains("mono"), "unexpected error: {err}");
    }

    #[test]
    fn new_rejects_missing_model() {
        let err = LocalWhisper::new(Path::new("/definitely/not/a/real/model.bin")).unwrap_err();
        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
    }

    /// End-to-end: load a real model, transcribe a known "hello world" WAV,
    /// assert the transcript contains "hello". Skipped unless the developer
    /// opts in via the two env vars below because both the model (30+ MB)
    /// and a representative recording are too large/non-portable for the
    /// repo.
    #[test]
    fn transcribes_hello_world_when_model_available() {
        let (Ok(model), Ok(wav)) = (env::var(MODEL_ENV), env::var(WAV_ENV)) else {
            eprintln!(
                "skipping: set {MODEL_ENV} (GGML whisper model) and {WAV_ENV} \
                 (16 kHz mono 'hello world' WAV) to run"
            );
            return;
        };

        let whisper = LocalWhisper::new(Path::new(&model)).expect("load model");
        let text = whisper.transcribe_wav(Path::new(&wav)).expect("transcribe");
        assert!(
            text.to_lowercase().contains("hello"),
            "transcript missing 'hello': {text:?}"
        );
    }
}
