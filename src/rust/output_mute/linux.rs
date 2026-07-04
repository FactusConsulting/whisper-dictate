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
pub struct PactlBackend<R: PactlRunner = SystemPactl> {
    runner: R,
}

impl Default for PactlBackend<SystemPactl> {
    fn default() -> Self {
        Self {
            runner: SystemPactl,
        }
    }
}

impl<R: PactlRunner> PactlBackend<R> {
    /// Construct a backend around an arbitrary runner. Used by tests
    /// and by callers that want a custom `pactl` path.
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: PactlRunner> OutputMuteBackend for PactlBackend<R> {
    fn get_mute(&self) -> Result<bool, MuteError> {
        let result = self.runner.run(&["get-sink-mute", DEFAULT_SINK])?;
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
        let result = self.runner.run(&["set-sink-mute", DEFAULT_SINK, flag])?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "pactl set-sink-mute {flag} failed: {}",
                result.stderr.trim(),
            )));
        }
        Ok(())
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
            ["get-sink-mute", DEFAULT_SINK] => Ok(PactlResult {
                status_ok: true,
                stdout: format!("Mute: {}\n", if state.muted { "yes" } else { "no" }),
                stderr: String::new(),
            }),
            ["set-sink-mute", DEFAULT_SINK, flag] => {
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
    #[test]
    fn controller_drives_pactl_through_a_full_recording_cycle() {
        let mock = Arc::new(MockPactl::default());
        let backend = Arc::new(PactlBackend::with_runner(SharedRunner(mock.clone())));
        let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

        controller.on_recording_start();
        controller.on_recording_stop();

        assert_eq!(
            mock.calls(),
            vec![
                vec!["get-sink-mute".to_owned(), DEFAULT_SINK.to_owned()],
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
            "recording cycle must save, mute, then restore via pactl",
        );
    }

    #[test]
    fn controller_skips_restore_when_output_was_already_muted() {
        let mock = Arc::new(MockPactl::default());
        mock.set_initial_muted(true);
        let backend = Arc::new(PactlBackend::with_runner(SharedRunner(mock.clone())));
        let mut controller = MuteController::new(backend as Arc<dyn OutputMuteBackend>);

        controller.on_recording_start();
        controller.on_recording_stop();

        // We only ever read; we must not have driven set-sink-mute.
        assert_eq!(
            mock.calls(),
            vec![vec!["get-sink-mute".to_owned(), DEFAULT_SINK.to_owned()]],
        );
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
