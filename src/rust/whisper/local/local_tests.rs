//! Tests for [`super`] (the `whisper::local` module) and [`super::wav`].
//!
//! Extracted from `local.rs` to keep module sizes under the 500-LOC cap;
//! wired in via `#[path = "local_tests.rs"] mod tests;` from `mod.rs`.

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
fn unwrap_err<T>(r: anyhow::Result<T>) -> anyhow::Error {
    match r {
        Ok(_) => panic!("expected an error, got Ok"),
        Err(e) => e,
    }
}

#[test]
fn new_rejects_missing_model() {
    let err = unwrap_err(LocalWhisper::new(std::path::Path::new(
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

    let whisper = LocalWhisper::new(std::path::Path::new(&model)).expect("load model");
    // Default to auto-detect for the spike: works for both `.en` and
    // multilingual models on a "hello world" recording. No initial prompt
    // — the dictionary hint is applied by the Python wiring layer.
    let text = whisper
        .transcribe_wav(std::path::Path::new(&wav), None, None)
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
