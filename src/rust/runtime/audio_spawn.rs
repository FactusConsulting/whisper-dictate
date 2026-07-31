//! Native audio-device selection shared by runtime capture callers.

/// Saved microphone selector. Empty means the operating-system default.
pub const AUDIO_DEVICE_ENV: &str = "VOICEPI_AUDIO_DEVICE";

/// Resolve the microphone from effective runtime overrides first, then the
/// process environment. Values are trimmed to match config handling.
pub fn resolve_audio_device_from_env(env_overrides: &[(String, String)]) -> String {
    for (key, value) in env_overrides {
        if key == AUDIO_DEVICE_ENV {
            return value.trim().to_owned();
        }
    }
    std::env::var(AUDIO_DEVICE_ENV)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default()
}

/// Resolve the microphone from the process environment.
pub fn resolved_audio_device() -> String {
    resolve_audio_device_from_env(&[])
}
