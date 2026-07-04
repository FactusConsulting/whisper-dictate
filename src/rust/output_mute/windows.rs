//! Windows output-mute backend driven through PowerShell + CoreAudio.
//!
//! Issue #322 asked for the CoreAudio Audio Session API via `windows-rs`.
//! We reach the same `IAudioEndpointVolume::SetMute` /
//! `IAudioEndpointVolume::GetMute` endpoint through PowerShell's
//! `Add-Type` C# interop so we do not have to pull the full `windows`
//! crate into a stock desktop build (it dominates cold Windows CI build
//! time for a P3 feature). The trade-off is one PowerShell launch per
//! record start / stop (~200 ms), which is well under the recording
//! start latency users already accept from the audio stack.
//!
//! The runner boundary matches the Linux side: real code shells out to
//! `powershell.exe`; tests substitute an in-memory recorder without
//! spawning anything. Keeping the backends symmetric also means the
//! save/restore state machine in [`crate::output_mute::state`] never
//! learns about either OS.

use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use crate::output_mute::{MuteError, OutputMuteBackend};

/// Hide the transient PowerShell console on Windows so a record start
/// does not flash a black window at the user. Mirrors the constant used
/// in `runtime.rs`; kept private here so the module does not depend on
/// that giant file.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// PowerShell + C# snippet: define an interop shim for the CoreAudio
/// `IAudioEndpointVolume` interface, get the default render endpoint,
/// then either read or write the mute state.
///
/// Kept as a const string so tests can assert on its shape without
/// spinning a real subprocess. The script prints exactly one line —
/// `MUTE:0` or `MUTE:1` for reads, `OK` for writes — followed by an
/// error line on the failure branch. That token contract is what the
/// [`parse_powershell_output`] parser locks in.
pub const POWERSHELL_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -Language CSharp -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioEndpointVolume {
    int NotUsed1(); int NotUsed2();
    int GetChannelCount(out uint pnChannelCount);
    int NotUsed4(); int NotUsed5(); int NotUsed6(); int NotUsed7(); int NotUsed8(); int NotUsed9(); int NotUsed10(); int NotUsed11();
    int NotUsed12();
    int SetMute([MarshalAs(UnmanagedType.Bool)] bool bMute, ref Guid pguidEventContext);
    int GetMute(out bool pbMute);
}
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDevice {
    int Activate(ref Guid iid, uint dwClsCtx, IntPtr pActivationParams, [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
}
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IMMDeviceEnumerator {
    int NotUsed();
    int GetDefaultAudioEndpoint(uint dataFlow, uint role, out IMMDevice ppDevice);
}
[Guid("BCDE0395-E52F-467C-8E3D-C4579291692E"), ClassInterface(ClassInterfaceType.None)]
public class MMDeviceEnumeratorComClass {}
public static class OutputMuteHelper {
    public static IAudioEndpointVolume Activate() {
        var mmType = Type.GetTypeFromCLSID(new Guid("BCDE0395-E52F-467C-8E3D-C4579291692E"));
        var enumerator = (IMMDeviceEnumerator)Activator.CreateInstance(mmType);
        IMMDevice device;
        Marshal.ThrowExceptionForHR(enumerator.GetDefaultAudioEndpoint(0, 0, out device));
        var iid = typeof(IAudioEndpointVolume).GUID;
        object o;
        Marshal.ThrowExceptionForHR(device.Activate(ref iid, 23, IntPtr.Zero, out o));
        return (IAudioEndpointVolume)o;
    }
}
'@ -ReferencedAssemblies System.Runtime.InteropServices | Out-Null
$vol = [OutputMuteHelper]::Activate()
switch ($env:VOICEPI_MUTE_ACTION) {
    'get' {
        $m = $false; [void]$vol.GetMute([ref]$m)
        if ($m) { 'MUTE:1' } else { 'MUTE:0' }
    }
    'mute' {
        $ctx = [Guid]::Empty
        [void]$vol.SetMute($true, [ref]$ctx)
        'OK'
    }
    'unmute' {
        $ctx = [Guid]::Empty
        [void]$vol.SetMute($false, [ref]$ctx)
        'OK'
    }
    default { throw "unknown VOICEPI_MUTE_ACTION" }
}
"#;

/// Outcome of one PowerShell invocation.
#[derive(Debug, Clone)]
pub struct PowerShellResult {
    pub status_ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// The subprocess boundary the backend calls into. Real code launches
/// `powershell.exe`; tests substitute a recorder.
pub trait PowerShellRunner: Send + Sync {
    /// Run PowerShell with the given script + action env var. Returns
    /// stdout/stderr/exit-status so the parser can distinguish "PS
    /// crashed" (Unavailable) from "the mute call failed" (OsFailure).
    fn run(&self, script: &str, action: &str) -> Result<PowerShellResult, MuteError>;
}

/// Real-subprocess implementation of [`PowerShellRunner`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPowerShell;

impl PowerShellRunner for SystemPowerShell {
    fn run(&self, script: &str, action: &str) -> Result<PowerShellResult, MuteError> {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("VOICEPI_MUTE_ACTION", action);
        #[cfg(windows)]
        {
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let output = command
            .output()
            .map_err(|err| MuteError::Unavailable(format!("powershell spawn failed: {err}")))?;
        Ok(PowerShellResult {
            status_ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// CoreAudio-via-PowerShell output-mute backend.
pub struct WindowsBackend<R: PowerShellRunner = SystemPowerShell> {
    runner: R,
}

impl Default for WindowsBackend<SystemPowerShell> {
    fn default() -> Self {
        Self {
            runner: SystemPowerShell,
        }
    }
}

impl<R: PowerShellRunner> WindowsBackend<R> {
    /// Build a backend around an arbitrary runner. Used by unit tests.
    pub fn with_runner(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: PowerShellRunner> OutputMuteBackend for WindowsBackend<R> {
    fn get_mute(&self) -> Result<bool, MuteError> {
        let result = self.runner.run(POWERSHELL_SCRIPT, "get")?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "powershell get-mute failed: {}",
                result.stderr.trim(),
            )));
        }
        parse_powershell_output(&result.stdout)
    }

    fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
        let action = if muted { "mute" } else { "unmute" };
        let result = self.runner.run(POWERSHELL_SCRIPT, action)?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "powershell {action} failed: {}",
                result.stderr.trim(),
            )));
        }
        if result.stdout.lines().any(|line| line.trim() == "OK") {
            Ok(())
        } else {
            Err(MuteError::UnexpectedOutput(format!(
                "powershell {action} produced no OK line: {:?}",
                result.stdout,
            )))
        }
    }
}

/// Parse the `MUTE:0` / `MUTE:1` token emitted by the get-branch of the
/// PowerShell script. Tolerant of surrounding whitespace + accidental
/// blank lines the shell can inject.
pub fn parse_powershell_output(stdout: &str) -> Result<bool, MuteError> {
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("MUTE:") {
            match rest.trim() {
                "0" | "False" | "false" => return Ok(false),
                "1" | "True" | "true" => return Ok(true),
                _ => {
                    return Err(MuteError::UnexpectedOutput(format!(
                        "unrecognised MUTE value: {rest:?}",
                    )))
                }
            }
        }
    }
    Err(MuteError::UnexpectedOutput(format!(
        "no MUTE: line in powershell output: {stdout:?}",
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockShell {
        inner: Mutex<MockState>,
    }

    #[derive(Default)]
    struct MockState {
        muted: bool,
        calls: Vec<String>,
        spawn_failure: Option<MuteError>,
        force_failure_exit: bool,
    }

    impl PowerShellRunner for Arc<MockShell> {
        fn run(&self, _script: &str, action: &str) -> Result<PowerShellResult, MuteError> {
            let mut state = self.inner.lock().unwrap();
            state.calls.push(action.to_owned());
            if let Some(err) = state.spawn_failure.take() {
                return Err(err);
            }
            if state.force_failure_exit {
                return Ok(PowerShellResult {
                    status_ok: false,
                    stdout: String::new(),
                    stderr: "forced mock ps failure".to_owned(),
                });
            }
            match action {
                "get" => Ok(PowerShellResult {
                    status_ok: true,
                    stdout: format!("MUTE:{}\n", if state.muted { 1 } else { 0 }),
                    stderr: String::new(),
                }),
                "mute" => {
                    state.muted = true;
                    Ok(PowerShellResult {
                        status_ok: true,
                        stdout: "OK\n".to_owned(),
                        stderr: String::new(),
                    })
                }
                "unmute" => {
                    state.muted = false;
                    Ok(PowerShellResult {
                        status_ok: true,
                        stdout: "OK\n".to_owned(),
                        stderr: String::new(),
                    })
                }
                other => Ok(PowerShellResult {
                    status_ok: false,
                    stdout: String::new(),
                    stderr: format!("mock ps: unexpected action {other}"),
                }),
            }
        }
    }

    fn backend(mock: Arc<MockShell>) -> WindowsBackend<Arc<MockShell>> {
        WindowsBackend::with_runner(mock)
    }

    #[test]
    fn parse_output_reads_mute_token() {
        assert!(!parse_powershell_output("MUTE:0\n").unwrap());
        assert!(parse_powershell_output("MUTE:1").unwrap());
        assert!(parse_powershell_output("MUTE: True\n").unwrap());
    }

    #[test]
    fn parse_output_reports_missing_token() {
        let err = parse_powershell_output("noise\n").unwrap_err();
        assert!(matches!(err, MuteError::UnexpectedOutput(_)));
    }

    #[test]
    fn parse_output_reports_bad_value() {
        let err = parse_powershell_output("MUTE:maybe\n").unwrap_err();
        assert!(matches!(err, MuteError::UnexpectedOutput(_)));
    }

    #[test]
    fn get_mute_reads_state_from_mock() {
        let mock = Arc::new(MockShell::default());
        mock.inner.lock().unwrap().muted = true;
        let backend = backend(mock.clone());
        assert!(backend.get_mute().unwrap());
        assert_eq!(mock.inner.lock().unwrap().calls, vec!["get".to_owned()]);
    }

    #[test]
    fn set_mute_emits_mute_then_unmute_actions() {
        let mock = Arc::new(MockShell::default());
        let backend = backend(mock.clone());
        backend.set_mute(true).unwrap();
        backend.set_mute(false).unwrap();
        assert_eq!(
            mock.inner.lock().unwrap().calls,
            vec!["mute".to_owned(), "unmute".to_owned()],
        );
    }

    #[test]
    fn spawn_failure_becomes_unavailable() {
        let mock = Arc::new(MockShell::default());
        mock.inner.lock().unwrap().spawn_failure =
            Some(MuteError::Unavailable("no powershell".to_owned()));
        let backend = backend(mock.clone());
        assert!(matches!(
            backend.get_mute().unwrap_err(),
            MuteError::Unavailable(_)
        ));
    }

    #[test]
    fn nonzero_exit_becomes_os_failure() {
        let mock = Arc::new(MockShell::default());
        mock.inner.lock().unwrap().force_failure_exit = true;
        let backend = backend(mock.clone());
        assert!(matches!(
            backend.get_mute().unwrap_err(),
            MuteError::OsFailure(_)
        ));
        assert!(matches!(
            backend.set_mute(true).unwrap_err(),
            MuteError::OsFailure(_)
        ));
    }

    #[test]
    fn set_mute_without_ok_becomes_unexpected_output() {
        // A run that succeeded but printed nothing useful — treat as
        // malformed so we do not silently claim to have muted.
        struct SilentRunner;
        impl PowerShellRunner for SilentRunner {
            fn run(&self, _script: &str, _action: &str) -> Result<PowerShellResult, MuteError> {
                Ok(PowerShellResult {
                    status_ok: true,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        }
        let backend = WindowsBackend::with_runner(SilentRunner);
        assert!(matches!(
            backend.set_mute(true).unwrap_err(),
            MuteError::UnexpectedOutput(_)
        ));
    }

    #[test]
    fn powershell_script_contains_the_coreaudio_interface_signatures() {
        // Sanity-check the embedded script so a careless edit that
        // removes the SetMute/GetMute lines fails the build rather
        // than showing up as a runtime error on Windows only.
        assert!(POWERSHELL_SCRIPT.contains("SetMute"));
        assert!(POWERSHELL_SCRIPT.contains("GetMute"));
        assert!(POWERSHELL_SCRIPT.contains("IAudioEndpointVolume"));
        assert!(POWERSHELL_SCRIPT.contains("IMMDeviceEnumerator"));
    }
}
