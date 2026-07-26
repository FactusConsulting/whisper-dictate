//! Windows audio ducking -- lower other apps' volume while recording.
//!
//! Rust port of `src/python/whisper_dictate/vp_audio_ducking.py` (parity
//! blocker #2 on the engine assessment). Restores audible parity when the
//! default engine flips to Rust so background media is dampened while the
//! user is dictating, exactly the way the Python engine has always done
//! it.
//!
//! # Parity target
//!
//! The Python reference is Windows-only: it uses `pycaw` (a Python
//! wrapper around WASAPI via COM) to enumerate every audio session on
//! the default render endpoint, skip the current process, remember each
//! session's current volume, and lower it to the configured target
//! ratio. On `enter()` -> `exit()` the previous volumes are restored in
//! reverse order. On Linux / macOS the Python impl warns once and
//! no-ops (`"audio ducking is only implemented on Windows"`); the Rust
//! port matches that behaviour exactly -- the PR description lists this
//! as a documented follow-up (Linux via `pactl` / `pw-cli` would be a
//! new backend, not a parity port).
//!
//! # Env-var gate + level
//!
//! Same keys the Python port reads (via `vp_config.get_value`, which is
//! backed by the same env vars + config.json overlay the rest of the
//! Rust runtime uses):
//!
//! * `VOICEPI_AUDIO_DUCKING` -- truthy enables the ducker (default: off).
//! * `VOICEPI_AUDIO_DUCKING_LEVEL` -- target volume ratio in `[0.0, 1.0]`,
//!   default `0.25`. Non-numeric / out-of-range values clamp to the
//!   valid interval; anything unparseable falls back to the default,
//!   matching `_float_setting` in the Python port.
//!
//! # Backend choice
//!
//! Windows: uses the `windows` crate (feature `audio-capture` -- the
//! same feature that pulls the crate in for the DirectSound capture
//! enumerator; a build without it falls through to the warn-once no-op
//! path so the type still compiles and links). This is a natural
//! upgrade over the Python COM path (`pycaw` shims over `comtypes`) --
//! same OS API, safer bindings. The WASAPI backend lives in the
//! [`wasapi`] submodule so this file stays focused on the
//! platform-agnostic trait + config surface.
//!
//! Non-Windows: warn-once no-op, matching Python.
//!
//! # Safety net
//!
//! [`SystemAudioDucker`] holds a `Drop` impl that calls `exit()` so a
//! panic / abnormal drop mid-utterance still restores the volumes it
//! lowered. The `enter()` path is idempotent (a second `enter()`
//! without an intervening `exit()` no-ops so a retry cannot leak
//! state) and any per-session WASAPI failure is swallowed by
//! `warn_once` so a broken audio device never aborts a dictation.

use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(windows, feature = "audio-capture"))]
mod wasapi;

/// Same env var Python's `vp_audio_ducking._truthy(get_value(...))`
/// reads. Truthy per the Rust port's env-truthy table (empty / 0 /
/// false / no / off are OFF; anything else is ON).
pub const AUDIO_DUCKING_ENV: &str = "VOICEPI_AUDIO_DUCKING";

/// Level env var (target volume ratio). Same key Python's
/// `_float_setting("VOICEPI_AUDIO_DUCKING_LEVEL", 0.25, 0.0, 1.0)`
/// reads.
pub const AUDIO_DUCKING_LEVEL_ENV: &str = "VOICEPI_AUDIO_DUCKING_LEVEL";

/// Default target volume ratio, matching Python's default (`0.25`,
/// i.e. dampen other apps to 25%).
pub const DEFAULT_TARGET_VOLUME: f32 = 0.25;

/// Per-utterance lifecycle boundary the session drives on PTT press /
/// release.
///
/// Both methods are infallible by contract: an audio-ducking failure
/// must NEVER abort a dictation, so implementations are expected to
/// convert any provider / OS error into a `warn_once` and return. See
/// [`SystemAudioDucker`] for the production impl and [`NoOpAudioDucker`]
/// for the silent default the session uses when the caller does not
/// opt in.
pub trait AudioDucker: Send {
    /// Lower the volume of other audio sessions (called at PTT press,
    /// after the `status=recording` flip so the timing lines up with
    /// Python's `vp_dictate.py::_start`).
    fn enter(&mut self);
    /// Restore whatever `enter` lowered (called at PTT release from
    /// `stop_and_transcribe` / `cancel`). Must be idempotent so a
    /// double-release, a chord-cancel that also calls exit, or the
    /// Drop safety-net cannot double-restore.
    fn exit(&mut self);
}

/// Silent implementation. Used as the default in
/// [`crate::dictate::DictateSession::new`] so the huge existing test
/// surface neither reads env vars nor talks to any OS audio API, and
/// by callers that explicitly want ducking off for this session.
pub struct NoOpAudioDucker;

impl AudioDucker for NoOpAudioDucker {
    fn enter(&mut self) {}
    fn exit(&mut self) {}
}

/// Production audio ducker: reads the env-var gate + level at
/// construction and dispatches to the platform backend on each
/// `enter` / `exit`. See the module docs for the per-platform
/// behaviour.
pub struct SystemAudioDucker {
    enabled: bool,
    target_volume: f32,
    warned: AtomicBool,
    #[cfg(all(windows, feature = "audio-capture"))]
    active: Vec<wasapi::LoweredSession>,
}

impl SystemAudioDucker {
    /// Read the env-var gate + level and build a ducker. The gate is
    /// stamped at construction, matching Python's
    /// `AudioDucker.from_config()` -- `enabled` and `target_volume`
    /// are read once and cached for the ducker's lifetime, so a live
    /// setting change requires a fresh session (matches the current
    /// supervisor lifecycle).
    pub fn from_env() -> Self {
        Self {
            enabled: env_truthy(AUDIO_DUCKING_ENV),
            target_volume: parse_target_volume_from_env(),
            warned: AtomicBool::new(false),
            #[cfg(all(windows, feature = "audio-capture"))]
            active: Vec::new(),
        }
    }

    /// Build a ducker with explicit values (used by tests + callers
    /// that supply the gate from a source other than the process env).
    /// `enabled=false` short-circuits every subsequent call.
    pub fn new(enabled: bool, target_volume: f32) -> Self {
        Self {
            enabled,
            target_volume: clamp_target_volume(target_volume),
            warned: AtomicBool::new(false),
            #[cfg(all(windows, feature = "audio-capture"))]
            active: Vec::new(),
        }
    }

    /// True when the ducker is configured to actually lower volumes.
    /// Exposed for tests + diagnostics.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Configured target volume in `[0.0, 1.0]`. Exposed for tests.
    pub fn target_volume(&self) -> f32 {
        self.target_volume
    }

    /// Log a one-shot warning to stderr. Mirrors Python's `_warn_once`
    /// so a broken audio subsystem produces a single actionable line
    /// per session instead of spamming every PTT press.
    fn warn_once(&self, message: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!("[audio-duck] {message}");
        }
    }
}

impl AudioDucker for SystemAudioDucker {
    fn enter(&mut self) {
        if !self.enabled {
            return;
        }
        #[cfg(all(windows, feature = "audio-capture"))]
        {
            // Idempotent: a second enter() without an intervening
            // exit() (e.g. a retry after a partial start failure)
            // must not leak a second set of lowered sessions.
            if !self.active.is_empty() {
                return;
            }
            match wasapi::duck(self.target_volume) {
                Ok(lowered) => {
                    let count = lowered.len();
                    self.active = lowered;
                    if count > 0 {
                        println!(
                            "[audio-duck] lowered {count} audio sessions to {:.2}",
                            self.target_volume,
                        );
                    }
                }
                Err(err) => {
                    self.warn_once(&format!("audio ducking unavailable: {err}"));
                }
            }
        }
        #[cfg(not(all(windows, feature = "audio-capture")))]
        {
            let _ = self.target_volume;
            self.warn_once("audio ducking is only implemented on Windows");
        }
    }

    fn exit(&mut self) {
        #[cfg(all(windows, feature = "audio-capture"))]
        {
            if self.active.is_empty() {
                return;
            }
            let lowered = std::mem::take(&mut self.active);
            let restored = wasapi::restore(lowered);
            println!("[audio-duck] restored {restored} audio sessions");
        }
    }
}

impl Drop for SystemAudioDucker {
    /// RAII safety-net: if the ducker is dropped while the session was
    /// mid-utterance (a panic, a cancelled session, an in-flight
    /// supervisor teardown), still restore the volumes we lowered so
    /// the user's media doesn't stay quiet forever.
    fn drop(&mut self) {
        self.exit();
    }
}

// -- env parsing -----------------------------------------------------------

/// Mirrors `vp_feedback._env_truthy` / the Python audio-ducking
/// truthiness table: empty / `0` / `false` / `no` / `off`
/// (case-insensitive, whitespace-trimmed) are OFF; everything else is
/// ON. Broken out so tests can pin the table without process-env
/// mutation.
pub(crate) fn env_truthy(name: &str) -> bool {
    let value = std::env::var(name).unwrap_or_default();
    is_truthy_value(&value)
}

/// Pure predicate used by [`env_truthy`]; exposed for unit tests.
pub(crate) fn is_truthy_value(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
}

/// Read + parse [`AUDIO_DUCKING_LEVEL_ENV`], falling back to
/// [`DEFAULT_TARGET_VOLUME`] on missing / unparseable values and
/// clamping to `[0.0, 1.0]` afterwards -- byte-for-byte matching
/// Python's `_float_setting`.
pub(crate) fn parse_target_volume_from_env() -> f32 {
    let raw = std::env::var(AUDIO_DUCKING_LEVEL_ENV).unwrap_or_default();
    let parsed = raw.trim().parse::<f32>().unwrap_or(DEFAULT_TARGET_VOLUME);
    clamp_target_volume(parsed)
}

/// Clamp `value` to `[0.0, 1.0]` (Python's `min(max(...))` bounds).
/// A NaN input clamps to the default so the ducker never asks WASAPI
/// to set a bogus level.
pub(crate) fn clamp_target_volume(value: f32) -> f32 {
    if value.is_nan() {
        return DEFAULT_TARGET_VOLUME;
    }
    value.clamp(0.0, 1.0)
}

// -- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Local mutex to serialise env-mutation tests in THIS file without
    /// clashing with the crate-wide `ENV_LOCK` (session tests own that
    /// one). We only mutate the two audio-ducking env vars here.
    static LOCAL_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn env_truthy_matches_python_table() {
        // Python's `_truthy` and the shared env-truthy in feedback.rs
        // agree on this table. Ducking must not diverge.
        for off in ["", "0", "false", "no", "off", " OFF ", "False"] {
            assert!(!is_truthy_value(off), "{off:?} must be falsy");
        }
        for on in ["1", "true", "yes", "on", "0.25", " YES "] {
            assert!(is_truthy_value(on), "{on:?} must be truthy");
        }
    }

    #[test]
    fn clamp_target_volume_clamps_out_of_range_values() {
        // Python's `_float_setting("VOICEPI_AUDIO_DUCKING_LEVEL", 0.25,
        // 0.0, 1.0)` clamps to `[0.0, 1.0]`. NaN maps to the default.
        assert_eq!(clamp_target_volume(0.5), 0.5);
        assert_eq!(clamp_target_volume(-0.5), 0.0);
        assert_eq!(clamp_target_volume(2.5), 1.0);
        assert_eq!(clamp_target_volume(0.0), 0.0);
        assert_eq!(clamp_target_volume(1.0), 1.0);
        assert_eq!(clamp_target_volume(f32::NAN), DEFAULT_TARGET_VOLUME);
    }

    #[test]
    fn parse_target_volume_from_env_uses_default_and_clamps() {
        let _local = LOCAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _outer = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var(AUDIO_DUCKING_LEVEL_ENV).ok();
        std::env::remove_var(AUDIO_DUCKING_LEVEL_ENV);
        assert_eq!(parse_target_volume_from_env(), DEFAULT_TARGET_VOLUME);
        std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, "0.5");
        assert_eq!(parse_target_volume_from_env(), 0.5);
        // Out of range -> clamp.
        std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, "2.5");
        assert_eq!(parse_target_volume_from_env(), 1.0);
        // Unparseable -> default.
        std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, "nope");
        assert_eq!(parse_target_volume_from_env(), DEFAULT_TARGET_VOLUME);
        match prior {
            Some(v) => std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, v),
            None => std::env::remove_var(AUDIO_DUCKING_LEVEL_ENV),
        }
    }

    #[test]
    fn from_env_reads_gate_and_level() {
        let _local = LOCAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _outer = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior_gate = std::env::var(AUDIO_DUCKING_ENV).ok();
        let prior_level = std::env::var(AUDIO_DUCKING_LEVEL_ENV).ok();

        // Off + default level.
        std::env::remove_var(AUDIO_DUCKING_ENV);
        std::env::remove_var(AUDIO_DUCKING_LEVEL_ENV);
        let d = SystemAudioDucker::from_env();
        assert!(!d.enabled(), "unset must be off");
        assert_eq!(d.target_volume(), DEFAULT_TARGET_VOLUME);

        // On + custom level.
        std::env::set_var(AUDIO_DUCKING_ENV, "1");
        std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, "0.4");
        let d = SystemAudioDucker::from_env();
        assert!(d.enabled(), "\"1\" must be on");
        assert_eq!(d.target_volume(), 0.4);

        match prior_gate {
            Some(v) => std::env::set_var(AUDIO_DUCKING_ENV, v),
            None => std::env::remove_var(AUDIO_DUCKING_ENV),
        }
        match prior_level {
            Some(v) => std::env::set_var(AUDIO_DUCKING_LEVEL_ENV, v),
            None => std::env::remove_var(AUDIO_DUCKING_LEVEL_ENV),
        }
    }

    #[test]
    fn noop_ducker_enter_exit_are_infallible() {
        // Trivial but keeps the contract explicit: the default
        // ducker must never touch anything, so a session that never
        // opts in stays byte-identical to a pre-ducker build.
        let mut d = NoOpAudioDucker;
        d.enter();
        d.exit();
        d.enter();
        d.exit();
    }

    #[test]
    fn disabled_system_ducker_never_warns() {
        // Gate off -> enter() is a pure early-return; no OS call, no
        // warn (the warn is reserved for actual failures / unsupported
        // platforms while enabled).
        let mut d = SystemAudioDucker::new(false, 0.25);
        d.enter();
        d.exit();
        // No observable side effect; the assertion is that neither
        // call panicked.
    }
}
