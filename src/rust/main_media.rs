//! Feature-gated media verb handlers for the CLI binary: `dictate-mic`,
//! the audio-capture self-test probe, Whisper model load, and the
//! transcribe file/server verbs. Each handler pair compiles a real
//! implementation behind its cargo feature and a stub that reports the
//! missing feature otherwise.
/// Feature-on path for `self-test audio-capture` — opens the cpal input
/// stream via [`whisper_dictate_app::audio::self_test::run_audio_capture_test`]
/// and prints either a JSON envelope or a plain summary. Returns Err (and
/// exits non-zero) when the report says the capture failed so CI trips.
#[cfg(feature = "audio-capture")]
pub(crate) fn handle_audio_capture_self_test(
    duration_ms: u64,
    device: String,
    json: bool,
    fail_on_silence: bool,
) -> anyhow::Result<()> {
    use whisper_dictate_app::audio::self_test::{run_audio_capture_test, AudioCaptureOptions};
    // Reject nonsense before we open a device. `--duration-ms 0` is a
    // vacuous "pass"; refuse loudly.
    if duration_ms == 0 {
        return Err(anyhow::anyhow!(
            "--duration-ms must be at least 1 (0 would be a vacuous pass)"
        ));
    }
    // Warn under 100 ms — cpal callback intervals on WASAPI can approach
    // 20-40 ms, so sub-100 ms runs risk zero callbacks and a false FAIL
    // for reasons other than a real regression. Not a hard cap; just a
    // one-line hint on stderr the caller can pipe away.
    if duration_ms < 100 {
        eprintln!(
            "warning: --duration-ms {duration_ms} is below the recommended 100ms floor \
             - cpal may not deliver even one callback in that window"
        );
    }
    let opts = AudioCaptureOptions {
        duration: std::time::Duration::from_millis(duration_ms),
        device,
        fail_on_silence,
    };
    let report = run_audio_capture_test(opts);
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.is_ok() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "self-test audio-capture failed (see report above for the specific error)"
        ))
    }
}

/// Feature-on path for `dictate-mic` — captures live mic audio through the
/// Rust VAD-free pipeline and drives the in-process `DictateSession`.
#[cfg(feature = "audio-capture")]
pub(crate) fn handle_dictate_mic(device: &str, seconds: f64, json: bool) -> anyhow::Result<()> {
    whisper_dictate_app::dictate::mic::handle_dictate_mic(device, seconds, json)
}

/// Stock-build stub: `dictate-mic` needs the cpal capture pipeline, which is
/// only compiled under `audio-capture`. Emit the same actionable rebuild
/// message shape as the sibling audio verbs and exit non-zero.
#[cfg(not(feature = "audio-capture"))]
pub(crate) fn handle_dictate_mic(_device: &str, _seconds: f64, _json: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "dictate-mic requires the `audio-capture` cargo feature; \
         rebuild with `cargo build --features audio-capture`"
    ))
}

/// Stock-build stub: the audio module isn't compiled in without the
/// `audio-capture` feature, so we can't open a cpal stream. Emit an
/// actionable rebuild message (matching the pattern the `ptt-wedge` and
/// `injection-idempotency` verbs use for their own feature gates) and
/// exit non-zero so CI / the smoke script pin-check trips.
#[cfg(not(feature = "audio-capture"))]
pub(crate) fn handle_audio_capture_self_test(
    _duration_ms: u64,
    _device: String,
    _json: bool,
    _fail_on_silence: bool,
) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "self-test audio-capture requires the `audio-capture` cargo feature - \
         rebuild with `cargo build --features audio-capture`"
    ))
}

/// Dispatch `self-test whisper-load`. Feature-gated: on a stock build we
/// return the same shape of "rebuild" error the sibling verbs return, so
/// the smoke script's `grep` on the message keeps working across verbs.
#[cfg(feature = "whisper-rs-local")]
pub(crate) fn handle_whisper_load(model: &str, json: bool) -> anyhow::Result<()> {
    use whisper_dictate_app::whisper::self_test::run_whisper_load_test;
    let report = run_whisper_load_test(model)?;
    if json {
        println!("{}", report.to_json());
    } else {
        print!("{}", report.to_plain());
    }
    if report.ok {
        Ok(())
    } else {
        // Non-zero exit so CI trips. The report already printed the
        // details; the tail keeps the error concise.
        Err(anyhow::anyhow!(
            "self-test whisper-load failed: {} ({})",
            report.error.unwrap_or_default(),
            report.error_kind.unwrap_or("unknown"),
        ))
    }
}

#[cfg(not(feature = "whisper-rs-local"))]
pub(crate) fn handle_whisper_load(_model: &str, _json: bool) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "self-test whisper-load requires the `whisper-rs-local` cargo feature - \
         rebuild with `cargo build --features whisper-rs-local` (needs cmake + a \
         C/C++ toolchain on the build host)"
    ))
}

/// Dispatch the hidden `transcribe-wav` sub-command.
///
/// Real implementation lives in `whisper::dispatch` and is only compiled in
/// behind the `whisper-rs-local` feature (which pulls in whisper.cpp + CMake).
/// In a stock build the binary still exposes the sub-command - keeping the
/// CLI surface stable across feature builds - but exits non-zero with a
/// clear "feature not compiled in" message.
///
/// `--probe` short-circuits before reading stdin or the model env var: it
/// exits 0 on a feature-enabled build and non-zero on a stock build, so
/// callers can cheaply check whether shelling out to this binary will
/// actually do whisper inference before committing to it for a dictation.
/// Note: ASCII-only strings here so the stderr message renders cleanly under
/// PowerShell / cmd.exe / hidden launchers and Rust UI subprocess logs
/// (AGENTS.md Windows-output rule).
#[cfg(feature = "whisper-rs-local")]
pub(crate) fn handle_transcribe_wav(probe: bool) -> anyhow::Result<()> {
    if probe {
        // Feature compiled in - probe succeeds without doing any work.
        return Ok(());
    }
    whisper_dictate_app::whisper::handle_transcribe_wav()
}

#[cfg(not(feature = "whisper-rs-local"))]
pub(crate) fn handle_transcribe_wav(_probe: bool) -> anyhow::Result<()> {
    // Same error for probe and real call: any non-zero exit reads as
    // "Rust backend unavailable, fall back".
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the Rust transcription backend is \
         unavailable - unset VOICEPI_TRANSCRIBE_BACKEND or install a build \
         with the feature enabled"
    ))
}

/// Long-running in-process Whisper worker. See
/// [`whisper::dispatch::handle_transcribe_server`] for the wire protocol
/// and the per-request error contract; the stock-build fallback mirrors
/// `handle_transcribe_wav` above so both subcommands share the same
/// "backend unavailable" exit code.
#[cfg(feature = "whisper-rs-local")]
pub(crate) fn handle_transcribe_server() -> anyhow::Result<()> {
    whisper_dictate_app::whisper::handle_transcribe_server()
}

#[cfg(not(feature = "whisper-rs-local"))]
pub(crate) fn handle_transcribe_server() -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "this build of whisper-dictate was compiled without the \
         `whisper-rs-local` feature; the long-running transcribe-server \
         is unavailable - install a build with the feature enabled"
    ))
}

#[cfg(test)]
mod tests {
    //! Stock-build stub contracts: the CLI surface stays stable across
    //! feature builds, and every stub must fail closed with an error that
    //! names the missing cargo feature so callers fall back cleanly.

    /// The dictate-mic stub must refuse with an actionable rebuild hint.
    #[cfg(not(feature = "audio-capture"))]
    #[test]
    fn dictate_mic_stub_error_names_the_missing_feature() {
        let err = super::handle_dictate_mic("default", 1.0, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("audio-capture"), "err = {err}");
    }

    /// The whisper-load stub must refuse with an actionable rebuild hint.
    #[cfg(not(feature = "whisper-rs-local"))]
    #[test]
    fn whisper_load_stub_error_names_the_missing_feature() {
        let err = super::handle_whisper_load("tiny", false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("whisper-rs-local"), "err = {err}");
    }

    /// The transcribe-wav stub must fail closed for probe and real calls
    /// alike so callers read any non-zero exit as "backend unavailable".
    #[cfg(not(feature = "whisper-rs-local"))]
    #[test]
    fn transcribe_wav_stub_fails_closed() {
        assert!(super::handle_transcribe_wav(false).is_err());
        assert!(super::handle_transcribe_wav(true).is_err());
    }
}
