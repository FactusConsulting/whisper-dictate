//! Audible cues for the Rust in-process dictation engine.
//!
//! A short cue plays at PTT press (start) and PTT release (stop) so
//! headless / autostart installs still get audible confirmation. The
//! cues are configurable per event and gated by `VOICEPI_FEEDBACK_SOUNDS`.
//!
//! * env gate: `VOICEPI_FEEDBACK_SOUNDS` is live-read on every cue;
//!   config.json is already overlaid onto the env at startup and on
//!   every live reload, so the env var IS the setting;
//! * per-event start/stop switches default on; processing-complete
//!   defaults off, preserving the previous two-cue behavior for
//!   existing users;
//! * Windows: short beep — 880 Hz on start, 440 Hz on stop, 80 ms —
//!   via `kernel32!Beep` directly through an `extern "system"` block
//!   so there is no new crate dependency;
//! * Linux: play the freedesktop asset files via `paplay` / `pw-play`
//!   (first found on `$PATH` wins), with a fixed asset-path and
//!   player-preference order;
//! * macOS / other platforms: no-op;
//! * non-blocking: every playback path spawns a short-lived thread so
//!   the PTT hot path returns immediately;
//! * best-effort: any error is swallowed — a broken audio subsystem
//!   never fails an utterance, never bubbles, never logs to stderr.
//!
//! Wiring
//! ======
//!
//! [`DictateSession`](crate::dictate::DictateSession) holds a boxed
//! [`CueSink`] (default: [`NoOpCueSink`], so existing tests never emit
//! sounds). Production sessions attach [`SystemCueSink`] via
//! [`crate::dictate::DictateSession::with_cue_sink`]; the session then
//! calls `sink.play(CueKind::Start)` right after emitting
//! `status=recording` and `sink.play(CueKind::Stop)` right after
//! capture stops, before the transcribe pass runs. `Done` is emitted
//! after that accepted attempt finishes, independently of a success or
//! failure result.

#[cfg(any(windows, target_os = "linux"))]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(windows, target_os = "linux"))]
use std::thread;

#[path = "feedback_custom_wav.rs"]
mod custom_wav;

/// Which lifecycle moment the cue is signalling: start, stop, plus the
/// optional native processing-complete event; a
/// closed enum on the Rust side keeps mis-spellings unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    /// PTT press: start-of-recording cue.
    Start,
    /// PTT release: end-of-recording cue.
    Stop,
    /// The accepted recording attempt finished processing (success or error).
    Done,
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
/// module docs for the per-platform behaviour.
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
    start: std::sync::atomic::AtomicBool,
    stop: std::sync::atomic::AtomicBool,
    done: std::sync::atomic::AtomicBool,
}

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
impl SessionCueSink {
    pub(crate) fn new(enabled: bool, start: bool, stop: bool, done: bool) -> Self {
        Self {
            enabled: std::sync::atomic::AtomicBool::new(enabled),
            start: std::sync::atomic::AtomicBool::new(start),
            stop: std::sync::atomic::AtomicBool::new(stop),
            done: std::sync::atomic::AtomicBool::new(done),
        }
    }
}

#[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
impl CueSink for SessionCueSink {
    fn play(&self, kind: CueKind) {
        let enabled = self.enabled.load(std::sync::atomic::Ordering::Relaxed);
        if cue_selected(
            kind,
            enabled,
            self.start.load(std::sync::atomic::Ordering::Relaxed),
            self.stop.load(std::sync::atomic::Ordering::Relaxed),
            self.done.load(std::sync::atomic::Ordering::Relaxed),
        ) {
            play_enabled_cue(kind);
        }
    }

    fn apply_settings(&self, settings: &std::collections::BTreeMap<String, String>) {
        if let Some(value) = settings.get("feedback_sounds") {
            self.enabled
                .store(is_truthy_value(value), std::sync::atomic::Ordering::Relaxed);
        }
        for (key, flag) in [
            ("feedback_start", &self.start),
            ("feedback_stop", &self.stop),
            ("feedback_done", &self.done),
        ] {
            if let Some(value) = settings.get(key) {
                flag.store(is_truthy_value(value), std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

/// Free-function entrypoint. Exposed for the CLI-side call sites that
/// don't hold a
/// session; the trait wrapper above delegates here.
pub fn play_cue(kind: CueKind) {
    if !cue_enabled_by_env(kind) {
        return;
    }
    play_enabled_cue(kind);
}

/// The same effective per-event gate used by CLI diagnostics and playback.
pub(crate) fn cue_enabled_by_env(kind: CueKind) -> bool {
    cue_selected(
        kind,
        sounds_enabled(),
        env_truthy_or("VOICEPI_FEEDBACK_START", true),
        env_truthy_or("VOICEPI_FEEDBACK_STOP", true),
        env_truthy_or("VOICEPI_FEEDBACK_DONE", false),
    )
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
        // macOS / other: no-op.
        let _ = kind;
    }
}

// ── env gate ────────────────────────────────────────────────────────────────

/// Live env-var read. Any value other than empty / `0` / `false` /
/// `no` / `off` (case-insensitive) enables cues. Kept `pub(crate)` so
/// unit tests can exercise the exact truthiness table.
pub(crate) fn sounds_enabled() -> bool {
    env_truthy("VOICEPI_FEEDBACK_SOUNDS")
}

/// Trims whitespace, ASCII-lowers
/// the value, and compares against the falsy token list. Broken out so
/// tests can pin the truthiness table without going through
/// `std::env::set_var` (which needs the crate-wide `ENV_LOCK`).
pub(crate) fn env_truthy(name: &str) -> bool {
    let value = std::env::var(name).unwrap_or_default();
    is_truthy_value(&value)
}

fn cue_selected(kind: CueKind, enabled: bool, start: bool, stop: bool, done: bool) -> bool {
    enabled
        && match kind {
            CueKind::Start => start,
            CueKind::Stop => stop,
            CueKind::Done => done,
        }
}

fn env_truthy_or(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| is_truthy_value(&value))
        .unwrap_or(default)
}

/// Pure truthiness predicate. Public in the crate so tests can drive
/// it directly without env mutation.
pub(crate) fn is_truthy_value(value: &str) -> bool {
    let trimmed = value.trim().to_ascii_lowercase();
    !matches!(trimmed.as_str(), "" | "0" | "false" | "no" | "off")
}

/// Cap detached cue workers while a sound device is slow or unavailable.
/// Custom WAVs can last up to three seconds; a held/repeated hotkey must not
/// leave an unbounded queue of playback threads.
#[cfg(any(windows, target_os = "linux"))]
const MAX_INFLIGHT_CUES: usize = 8;

#[cfg(any(windows, target_os = "linux"))]
static INFLIGHT_CUES: AtomicUsize = AtomicUsize::new(0);

// ── platform: windows ───────────────────────────────────────────────────────

#[cfg(windows)]
pub(crate) fn custom_cue_available(kind: CueKind) -> bool {
    custom_wav::cue_file(kind).is_some()
}

#[cfg(windows)]
extern "system" {
    /// kernel32!Beep; documented at
    /// <https://learn.microsoft.com/en-us/windows/win32/api/utilapiset/nf-utilapiset-beep>.
    /// Synchronous — blocks for `duration` milliseconds — so the
    /// caller runs it on a short-lived detached thread.
    fn Beep(dwFreq: u32, dwDuration: u32) -> i32;
}

/// Windows custom WAV (when present), otherwise the legacy beep. Playback
/// runs on a detached thread so the PTT hot path returns immediately.
#[cfg(windows)]
fn play_windows(kind: CueKind) {
    let frequency: u32 = match kind {
        CueKind::Start => 880,
        CueKind::Stop => 440,
        CueKind::Done => 660,
    };
    // Runaway-thread guard — see MAX_INFLIGHT_CUES. Roll back the
    // increment when we refuse so the counter doesn't drift.
    if INFLIGHT_CUES.fetch_add(1, Ordering::Relaxed) >= MAX_INFLIGHT_CUES {
        INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let spawn_result = thread::Builder::new()
        .name("wd-cue-play".to_owned())
        .spawn(move || {
            let custom = custom_wav::cue_file(kind);
            play_windows_selected(custom.as_deref(), custom_wav::play_windows, || {
                // SAFETY: kernel32!Beep is thread-safe and takes two DWORDs.
                unsafe {
                    let _ = Beep(frequency, 80);
                }
            });
            INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
        });
    if spawn_result.is_err() {
        // Rare: thread-spawn failure. Roll the counter back so a
        // future press isn't permanently gated by this failure.
        INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn play_windows_selected(
    custom: Option<&std::path::Path>,
    play_wav: impl FnOnce(&std::path::Path) -> bool,
    beep: impl FnOnce(),
) {
    if custom.is_some_and(play_wav) {
        return;
    }
    beep();
}

// ── platform: linux ─────────────────────────────────────────────────────────

/// Freedesktop start-cue path.
#[cfg(target_os = "linux")]
pub(crate) const FREEDESKTOP_START: &str = "/usr/share/sounds/freedesktop/stereo/message.oga";

/// Freedesktop stop-cue path.
#[cfg(target_os = "linux")]
pub(crate) const FREEDESKTOP_STOP: &str =
    "/usr/share/sounds/freedesktop/stereo/dialog-information.oga";
#[cfg(target_os = "linux")]
pub(crate) const FREEDESKTOP_DONE: &str = "/usr/share/sounds/freedesktop/stereo/complete.oga";

/// Player binaries tried in order — `paplay` first so PipeWire/PulseAudio boxes with both
/// installed pick the PulseAudio-native player, `pw-play` as the
/// PipeWire-only fallback.
#[cfg(target_os = "linux")]
pub(crate) const LINUX_PLAYERS: &[&str] = &["paplay", "pw-play"];

/// Asset selected by a lifecycle cue on Linux.
#[cfg(target_os = "linux")]
pub(crate) fn freedesktop_cue_file(kind: CueKind) -> &'static str {
    match kind {
        CueKind::Start => FREEDESKTOP_START,
        CueKind::Stop => FREEDESKTOP_STOP,
        CueKind::Done => FREEDESKTOP_DONE,
    }
}

/// Linux playback: resolve the custom file and launch a player away from the
/// session thread. A stalled config filesystem cannot delay dictation.
#[cfg(target_os = "linux")]
fn play_linux(kind: CueKind) {
    spawn_linux_cue_worker(move || play_linux_inner(kind));
}

#[cfg(target_os = "linux")]
fn spawn_linux_cue_worker(work: impl FnOnce() + Send + 'static) {
    if INFLIGHT_CUES.fetch_add(1, Ordering::Relaxed) >= MAX_INFLIGHT_CUES {
        INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
        return;
    }
    let spawned = thread::Builder::new()
        .name("wd-cue-play".to_owned())
        .spawn(move || {
            work();
            INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
        });
    if spawned.is_err() {
        INFLIGHT_CUES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Launch `paplay` / `pw-play` on the selected file. The bounded cue worker
/// waits for the child, keeping its slot occupied until playback exits.
#[cfg(target_os = "linux")]
fn play_linux_inner(kind: CueKind) {
    let sound_file = linux_cue_file(kind);
    if !sound_file.exists() {
        return;
    }
    for player in LINUX_PLAYERS {
        let mut command = std::process::Command::new(player);
        command
            .arg(&sound_file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        crate::runtime::settings_snapshot::scrub_credentials_from_child(&mut command);
        match command.spawn() {
            Ok(mut child) => {
                let _ = child.wait();
                return;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return, // some other spawn failure — swallow
        }
    }
}

/// The same per-user override resolution used by playback and self-test.
#[cfg(target_os = "linux")]
pub(crate) fn linux_cue_file(kind: CueKind) -> std::path::PathBuf {
    custom_wav::cue_file(kind)
        .unwrap_or_else(|| std::path::PathBuf::from(freedesktop_cue_file(kind)))
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

    #[cfg(windows)]
    #[test]
    fn windows_custom_wav_wins_and_failed_wav_uses_legacy_beep() {
        use std::cell::Cell;
        let wav_calls = Cell::new(0);
        let beep_calls = Cell::new(0);
        let path = std::path::Path::new("start.wav");
        play_windows_selected(
            Some(path),
            |selected| {
                assert_eq!(selected, path);
                wav_calls.set(wav_calls.get() + 1);
                true
            },
            || beep_calls.set(beep_calls.get() + 1),
        );
        assert_eq!((wav_calls.get(), beep_calls.get()), (1, 0));
        play_windows_selected(
            Some(path),
            |_| false,
            || beep_calls.set(beep_calls.get() + 1),
        );
        play_windows_selected(
            None,
            |_| panic!("no WAV selected"),
            || beep_calls.set(beep_calls.get() + 1),
        );
        assert_eq!(beep_calls.get(), 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_cue_dispatch_does_not_wait_for_file_resolution() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (returned_tx, returned_rx) = std::sync::mpsc::channel();
        let caller = thread::spawn(move || {
            spawn_linux_cue_worker(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            returned_tx.send(()).unwrap();
        });
        let entered = entered_rx.recv_timeout(std::time::Duration::from_secs(2));
        let returned = returned_rx.recv_timeout(std::time::Duration::from_secs(2));
        let _ = release_tx.send(());
        caller.join().unwrap();
        assert!(entered.is_ok(), "cue worker did not start");
        assert!(returned.is_ok(), "cue dispatch blocked on worker I/O");
    }

    #[test]
    fn each_cue_can_be_disabled_without_muting_the_other_events() {
        assert!(!cue_selected(CueKind::Start, false, true, true, true));
        assert!(!cue_selected(CueKind::Start, true, false, true, true));
        assert!(cue_selected(CueKind::Stop, true, false, true, false));
        assert!(!cue_selected(CueKind::Stop, true, true, false, true));
        assert!(cue_selected(CueKind::Done, true, false, false, true));
        assert!(!cue_selected(CueKind::Done, true, true, true, false));
    }

    #[cfg(all(feature = "whisper-rs-local", feature = "rust-injection"))]
    #[test]
    fn session_cue_sink_applies_live_event_switches_independently() {
        use std::sync::atomic::Ordering;

        let sink = SessionCueSink::new(false, true, true, false);
        sink.apply_settings(&std::collections::BTreeMap::from([
            ("feedback_sounds".to_owned(), "1".to_owned()),
            ("feedback_start".to_owned(), "0".to_owned()),
            ("feedback_stop".to_owned(), "false".to_owned()),
            ("feedback_done".to_owned(), "yes".to_owned()),
        ]));
        assert!(sink.enabled.load(Ordering::Relaxed));
        assert!(!sink.start.load(Ordering::Relaxed));
        assert!(!sink.stop.load(Ordering::Relaxed));
        assert!(sink.done.load(Ordering::Relaxed));

        // A partial live update must preserve the other event switches.
        sink.apply_settings(&std::collections::BTreeMap::from([(
            "feedback_start".to_owned(),
            "1".to_owned(),
        )]));
        assert!(sink.start.load(Ordering::Relaxed));
        assert!(!sink.stop.load(Ordering::Relaxed));
        assert!(sink.done.load(Ordering::Relaxed));
    }

    #[test]
    fn event_env_switches_have_backward_compatible_defaults() {
        let _guard = LOCAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _outer = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let vars = [
            ("VOICEPI_FEEDBACK_START", true, "0"),
            ("VOICEPI_FEEDBACK_STOP", true, "false"),
            ("VOICEPI_FEEDBACK_DONE", false, "1"),
        ];
        let prior: Vec<_> = vars
            .iter()
            .map(|(name, _, _)| std::env::var(name).ok())
            .collect();
        for (name, default, override_value) in vars {
            std::env::remove_var(name);
            assert_eq!(env_truthy_or(name, default), default, "unset {name}");
            std::env::set_var(name, override_value);
            assert_eq!(env_truthy_or(name, default), !default, "set {name}");
        }
        for ((name, _, _), value) in vars.into_iter().zip(prior) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    #[test]
    fn is_truthy_value_table() {
        // These values are OFF.
        for off in ["", "0", "false", "no", "off", " OFF ", "False"] {
            assert!(!is_truthy_value(off), "expected {off:?} to be falsy");
        }
        // And these as ON (anything else).
        for on in ["1", "true", "yes", "on", "enabled", " YES "] {
            assert!(is_truthy_value(on), "expected {on:?} to be truthy");
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
    fn linux_asset_paths_are_freedesktop() {
        // Start/stop use the shared freedesktop assets; Done uses the
        // freedesktop completion asset.
        assert_eq!(
            FREEDESKTOP_START,
            "/usr/share/sounds/freedesktop/stereo/message.oga"
        );
        assert_eq!(
            FREEDESKTOP_STOP,
            "/usr/share/sounds/freedesktop/stereo/dialog-information.oga"
        );
        assert_eq!(
            FREEDESKTOP_DONE,
            "/usr/share/sounds/freedesktop/stereo/complete.oga"
        );
        assert_eq!(freedesktop_cue_file(CueKind::Start), FREEDESKTOP_START);
        assert_eq!(freedesktop_cue_file(CueKind::Stop), FREEDESKTOP_STOP);
        assert_eq!(freedesktop_cue_file(CueKind::Done), FREEDESKTOP_DONE);
        assert_eq!(LINUX_PLAYERS, &["paplay", "pw-play"]);
    }
}
