//! PulseAudio / PipeWire output-mute backend, driven through `pactl`.
//!
//! We deliberately shell out to `pactl` (present on both PulseAudio and
//! PipeWire's `pipewire-pulse` compat shim) rather than link against
//! libpulse: it keeps this feature dep-free on stock Linux builds,
//! matches how PipeWire-only distros expose the sink API, and gives
//! us a trivial [`MockPactl`] test double for the integration test.
//!
//! # Commands
//!
//! * `pactl get-sink-mute @DEFAULT_SINK@` prints `Mute: yes` or
//!   `Mute: no` on stdout (case-insensitive). We use `@DEFAULT_SINK@`
//!   rather than a numeric index so PipeWire and PulseAudio both
//!   route to the current default without us having to enumerate.
//! * `pactl set-sink-mute @DEFAULT_SINK@ <1|0>` mutes or unmutes.
//!
//! The command runner is behind the [`PactlRunner`] trait so unit +
//! integration tests can substitute a recorder without spawning a
//! subprocess.

use std::process::Command;

use crate::output_mute::{MuteError, OutputMuteBackend};

/// The `@DEFAULT_SINK@` symbolic name that both PulseAudio and PipeWire
/// resolve to the current default render endpoint. Prefer this to a
/// numeric sink index so a device-switch mid-session still works.
pub const DEFAULT_SINK: &str = "@DEFAULT_SINK@";

/// Outcome of one `pactl` invocation, split so the backend does not
/// need to care whether the exit status came from a real subprocess or
/// a test recorder.
#[derive(Debug, Clone)]
pub struct PactlResult {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The subprocess boundary the backend calls into. Real code drives
/// `std::process::Command`; tests substitute a recorder that captures
/// the arguments without executing anything.
pub trait PactlRunner: Send + Sync {
    fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError>;
}

/// Real-subprocess implementation of [`PactlRunner`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPactl;

impl PactlRunner for SystemPactl {
    fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError> {
        let output = Command::new("pactl")
            .args(args)
            .output()
            .map_err(|err| MuteError::Unavailable(format!("pactl spawn failed: {err}")))?;
        Ok(PactlResult {
            status_ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Output-mute backend that talks to PulseAudio / PipeWire via `pactl`.
///
/// Generic in the runner so [`MockPactl`] can be swapped in for tests.
/// The default constructor uses the real subprocess runner.
///
/// Codex P2 (state.rs:175, PR #440) — [`Self::pin_current_endpoint`]
/// resolves `@DEFAULT_SINK@` to a specific sink name via
/// `pactl get-default-sink` and caches it in [`Self::pinned_sink`]. All
/// subsequent `get_mute` / `set_mute` calls target that concrete sink
/// until the pin is cleared, so a mid-recording default-device switch
/// does not leave the original speakers muted / silently unmute a
/// newly-selected device.
pub struct PactlBackend<R: PactlRunner = SystemPactl> {
    runner: R,
    /// Pinned concrete sink name (e.g. `alsa_output.pci-0000_00_1f.3.analog-stereo`).
    /// `None` → fall back to the `@DEFAULT_SINK@` symbolic name.
    pinned_sink: std::sync::Mutex<Option<String>>,
}

impl Default for PactlBackend<SystemPactl> {
    fn default() -> Self {
        Self {
            runner: SystemPactl,
            pinned_sink: std::sync::Mutex::new(None),
        }
    }
}

impl<R: PactlRunner> PactlBackend<R> {
    /// Construct a backend around an arbitrary runner. Used by tests
    /// and by callers that want a custom `pactl` path.
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            pinned_sink: std::sync::Mutex::new(None),
        }
    }

    /// Resolve the sink identifier that get/set calls should target:
    /// the pinned concrete name if one is set, otherwise the
    /// `@DEFAULT_SINK@` symbolic name.
    fn effective_sink(&self) -> String {
        self.pinned_sink
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| DEFAULT_SINK.to_owned())
    }
}

impl<R: PactlRunner> OutputMuteBackend for PactlBackend<R> {
    fn get_mute(&self) -> Result<bool, MuteError> {
        let sink = self.effective_sink();
        let result = self.runner.run(&["get-sink-mute", &sink])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl get-sink-mute failed: {}",
                result.stderr.trim(),
            )));
        }
        parse_get_sink_mute(&result.stdout)
    }

    fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
        let flag = if muted { "1" } else { "0" };
        let sink = self.effective_sink();
        let result = self.runner.run(&["set-sink-mute", &sink, flag])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl set-sink-mute {flag} failed: {}",
                result.stderr.trim(),
            )));
        }
        Ok(())
    }

    fn pin_current_endpoint(&self) -> Result<(), MuteError> {
        // Codex P2 (state.rs:175, PR #440) — resolve @DEFAULT_SINK@ to
        // a concrete sink name once at recording start so a mid-session
        // default-device switch does not misroute the restore.
        let result = self.runner.run(&["get-default-sink"])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl get-default-sink failed: {}",
                result.stderr.trim(),
            )));
        }
        let name = result.stdout.trim();
        if name.is_empty() {
            return Err(MuteError::UnexpectedOutput(format!(
                "pactl get-default-sink produced no sink name: {:?}",
                result.stdout,
            )));
        }
        *self.pinned_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(name.to_owned());
        Ok(())
    }

    fn clear_endpoint_pin(&self) {
        *self.pinned_sink.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Parse a `pactl get-sink-mute @DEFAULT_SINK@` line.
///
/// `pactl` prints `Mute: yes` / `Mute: no` (English regardless of
/// locale — pactl explicitly hardcodes those tokens). We accept any
/// case + surrounding whitespace so a version-drift in spacing does
/// not silently break the parse.
pub fn parse_get_sink_mute(stdout: &str) -> Result<bool, MuteError> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("Mute:")
            .or_else(|| trimmed.strip_prefix("mute:"))
            .or_else(|| trimmed.strip_prefix("MUTE:"))
        {
            match rest.trim().to_ascii_lowercase().as_str() {
                "yes" | "1" | "true" => return Ok(true),
                "no" | "0" | "false" => return Ok(false),
                _ => {
                    return Err(MuteError::UnexpectedOutput(format!(
                        "unrecognised pactl mute value: {rest:?}",
                    )))
                }
            }
        }
    }
    Err(MuteError::UnexpectedOutput(format!(
        "no `Mute:` line in pactl output: {stdout:?}",
    )))
}

/// Recorder [`PactlRunner`] used by unit + integration tests.
///
/// Every `run` call is captured to `calls` (readable via
/// [`Self::calls`]) so tests can assert on the exact argv sequence.
/// The mock also lets tests script the mute state a `get-sink-mute`
/// call reports and inject failure modes.
#[derive(Default)]
pub struct MockPactl {
    inner: std::sync::Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    muted: bool,
    calls: Vec<Vec<String>>,
    next_error: Option<MuteError>,
    force_failure_exit: bool,
    /// Sink name reported by `pactl get-default-sink`. Empty string
    /// causes the mock to emit blank output (used to exercise the
    /// UnexpectedOutput branch in `pin_current_endpoint`).
    /// Codex P2 (state.rs:175, PR #440).
    default_sink: Option<String>,
}

impl MockPactl {
    /// Preload the state a `get-sink-mute` call will report.
    pub fn set_initial_muted(&self, muted: bool) {
        self.inner.lock().unwrap().muted = muted;
    }

    /// Fail the next `run` call with this error (spawn-time failure).
    pub fn fail_next(&self, err: MuteError) {
        self.inner.lock().unwrap().next_error = Some(err);
    }

    /// Return a non-zero exit + stderr for every subsequent call. Used
    /// to exercise the OsFailure branch without the runner erroring.
    pub fn force_failure_exit(&self, on: bool) {
        self.inner.lock().unwrap().force_failure_exit = on;
    }

    /// Every `pactl <args>` invocation that reached the mock, in order.
    pub fn calls(&self) -> Vec<Vec<String>> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Script the sink name that a `get-default-sink` call returns.
    /// Codex P2 (state.rs:175, PR #440) tests use this to prove the
    /// controller pins the resolved name and routes subsequent
    /// set-sink-mute calls through it.
    pub fn set_default_sink(&self, name: impl Into<String>) {
        self.inner.lock().unwrap().default_sink = Some(name.into());
    }
}

impl PactlRunner for MockPactl {
    fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError> {
        let mut state = self.inner.lock().unwrap();
        state
            .calls
            .push(args.iter().map(|s| (*s).to_owned()).collect());
        if let Some(err) = state.next_error.take() {
            return Err(err);
        }
        if state.force_failure_exit {
            return Ok(PactlResult {
                status_ok: false,
                stdout: String::new(),
                stderr: "mock pactl forced failure".to_owned(),
            });
        }
        match args {
            ["get-default-sink"] => {
                // Codex P2 (state.rs:175, PR #440) — mock the resolved
                // default sink so `pin_current_endpoint` gets a
                // deterministic name to cache.
                let sink = state.default_sink.clone().unwrap_or_default();
                Ok(PactlResult {
                    status_ok: true,
                    stdout: format!("{sink}\n"),
                    stderr: String::new(),
                })
            }
            ["get-sink-mute", _sink] => Ok(PactlResult {
                status_ok: true,
                stdout: format!("Mute: {}\n", if state.muted { "yes" } else { "no" }),
                stderr: String::new(),
            }),
            ["set-sink-mute", _sink, flag] => {
                state.muted = matches!(*flag, "1" | "true" | "yes");
                Ok(PactlResult {
                    status_ok: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
            _ => Ok(PactlResult {
                status_ok: false,
                stdout: String::new(),
                stderr: format!("mock pactl: unexpected args {args:?}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::output_mute::MuteController;

    #[test]
    fn parse_recognises_yes_and_no_and_case() {
        assert!(parse_get_sink_mute("Mute: yes\n").unwrap());
        assert!(!parse_get_sink_mute("Mute: no\n").unwrap());
        assert!(parse_get_sink_mute("mute: YES").unwrap());
        assert!(!parse_get_sink_mute("  Mute:   no   ").unwrap());
        assert!(parse_get_sink_mute("Mute: 1").unwrap());
        assert!(!parse_get_sink_mute("Mute: 0").unwrap());
    }

    #[test]
    fn parse_reports_missing_mute_line() {
        let err = parse_get_sink_mute("Sink #0\nDescription: ...\n").unwrap_err();
        assert!(matches!(err, MuteError::UnexpectedOutput(_)));
    }

    #[test]
    fn parse_reports_unrecognised_mute_value() {
        let err = parse_get_sink_mute("Mute: maybe\n").unwrap_err();
        assert!(matches!(err, MuteError::UnexpectedOutput(_)));
    }

    #[test]
    fn get_mute_uses_default_sink_argv() {
        let mock = Arc::new(MockPactl::default());
        mock.set_initial_muted(true);
        let backend = PactlBackend::with_runner(SharedRunner(mock.clone()));

        assert!(backend.get_mute().unwrap());
        assert_eq!(
            mock.calls(),
            vec![vec!["get-sink-mute".to_owned(), DEFAULT_SINK.to_owned()]],
        );
    }

    #[test]
    fn set_mute_emits_1_and_0_argv() {
        let mock = Arc::new(MockPactl::default());
        let backend = PactlBackend::with_runner(SharedRunner(mock.clone()));

        backend.set_mute(true).unwrap();
        backend.set_mute(false).unwrap();

        assert_eq!(
            mock.calls(),
            vec![
                vec![
                    "set-sink-mute".to_owned(),
                    DEFAULT_SINK.to_owned(),
                    "1".to_owned(),
                ],
                vec![
                    "set-sink-mute".to_owned(),
                    DEFAULT_SINK.to_owned(),
                    "0".to_owned(),
                ],
            ],
        );
    }

    #[test]
    fn spawn_error_bubbles_up_as_unavailable() {
        let mock = Arc::new(MockPactl::default());
        mock.fail_next(MuteError::Unavailable("no pactl binary".to_owned()));
        let backend = PactlBackend::with_runner(SharedRunner(mock.clone()));

        let err = backend.get_mute().unwrap_err();
        assert!(matches!(err, MuteError::Unavailable(_)));
    }

    #[test]
    fn nonzero_exit_surfaces_as_os_failure() {
        let mock = Arc::new(MockPactl::default());
        mock.force_failure_exit(true);
        let backend = PactlBackend::with_runner(SharedRunner(mock.clone()));

        assert!(matches!(
            backend.get_mute().unwrap_err(),
            MuteError::OsFailure(_)
        ));
        assert!(matches!(
            backend.set_mute(true).unwrap_err(),
            MuteError::OsFailure(_)
        ));
    }

    // Integration test on Linux only: drive the controller through a full
    // recording start/stop cycle with the mock recorder and assert the
    // exact pactl commands we emitted. Serves as the golden-path check
    // that nothing between the state machine and the shell boundary
    // regressed. See #322 for context.
    //
    // Codex P2 (state.rs:175, PR #440) — the controller now pins the
    // resolved default sink before muting so the cycle now leads with
    // `pactl get-default-sink` and then uses the returned concrete sink
    // name for the get/set-sink-mute calls.
    #[test]
    fn controller_drives_pactl_through_a_full_recording_cycle() {
        let mock = Arc::new(MockPactl::default());
        mock.set_default_sink("alsa_output.usb.headphones");
        let backend = Arc::new(PactlBackend::with_runner(SharedRunner(mock.clone())));
        let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

        controller.on_recording_start();
        controller.on_recording_stop();

        assert_eq!(
            mock.calls(),
            vec![
                vec!["get-default-sink".to_owned()],
                vec![
                    "get-sink-mute".to_owned(),
                    "alsa_output.usb.headphones".to_owned(),
                ],
                vec![
                    "set-sink-mute".to_owned(),
                    "alsa_output.usb.headphones".to_owned(),
                    "1".to_owned(),
                ],
                vec![
                    "set-sink-mute".to_owned(),
                    "alsa_output.usb.headphones".to_owned(),
                    "0".to_owned(),
                ],
            ],
            "recording cycle must pin the default sink, then save, mute, and restore via pactl",
        );
    }

    #[test]
    fn controller_skips_restore_when_output_was_already_muted() {
        let mock = Arc::new(MockPactl::default());
        mock.set_default_sink("alsa_output.default");
        mock.set_initial_muted(true);
        let backend = Arc::new(PactlBackend::with_runner(SharedRunner(mock.clone())));
        let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

        controller.on_recording_start();
        controller.on_recording_stop();

        // We only ever read; we must not have driven set-sink-mute.
        assert_eq!(
            mock.calls(),
            vec![
                vec!["get-default-sink".to_owned()],
                vec!["get-sink-mute".to_owned(), "alsa_output.default".to_owned(),],
            ],
        );
    }

    /// Codex P2 (state.rs:175, PR #440) — the whole point of endpoint
    /// pinning: even if the user switches the default output device
    /// mid-recording, the stop must un-mute the ORIGINAL sink and not
    /// the new default.
    #[test]
    fn controller_restore_targets_pinned_sink_even_after_default_switch() {
        let mock = Arc::new(MockPactl::default());
        mock.set_default_sink("headphones");
        let backend = Arc::new(PactlBackend::with_runner(SharedRunner(mock.clone())));
        let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

        controller.on_recording_start();
        // The user switches the default device between start and stop.
        mock.set_default_sink("hdmi_speakers");
        controller.on_recording_stop();

        // The restore MUST target `headphones` (the sink we originally
        // muted) not `hdmi_speakers`. The pin also means the pactl args
        // never carry @DEFAULT_SINK@ once we're past pin_current_endpoint.
        let calls = mock.calls();
        assert_eq!(
            calls.first().map(|c| c.as_slice()),
            Some(["get-default-sink".to_owned()].as_slice())
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.contains(&DEFAULT_SINK.to_owned())),
            "no post-pin call may re-resolve @DEFAULT_SINK@: {calls:?}",
        );
        let restore_call = calls.last().expect("controller emitted at least one call");
        assert_eq!(
            restore_call,
            &vec![
                "set-sink-mute".to_owned(),
                "headphones".to_owned(),
                "0".to_owned(),
            ],
            "restore must target the originally-pinned sink, not the new default",
        );
    }

    /// Codex P2 (state.rs:175, PR #440) — `pactl get-default-sink`
    /// producing empty output surfaces as `UnexpectedOutput` so the
    /// controller can log it and fall back to the previous
    /// always-default behaviour instead of pinning "".
    #[test]
    fn pin_current_endpoint_rejects_empty_default_sink_name() {
        let mock = Arc::new(MockPactl::default());
        // Leaving default_sink unset yields empty stdout.
        let backend = PactlBackend::with_runner(SharedRunner(mock.clone()));
        let err = backend.pin_current_endpoint().unwrap_err();
        assert!(matches!(err, MuteError::UnexpectedOutput(_)));
    }

    // Shared runner adapter so tests can hold onto the MockPactl for
    // assertions while the backend consumes its own PactlRunner impl.
    struct SharedRunner(Arc<MockPactl>);
    impl PactlRunner for SharedRunner {
        fn run(&self, args: &[&str]) -> Result<PactlResult, MuteError> {
            self.0.run(args)
        }
    }
}
