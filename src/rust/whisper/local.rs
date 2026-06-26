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
    /// Load a GGML whisper.cpp model from disk (CPU-only).
    ///
    /// `model_path` should point to a file such as `ggml-small.en.bin` from
    /// the [ggerganov/whisper.cpp releases]. GPU is explicitly disabled in
    /// this spike — that lands in a later roadmap sub-task.
    ///
    /// **Only GGML is supported.** whisper.cpp has not yet picked up
    /// llama.cpp's GGUF container, so a GGUF file is rejected up front by a
    /// magic-bytes check rather than failing with a cryptic FFI error.
    ///
    /// [ggerganov/whisper.cpp releases]: https://huggingface.co/ggerganov/whisper.cpp
    pub fn new(model_path: &Path) -> Result<Self> {
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
            use_gpu: false,
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
    let mut samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read float samples from {}", wav_path.display()))?,
        hound::SampleFormat::Int => {
            // Guard against malformed/exotic WAVs claiming bit depths that
            // would either overflow our shift (`1i64 << 63` panics in debug)
            // or that hound can't decode into i32 anyway. WAV PCM tops out
            // at 32-bit integer in practice; reject anything wider with a
            // clean error instead of crashing.
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                return Err(anyhow!(
                    "integer WAV bit depth {} is not supported (must be 1..=32); {}",
                    spec.bits_per_sample,
                    wav_path.display()
                ));
            }
            // Compute the full-scale magnitude in i64 to avoid overflow when
            // bits_per_sample == 32: `i32::pow(2, 31)` panics in debug and
            // wraps to i32::MIN in release, which would silently invert every
            // sample. i64 has plenty of headroom for any hound-supported depth.
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| {
                    format!("failed to read int samples from {}", wav_path.display())
                })?
        }
    };

    // Float WAVs are *spec'd* to range in [-1.0, 1.0] but real-world files
    // (loud masters, 0-dBFS exports, mixdowns that preserve headroom) often
    // ship outside that range. Whisper expects normalised audio; out-of-range
    // peaks produce silently wrong transcriptions on otherwise valid input.
    //
    // Reject NaN/Inf up front (we can't meaningfully scale them), then
    // divide by max-abs if any sample exceeds 1.0. We don't *amplify* quiet
    // files (max < 1.0) — they may be intentionally low and whisper handles
    // silence-padded windows fine. Integer paths already produce values in
    // [-1, 1] by construction, but the normalisation pass is cheap and
    // protects against a future int decoder change too.
    if matches!(spec.sample_format, hound::SampleFormat::Float) {
        let mut max_abs: f32 = 0.0;
        for &s in &samples {
            if !s.is_finite() {
                return Err(anyhow!(
                    "float WAV {} contains a non-finite sample (NaN/Inf)",
                    wav_path.display()
                ));
            }
            let a = s.abs();
            if a > max_abs {
                max_abs = a;
            }
        }
        if max_abs > 1.0 {
            for s in &mut samples {
                *s /= max_abs;
            }
        }
    }

    if samples.is_empty() {
        return Err(anyhow!(
            "WAV file {} contains no samples",
            wav_path.display()
        ));
    }
    Ok(samples)
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

    /// Small helper: `Result::unwrap_err` requires the `Ok` value (here a
    /// `LocalWhisper` wrapping a non-Debug `WhisperContext`) to implement
    /// `Debug`; this avoids the bound without forcing `Debug` onto our
    /// public type.
    fn unwrap_err<T>(r: Result<T>) -> anyhow::Error {
        match r {
            Ok(_) => panic!("expected an error, got Ok"),
            Err(e) => e,
        }
    }

    #[test]
    fn new_rejects_missing_model() {
        let err = unwrap_err(LocalWhisper::new(Path::new(
            "/definitely/not/a/real/model.bin",
        )));
        assert!(
            err.to_string().contains("not found"),
            "unexpected error: {err}"
        );
    }

    /// Regression: writing a positive 16-bit PCM sample must decode to a
    /// positive f32. The earlier `i32::pow(2, bits-1) as f32` worked for
    /// 16-bit but is here as a sanity baseline so a future change can't
    /// silently flip sign across the common-case path.
    #[test]
    fn decode_wav_16bit_int_preserves_sign() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pos16.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        // Large positive amplitude near full scale.
        w.write_sample(16_000i16).unwrap();
        w.write_sample(8_000i16).unwrap();
        w.finalize().unwrap();

        let samples = decode_wav_16k_mono(&path).expect("decode 16-bit WAV");
        assert_eq!(samples.len(), 2);
        assert!(
            samples[0] > 0.0,
            "expected positive sample, got {}",
            samples[0]
        );
        assert!(
            samples[1] > 0.0,
            "expected positive sample, got {}",
            samples[1]
        );
        // Roughly 16000 / 32768 ≈ 0.488 — allow slack for the conversion.
        assert!(
            (0.3..0.7).contains(&samples[0]),
            "amplitude off: {}",
            samples[0]
        );
    }

    /// Regression for the i32 overflow bug at the previous
    /// `i32::pow(2, bits - 1)` line: 32-bit PCM panicked in debug and
    /// wrapped to i32::MIN (negative) in release, silently inverting every
    /// sample. Decoding a known-positive 32-bit sample must produce a
    /// positive f32.
    #[test]
    fn decode_wav_32bit_int_does_not_overflow_or_flip_sign() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("pos32.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        // Quarter-scale positive samples — large enough that any sign flip
        // or overflow shows up as obviously-wrong magnitude.
        let amp: i32 = 1 << 29;
        w.write_sample(amp).unwrap();
        w.write_sample(amp / 2).unwrap();
        w.finalize().unwrap();

        let samples = decode_wav_16k_mono(&path).expect("decode 32-bit WAV");
        assert_eq!(samples.len(), 2);
        assert!(
            samples[0] > 0.0,
            "32-bit sample sign flipped: {}",
            samples[0]
        );
        assert!(
            samples[1] > 0.0,
            "32-bit sample sign flipped: {}",
            samples[1]
        );
        // 2^29 / 2^31 = 0.25 — within range, well below 1.0.
        assert!(
            (0.2..0.3).contains(&samples[0]),
            "amplitude off: {}",
            samples[0]
        );
        assert!(
            (0.1..0.15).contains(&samples[1]),
            "amplitude off: {}",
            samples[1]
        );
    }

    /// Loading a path whose file name contains non-ASCII characters must
    /// not panic or silently mangle the path. On Windows valid UTF-8 paths
    /// must succeed (we only error on the very rare unpaired-surrogate
    /// case); on Unix `tempdir` + non-ASCII file names round-trip fine.
    /// Either way we expect a clean "not found"-style error when the file
    /// doesn't exist, not a panic, and a "model file not found" message
    /// since we point at a non-existent path under the non-ASCII directory.
    #[test]
    fn new_handles_non_ascii_path_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("æøå-日本語-model.bin");
        // File does not exist — we just want to confirm the error message
        // surfaces the path without panicking on Display / to_str.
        let err = unwrap_err(LocalWhisper::new(&path));
        let msg = err.to_string();
        assert!(
            msg.contains("not found"),
            "expected missing-file error, got: {msg}"
        );
    }

    /// GGUF files start with the magic bytes `GGUF`; whisper.cpp can't read
    /// them yet and would otherwise produce an opaque FFI error.
    #[test]
    fn new_rejects_gguf_model_with_clear_error() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fake.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        // Magic + a bit of filler so the file exists with the right header.
        f.write_all(b"GGUF\x00\x00\x00\x00more bytes").unwrap();
        f.sync_all().unwrap();
        drop(f);

        let err = unwrap_err(LocalWhisper::new(&path));
        let msg = err.to_string();
        assert!(
            msg.contains("GGUF") && msg.contains("GGML"),
            "expected GGUF/GGML guidance, got: {msg}"
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
        // Default to auto-detect for the spike: works for both `.en` and
        // multilingual models on a "hello world" recording. No initial prompt
        // — the dictionary hint is applied by the Python wiring layer.
        let text = whisper
            .transcribe_wav(Path::new(&wav), None, None)
            .expect("transcribe");
        assert!(
            text.to_lowercase().contains("hello"),
            "transcript missing 'hello': {text:?}"
        );
    }

    /// Float WAVs above 0 dBFS (loud masters, headroom-preserving exports)
    /// must be scaled down to [-1, 1] so whisper sees the normalised range
    /// it expects. The earlier code passed the raw values straight through,
    /// silently mis-transcribing on peak-3.0 inputs.
    #[test]
    fn decode_wav_normalizes_out_of_range_float_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("loud_float.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        // Peak at +3.0, also write a smaller positive sample so we can
        // confirm relative dynamics are preserved (not just clipped).
        w.write_sample(3.0f32).unwrap();
        w.write_sample(-1.5f32).unwrap();
        w.write_sample(0.75f32).unwrap();
        w.finalize().unwrap();

        let samples = decode_wav_16k_mono(&path).expect("decode float WAV");
        assert_eq!(samples.len(), 3);
        for (i, &s) in samples.iter().enumerate() {
            assert!(
                s.abs() <= 1.0 + f32::EPSILON,
                "sample {i} = {s} not in [-1, 1] after normalisation"
            );
        }
        // 3.0 → 1.0 (peak), -1.5 → -0.5, 0.75 → 0.25. Within rounding slack.
        assert!((samples[0] - 1.0).abs() < 1e-5, "peak: {}", samples[0]);
        assert!((samples[1] + 0.5).abs() < 1e-5, "mid:  {}", samples[1]);
        assert!((samples[2] - 0.25).abs() < 1e-5, "low:  {}", samples[2]);
    }

    /// Quiet float WAVs (all samples below 1.0) must NOT be amplified — the
    /// low level may be intentional and whisper handles silence-padded
    /// windows just fine. Only out-of-range peaks trigger renormalisation.
    #[test]
    fn decode_wav_does_not_amplify_quiet_float_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("quiet_float.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        w.write_sample(0.1f32).unwrap();
        w.write_sample(-0.05f32).unwrap();
        w.finalize().unwrap();

        let samples = decode_wav_16k_mono(&path).expect("decode quiet float WAV");
        assert!((samples[0] - 0.1).abs() < 1e-6, "amplified: {}", samples[0]);
        assert!(
            (samples[1] + 0.05).abs() < 1e-6,
            "amplified: {}",
            samples[1]
        );
    }

    /// Non-finite float samples (NaN/Inf) can't be meaningfully normalised
    /// or transcribed — reject them with a clean error instead of feeding
    /// poisoned input to whisper.cpp.
    #[test]
    fn decode_wav_rejects_non_finite_float_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nan_float.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        w.write_sample(0.5f32).unwrap();
        w.write_sample(f32::NAN).unwrap();
        w.finalize().unwrap();

        let err = decode_wav_16k_mono(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("non-finite"),
            "expected non-finite rejection, got: {msg}"
        );
    }

    /// Synthesise a minimal WAV header advertising a 64-bit integer PCM
    /// depth so the decoder's bit-depth guard fires before the unsupported
    /// shift `1i64 << 63`. hound's writer won't produce this (it caps at
    /// 32-bit), so we hand-craft the RIFF chunks. The exact data payload
    /// doesn't matter — we only need to get past header parsing into the
    /// `samples::<i32>()` path.
    #[test]
    fn decode_wav_rejects_oversized_integer_bit_depth() {
        use std::io::Write;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("huge_bits.wav");
        let mut f = std::fs::File::create(&path).unwrap();
        // RIFF header
        f.write_all(b"RIFF").unwrap();
        f.write_all(&36u32.to_le_bytes()).unwrap(); // file size - 8 (rough)
        f.write_all(b"WAVE").unwrap();
        // fmt chunk: PCM, 1 channel, 16 kHz, 64 bits per sample
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap(); // chunk size
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM (integer)
        f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
        f.write_all(&16_000u32.to_le_bytes()).unwrap(); // sample rate
        f.write_all(&128_000u32.to_le_bytes()).unwrap(); // byte rate (placeholder)
        f.write_all(&8u16.to_le_bytes()).unwrap(); // block align
        f.write_all(&64u16.to_le_bytes()).unwrap(); // bits per sample — the trap
                                                    // data chunk (empty)
        f.write_all(b"data").unwrap();
        f.write_all(&0u32.to_le_bytes()).unwrap();
        f.sync_all().unwrap();
        drop(f);

        // hound may reject the header itself (it does not advertise 64-bit
        // int support) — that's still a clean error, not a panic. Accept
        // either our explicit guard message or hound's own rejection.
        let err = decode_wav_16k_mono(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("bit depth") || msg.contains("bits") || msg.contains("WAV"),
            "expected a clean bit-depth/format error, got: {msg}"
        );
    }
}
