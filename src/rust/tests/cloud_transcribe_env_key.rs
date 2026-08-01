//! The STT API key must cross the process boundary in the ENVIRONMENT, never
//! in argv.
//!
//! `ps aux` and `/proc/<pid>/cmdline` are readable by other local users on a
//! stock box; `/proc/<pid>/environ` is owner-only. The unit tests in
//! `cloud_api::transcribe` pin the precedence, and the Python test pins what
//! the worker builds -- but neither actually launches the helper, so nothing
//! proved the variable survives the spawn. On Windows that is a different
//! mechanism entirely (`CreateProcess` with an explicit environment block),
//! which is the platform this project ships on. CI runs this file on both.
//!
//! These assert on WHICH failure comes back rather than on success: reaching
//! a real provider from CI is neither possible nor desirable. "Did not fail
//! for want of a key" is the whole claim.

use std::process::Command;

const WD: &str = env!("CARGO_BIN_EXE_wd");

const EMPTY_KEY_ERROR: &str = "cloud transcription API key is empty";

fn run(env: &[(&str, &str)], extra: &[&str]) -> String {
    // A REAL file is required. `handle_cloud_transcribe` reads the WAV before
    // it validates the key, so pointing at a missing path makes every run
    // fail with "No such file or directory" -- and the two positive tests
    // below would then pass with no key set at all. The vacuity guard
    // (`no_key_anywhere_still_reports_the_empty_key_error`) caught exactly
    // that while this file was being written.
    let dir = tempfile::tempdir().expect("temp dir");
    let wav = dir.path().join("audio.wav");
    std::fs::write(&wav, b"RIFF....WAVEfmt ").expect("write fixture");

    let mut cmd = Command::new(WD);
    cmd.args([
        "cloud-transcribe",
        "--base-url",
        "https://api.openai.com/v1",
        "--model",
        "whisper-1",
        "--audio-wav-path",
        &wav.to_string_lossy(),
        "--timeout-ms",
        "1000",
    ])
    .args(extra)
    .env_remove("VOICEPI_STT_API_KEY")
    .env_remove("OPENAI_API_KEY")
    .env_remove("GROQ_API_KEY");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("helper runs");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn key_from_the_environment_reaches_the_helper_without_argv() {
    let combined = run(&[("VOICEPI_STT_API_KEY", "env-provided-key")], &[]);
    assert!(
        !combined.contains(EMPTY_KEY_ERROR),
        "helper did not see the env key; got: {combined}"
    );
}

#[test]
fn provider_generic_key_also_crosses_for_a_recognised_host() {
    let combined = run(&[("OPENAI_API_KEY", "env-provided-key")], &[]);
    assert!(
        !combined.contains(EMPTY_KEY_ERROR),
        "helper did not see OPENAI_API_KEY for an api.openai.com base URL; got: {combined}"
    );
}

#[test]
fn no_key_anywhere_still_reports_the_empty_key_error() {
    // Guards the two tests above against passing vacuously: if the helper
    // stopped emitting this message, they would go green with no key at all.
    let combined = run(&[], &[]);
    assert!(
        combined.contains(EMPTY_KEY_ERROR),
        "expected the empty-key error with no key set; got: {combined}"
    );
}

#[test]
fn api_key_flag_still_works_for_backwards_compatibility() {
    let combined = run(&[], &["--api-key", "flag-provided-key"]);
    assert!(
        !combined.contains(EMPTY_KEY_ERROR),
        "the deprecated --api-key flag must keep working; got: {combined}"
    );
}
