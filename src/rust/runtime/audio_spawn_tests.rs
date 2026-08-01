//! Tests for native runtime audio-device selection.

use crate::runtime::audio_spawn::{
    resolve_audio_device_from_env, resolved_audio_device, AUDIO_DEVICE_ENV,
};
use crate::test_env_lock::ENV_LOCK;
use std::env;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = env::var(key).ok();
        env::set_var(key, value);
        Self { key, prev }
    }
    fn unset(key: &'static str) -> Self {
        let prev = env::var(key).ok();
        env::remove_var(key);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => env::set_var(self.key, v),
            None => env::remove_var(self.key),
        }
    }
}

#[test]
fn resolved_audio_device_honours_voicepi_audio_device_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::set(AUDIO_DEVICE_ENV, "Yeti X");
    // A user's saved mic choice applies to the native backend without a
    // second selector. Empty string (the unset case) means
    // "system default" — `audio::capture::start_capture` respects that.
    assert_eq!(resolved_audio_device(), "Yeti X");
}

#[test]
fn resolved_audio_device_defaults_to_empty_for_unset_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    assert!(
        resolved_audio_device().is_empty(),
        "unset VOICEPI_AUDIO_DEVICE must resolve to '' (= system default)",
    );
}

/// Verify the selected audio device is read from `WorkerCommand.env`,
/// rather than only from the parent process environment.
#[test]
fn resolve_audio_device_from_env_prefers_worker_command_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Even if the process env says something else (typical: shell
    // doesn't export it at all), the worker-command override wins.
    let _g = EnvGuard::set(AUDIO_DEVICE_ENV, "system-shell-mic");
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "Saved Settings Mic".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "Saved Settings Mic",
    );
}

#[test]
fn resolve_audio_device_from_env_falls_back_to_process_env() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No override in the command env → process env is the next-best
    // source so legacy shell-export workflows keep working.
    let _g = EnvGuard::set(AUDIO_DEVICE_ENV, "Process Env Mic");
    assert_eq!(
        resolve_audio_device_from_env(&[]),
        "Process Env Mic",
        "process env must serve as the legacy fallback",
    );
}

#[test]
fn resolve_audio_device_from_env_returns_empty_when_neither_set() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    assert!(
        resolve_audio_device_from_env(&[]).is_empty(),
        "neither source set → empty string = system default",
    );
}

/// Values like `"  Yeti  "` resolve to `"Yeti"` and a whitespace-only value
/// collapses to `""` (= system default). Without trimming, raw spaces are
/// forwarded to CPAL's device matching and
/// either fail to match or are treated as a literal selector.
#[test]
fn resolve_audio_device_from_env_trims_whitespace_from_overrides() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "  Yeti X  ".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "Yeti X",
        "leading/trailing whitespace must be trimmed before CPAL lookup",
    );
}

#[test]
fn resolve_audio_device_from_env_collapses_blank_override_to_empty() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    let overrides = vec![(AUDIO_DEVICE_ENV.to_owned(), "   ".to_owned())];
    assert_eq!(
        resolve_audio_device_from_env(&overrides),
        "",
        "a whitespace-only override must collapse to '' (= system default)",
    );
}

#[test]
fn resolve_audio_device_from_env_trims_process_env_fallback() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // No worker-command override → process env is consulted, and it
    // must be trimmed the same way the override path is.
    let _g = EnvGuard::set(AUDIO_DEVICE_ENV, "\tHeadset Mic\n");
    assert_eq!(resolve_audio_device_from_env(&[]), "Headset Mic");
}

#[test]
fn resolve_audio_device_from_env_ignores_unrelated_overrides() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _g = EnvGuard::unset(AUDIO_DEVICE_ENV);
    let overrides = vec![
        ("VOICEPI_MODEL".to_owned(), "small".to_owned()),
        ("UNRELATED_RUNTIME_KEY".to_owned(), "/somewhere".to_owned()),
    ];
    assert!(
        resolve_audio_device_from_env(&overrides).is_empty(),
        "unrelated env keys must not be mistaken for the device override",
    );
}
