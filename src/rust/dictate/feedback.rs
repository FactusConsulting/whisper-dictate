//! Audible cues for the Rust in-process dictation engine, ported from
//! `src/python/whisper_dictate/vp_feedback.py`.
//!
//! Parity target
//! =============
//!
//! The Python engine plays a short cue at PTT press (start) and PTT
//! release (stop) so headless / autostart installs still get audible
//! confirmation. When `VOICEPI_DICTATE_ENGINE=rust` flipped the default
//! engine to Rust these cues silently disappeared (parity blocker #3 on
//! the engine assessment). This module restores them so the Rust engine
//! behaves the same way the Python engine does:
//!
//! * env gate: `VOICEPI_FEEDBACK_SOUNDS` (same variable as Python, live
//!   read on every cue; config.json is already overlaid onto the env at
//!   startup and on every live reload, so the env var IS the setting);
//! * Windows: short beep — 880 Hz on start, 440 Hz on stop, 80 ms —
//!   matching `vp_feedback._play_windows` exactly (Python uses
//!   `winsound.Beep`; the Rust port calls `kernel32!Beep` directly via
//!   an `extern "system"` block so there is no new crate dependency);
//! * Linux: play the same freedesktop asset files Python does, via
//!   `paplay` / `pw-play` (first found on `$PATH` wins) — matches
//!   `vp_feedback._play_linux` including the asset paths and the player
//!   preference order;
//! * macOS / other platforms: no-op (matches Python's silent skip);
//! * non-blocking: every playback path spawns a short-lived thread so
//!   the PTT hot path returns immediately (mirrors Python's `Popen` +
//!   reaper-thread pattern and its `threading.Thread(daemon=True)`
//!   `winsound.Beep` wrapper);
//! * best-effort: any error is swallowed — a broken audio subsystem
//!   never fails an utterance, never bubbles, never logs to stderr
//!   (Python's `try / except: pass`).
//!
//! Wiring
//! ======
//!
//! [`DictateSession`](crate::dictate::DictateSession) holds a boxed
//! [`CueSink`] (default: [`NoOpCueSink`], so existing tests never emit
//! sounds). Production sessions attach [`SystemCueSink`] via
//! [`crate::dictate::DictateSession::with_cue_sink`]; the session then
//! calls `sink.play(CueKind::Start)` at the same moment Python calls
//! `play_cue("start")` (right after emitting `status=recording`) and
//! `sink.play(CueKind::Stop)` at the same moment Python calls
//! `play_cue("stop")` (right after capture stops, before the transcribe
//! pass runs).

#[cfg(windows)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(windows, target_os = "linux"))]
use std::thread;

/// Which lifecycle moment the cue is signalling. Mirrors the string
/// parameter Python's `play_cue` accepts (`"start"` / `"stop"`); a
/// closed enum on the Rust side keeps mis-spellings unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    /// PTT press: start-of-recording cue.
    Start,
    /// PTT release: end-of-recording cue.
    Stop,
}

/// Playback boundary the session drives on start / stop. Kept as a
/// trait so a test can swap in a capturing mock without touching the
/// audio subsystem. Production wiring installs [`SystemCueSink`], the
/// default in [`crate::dictate::DictateSession::new`] is
/// [`NoOpCueSink`] so the huge existing test surface neither emits
/// sounds nor takes on a new env-var dependency.
pub trait CueSink {
    /// Emit the cue for `kind`. Must be non-blocking and infallible: a
    /// broken audio device or missing sound file must never propagate.
    fn play(&self, kind: CueKind);

    /// Apply the session-owned live settings used by the next cue.
    fn apply_settings(&self, _settings: &std::collections::BTreeMap<String, String>) {}
}

/// Silent implementation. Used as the [`crate::dictate::DictateSession`]
/// default so the state-machine tests never touch the audio subsystem,
/// and by callers that explicitly want cues off.
pub struct NoOpCueSink;

impl CueSink for NoOpCueSink {
    fn play(&self, _kind: CueKind) {}
}

/// Production sink: reads `VOICEPI_FEEDBACK_SOUNDS` live on every call
/// and dispatches to the platform-specific playback path. See the
/// module docs for the per-platform behaviour and the Python
/// reference.
pub struct SystemCueSink;

impl CueSink for SystemCueSink {
    fn play(&self, kind: CueKind) {
        play_cue(kind);
    }
}

/// Production cue sink for the native runtime. The live enable gate is owned
/// by the session rather than read from the process environment.
#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
pub(crate) struct SessionCueSink {
    enabled: std::sync::atomic::AtomicBool,
}

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
impl SessionCueSink {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled: std::sync::atomic::AtomicBool::new(enabled),
        }
    }
}

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
impl CueSink for SessionCueSink {
    fn play(&self, kind: CueKind) {
        if self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            play_enabled_cue(kind);
        }
    }

    fn apply_settings(&self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(value) = settings.get("feedback_sounds") {
            self.enabled
                .store(is_truthy_value(value), std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// Free-function entrypoint that mirrors Python's `vp_feedback.play_cue`
/// one-for-one. Exposed for the CLI-side call sites that don't hold a
/// session; the trait wrapper above delegates here.
pub fn play_cue(kind: CueKind) {
    if !sounds_enabled() {
        return;
    }
    play_enabled_cue(kind);
}

fn play_enabled_cue(kind: CueKind) {
    #[cfg(windows)]
    {
        play_windows(kind);
    }
    #[cfg(target_os = "linux")]
    {
        play_linux(kind);
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        // macOS / other: no-op, matching Python.
        let _ = kind;
    }
}

// ── env gate ────────────────────────────────────────────────────────────────

/// Live env-var read matching Python's `_env_truthy`. Any value other
/// than empty / `0` / `false` / `no` / `off` (case-insensitive) enables
/// cues. Kept `pub(crate)` so unit tests can exercise the exact same
/// truthiness table Python honours.
pub(crate) fn sounds_enabled() -> bool {
    env_truthy("VOICEPI_FEEDBACK_SOUNDS")
}

/// Mirrors `vp_feedback._env_truthy` — trims whitespace, ASCII-lowers
/// the value, and compares against the falsy token list. Broken out so
/// tests can pin the truthiness table without going through
/// `std::env::set_var` (which needs the crate-wide `ENV_LOCK`).
pub(crate) fn env_truthy(name: &str) -> bool {
    let value = std::env::var(name).unwrap_or_default();
    is_truthy_value(&value)
}

/// Pure predicate ported from `vp_feedback._env_truthy`. Public in the
/// crate so tests can drive it directly without env mutation.
pub(crate) fn is_truthy_value(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
}

// ── platform: windows ───────────────────────────────────────────────────────

/// Cap on concurrently in-flight beep threads. A jammed audio
/// subsystem could otherwise pile up 80 ms beep threads on every PTT
/// press; the cap keeps the process from leaking threads under
/// failure. Chosen small (matches Python's implicit "one at a time"
/// behaviour via the daemon thread turnover — the practical bound is
/// how many PTT presses fit in an 80 ms window, which is ~1).
#[cfg(windows)]
const MAX_INFLIGHT_BEEPS: usize = 8;

#[cfg(windows)]
static INFLIGHT_BEEPS: AtomicUsize = AtomicUsize::new(0);

#[cfg(windows)]
extern "system" {
    /// kernel32!Beep. Same Win32 primitive Python's `winsound.Beep`
    /// calls; documented at
    /// <https://learn.microsoft.com/en-us/windows/win32/api/utilapiset/nf-utilapiset-beep>.
    /// Synchronous — blocks for `duration` milliseconds — so the
    /// caller runs it on a short-lived detached thread.
    fn Beep(dwFreq: u32, dwDuration: u32) -> i32;
}

/// Windows beep on a daemon thread. 880 Hz start / 440 Hz stop /
/// 80 ms — same values `vp_feedback._play_windows` uses. Non-blocking:
/// the PTT hot path returns immediately.
#[cfg(windows)]
fn play_windows(kind: CueKind) {
    let frequency: u32 = match kind {
        CueKind::Start => 880,
        CueKind::Stop => 440,
    };
    // Runaway-thread guard — see MAX_INFLIGHT_BEEPS. Roll back the
    // increment when we refuse so the counter doesn't drift.
    if INFLIGHT_BEEPS.fetch_add(1, Ordering::Relaxed) >= MAX_INFLIGHT_BEEPS {
        INFLIGHT_BEEPS.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let spawn_result = thread::Builder::new()
        .name("wd-cue-beep".to_owned())
        .spawn(move || {
            // SAFETY: kernel32!Beep is thread-safe and takes two
            // primitive DWORDs. Return value is ignored — Python
            // wraps this in `try / except: pass`.
            unsafe {
                let _ = Beep(frequency, 80);
            }
            INFLIGHT_BEEPS.fetch_sub(1, Ordering::Relaxed);
        });
    if spawn_result.is_err() {
        // Rare: thread-spawn failure. Roll the counter back so a
        // future press isn't permanently gated by this failure.
        INFLIGHT_BEEPS.fetch_sub(1, Ordering::Relaxed);
    }
}

// ── platform: linux ─────────────────────────────────────────────────────────

/// Freedesktop start-cue path. Matches
/// `vp_feedback._FREEDESKTOP_START`.
#[cfg(target_os = "linux")]
pub(crate) const FREEDESKTOP_START: &str = "/usr/share/sounds/freedesktop/stereo/message.oga";

/// Freedesktop stop-cue path. Matches `vp_feedback._FREEDESKTOP_STOP`.
#[cfg(target_os = "linux")]
pub(crate) const FREEDESKTOP_STOP: &str =
    "/usr/share/sounds/freedesktop/stereo/dialog-information.oga";

/// Player binaries tried in order. Matches `vp_feedback._LINUX_PLAYERS`
/// exactly — `paplay` first so PipeWire/PulseAudio boxes with both
/// installed pick the PulseAudio-native player, `pw-play` as the
/// PipeWire-only fallback.
#[cfg(target_os = "linux")]
pub(crate) const LINUX_PLAYERS: &[&str] = &["paplay", "pw-play"];

/// Linux playback: spawn `paplay` / `pw-play` on the same freedesktop
/// file Python uses, fire-and-forget with a reaper thread so no zombie
/// accumulates. Missing sound file → silent no-op (matches Python's
/// `os.path.exists` short-circuit).
#[cfg(target_os = "linux")]
fn play_linux(kind: CueKind) {
    let sound_file = match kind {
        CueKind::Start => FREEDESKTOP_START,
        CueKind::Stop => FREEDESKTOP_STOP,
    };
    if !std::path::Path::new(sound_file).exists() {
        return;
    }
    for player in LINUX_PLAYERS {
        let mut command = std::process::Command::new(player);
        command
            .arg(sound_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
        match command.spawn() {
            Ok(child) => {
                reap(child);
                return;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return, // some other spawn failure — swallow
        }
    }
}

/// Detached waiter so a fire-and-forget subprocess doesn't leak a
/// zombie on POSIX. Mirrors `vp_feedback._reap`.
#[cfg(target_os = "linux")]
fn reap(mut child: std::process::Child) {
    let _ = thread::Builder::new()
        .name("wd-cue-reap".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Local mutex to serialise the tests in this file that mutate the
    /// process env. We can't take the crate-wide `ENV_LOCK` here
    /// because these tests only touch this module's variable and the
    /// wider test suite is already using it for the session tests.
    static LOCAL_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn is_truthy_value_matches_python_table() {
        // Python's `_env_truthy` treats these as OFF.
        for off in ["", "0", "false", "no", "off", " OFF ", "False"] {
            assert!(
                !is_truthy_value(off),
                "expected {off:?} to be falsy (Python parity)"
            );
        }
        // And these as ON (anything else).
        for on in ["1", "true", "yes", "on", "enabled", " YES "] {
            assert!(
                is_truthy_value(on),
                "expected {on:?} to be truthy (Python parity)"
            );
        }
    }

    #[test]
    fn sounds_enabled_reflects_env_var() {
        let _guard = LOCAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _outer = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("VOICEPI_FEEDBACK_SOUNDS").ok();
        std::env::remove_var("VOICEPI_FEEDBACK_SOUNDS");
        assert!(!sounds_enabled(), "unset must be off");
        std::env::set_var("VOICEPI_FEEDBACK_SOUNDS", "1");
        assert!(sounds_enabled(), "\"1\" must be on");
        std::env::set_var("VOICEPI_FEEDBACK_SOUNDS", "0");
        assert!(!sounds_enabled(), "\"0\" must be off");
        // Restore.
        match prior {
            Some(v) => std::env::set_var("VOICEPI_FEEDBACK_SOUNDS", v),
            None => std::env::remove_var("VOICEPI_FEEDBACK_SOUNDS"),
        }
    }

    #[test]
    fn play_cue_when_disabled_is_a_noop() {
        // With the env var unset, play_cue must return without touching
        // the audio subsystem. We can't observe the audio subsystem
        // from here, but we can at least verify the function does not
        // panic and returns synchronously (the platform paths would
        // spawn a thread; the disabled path must not).
        let _guard = LOCAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _outer = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("VOICEPI_FEEDBACK_SOUNDS").ok();
        std::env::remove_var("VOICEPI_FEEDBACK_SOUNDS");
        play_cue(CueKind::Start);
        play_cue(CueKind::Stop);
        match prior {
            Some(v) => std::env::set_var("VOICEPI_FEEDBACK_SOUNDS", v),
            None => std::env::remove_var("VOICEPI_FEEDBACK_SOUNDS"),
        }
    }

    #[test]
    fn system_cue_sink_never_panics() {
        // Regardless of env / audio device state, SystemCueSink::play
        // must swallow all errors. Belt-and-braces: the platform impls
        // already swallow via `_ = ...`, but the trait-level contract
        // is tested here so a future refactor can't quietly break it.
        let sink = SystemCueSink;
        sink.play(CueKind::Start);
        sink.play(CueKind::Stop);
    }

    #[test]
    fn noop_sink_is_silent() {
        // Nothing to assert other than "does not panic and produces no
        // observable side-effects" -- the type carries the invariant.
        NoOpCueSink.play(CueKind::Start);
        NoOpCueSink.play(CueKind::Stop);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_asset_paths_match_python_reference() {
        // Python parity: the two path constants must be exactly those
        // in `vp_feedback._FREEDESKTOP_START` / `_FREEDESKTOP_STOP`.
        assert_eq!(
            FREEDESKTOP_START,
            "/usr/share/sounds/freedesktop/stereo/message.oga"
        );
        assert_eq!(
            FREEDESKTOP_STOP,
            "/usr/share/sounds/freedesktop/stereo/dialog-information.oga"
        );
        assert_eq!(LINUX_PLAYERS, &["paplay", "pw-play"]);
    }
}
