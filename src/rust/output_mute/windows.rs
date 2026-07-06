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
// Codex P1 (windows.rs:51, PR #440) — the IAudioEndpointVolume vtable
// order (past IUnknown) is:
//   1  RegisterControlChangeNotify
//   2  UnregisterControlChangeNotify
//   3  GetChannelCount
//   4  SetMasterVolumeLevel
//   5  SetMasterVolumeLevelScalar
//   6  GetMasterVolumeLevel
//   7  GetMasterVolumeLevelScalar
//   8  SetChannelVolumeLevel
//   9  SetChannelVolumeLevelScalar
//   10 GetChannelVolumeLevel
//   11 GetChannelVolumeLevelScalar
//   12 SetMute      <-- must sit immediately after GetChannelVolumeLevelScalar
//   13 GetMute      <-- must sit immediately after SetMute
// A stray NotUsed12 placeholder used to shift SetMute/GetMute into the
// wrong vtable slots, so the script was silently dispatching to
// GetVolumeStepInfo when it thought it was calling SetMute. The primary
// Windows path never actually toggled the mute state until this fix.
//
// Codex P2 (windows.rs:53, PR #440) — GetMute writes a Win32 BOOL
// (4-byte int), but COM interop marshals System.Boolean out params as
// VARIANT_BOOL (2-byte short) by default. Explicit
// [MarshalAs(UnmanagedType.Bool)] on the out parameter matches the
// SetMute argument's marshalling and prevents the caller from reading a
// corrupt / half-truncated buffer for the prior mute state.
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IAudioEndpointVolume {
    int NotUsed1(); int NotUsed2();
    int GetChannelCount(out uint pnChannelCount);
    int NotUsed4(); int NotUsed5(); int NotUsed6(); int NotUsed7(); int NotUsed8(); int NotUsed9(); int NotUsed10(); int NotUsed11();
    int SetMute([MarshalAs(UnmanagedType.Bool)] bool bMute, ref Guid pguidEventContext);
    int GetMute([MarshalAs(UnmanagedType.Bool)] out bool pbMute);
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
# Codex P2 (windows.rs:87, PR #440) — every IAudioEndpointVolume call
# returns an HRESULT. The previous version cast the return to [void],
# which silently swallowed any endpoint/COM failure and then emitted
# OK / MUTE:0 as if the operation had succeeded. Wrapping each call in
# Marshal.ThrowExceptionForHR converts a non-zero HRESULT into a
# ComException that propagates through PowerShell's non-zero exit and
# stderr, letting the Rust caller distinguish real success from silent
# failure.
switch ($env:VOICEPI_MUTE_ACTION) {
    'get' {
        $m = $false
        [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.GetMute([ref]$m))
        if ($m) { 'MUTE:1' } else { 'MUTE:0' }
    }
    'mute' {
        $ctx = [Guid]::Empty
        [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.SetMute($true, [ref]$ctx))
        'OK'
    }
    'unmute' {
        $ctx = [Guid]::Empty
        [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.SetMute($false, [ref]$ctx))
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

    #[test]
    fn iaudio_vtable_places_setmute_and_getmute_in_the_correct_slots() {
        // Codex P1 (windows.rs:51, PR #440) — SetMute must sit
        // immediately after the last NotUsed11 slot (which stands in
        // for GetChannelVolumeLevelScalar in the real vtable) and
        // GetMute must sit immediately after SetMute. A stray
        // NotUsed12 shim used to shift both methods into the wrong
        // vtable slots. Pin the interface signature so a similar
        // future edit fails CI instead of showing up as a silent
        // no-op on Windows only.
        let script = POWERSHELL_SCRIPT;
        let not_used11 = script.find("NotUsed11").expect("NotUsed11 slot present");
        let set_mute = script
            .find("int SetMute([MarshalAs")
            .expect("SetMute signature present");
        let get_mute = script
            .find("int GetMute([MarshalAs")
            .expect("GetMute signature present");
        assert!(
            not_used11 < set_mute && set_mute < get_mute,
            "SetMute must follow the last NotUsed11 slot, GetMute must follow SetMute",
        );

        // The stray NotUsed12 placeholder must be gone: only NotUsed1..
        // NotUsed11 are legal in the current interface. A future review
        // adding a stub back would push the vtable out of alignment
        // again.
        let between = &script[not_used11..set_mute];
        assert!(
            !between.contains("NotUsed12"),
            "no NotUsed12 stub is allowed between the volume methods and SetMute",
        );
    }

    #[test]
    fn getmute_out_parameter_is_marshalled_as_win32_bool() {
        // Codex P2 (windows.rs:53, PR #440) — Win32 BOOL is a 4-byte
        // int, and the default marshalling for a System.Boolean out
        // param is VARIANT_BOOL (2 bytes). Without the explicit
        // MarshalAs the caller reads a truncated / uninitialised
        // buffer for the prior mute state, which then flips the saved
        // "did we mute" bit.
        assert!(
            POWERSHELL_SCRIPT.contains("int GetMute([MarshalAs(UnmanagedType.Bool)] out bool"),
            "GetMute's out parameter must be explicitly marshalled as Win32 BOOL",
        );
    }

    #[test]
    fn powershell_actions_check_hresults_before_reporting_success() {
        // Codex P2 (windows.rs:87, PR #440) — each SetMute/GetMute
        // call must have its HRESULT checked, otherwise a non-zero
        // return silently drops through to "OK" or "MUTE:0".
        // Pin the ThrowExceptionForHR wrapping on every action branch.
        let script = POWERSHELL_SCRIPT;
        for expected in [
            "ThrowExceptionForHR($vol.GetMute",
            "ThrowExceptionForHR($vol.SetMute($true",
            "ThrowExceptionForHR($vol.SetMute($false",
        ] {
            assert!(
                script.contains(expected),
                "powershell script must wrap `{expected}...` in Marshal.ThrowExceptionForHR",
            );
        }
        // Belt-and-braces: the previous `[void]$vol.SetMute(` /
        // `[void]$vol.GetMute(` pattern (which discarded the HRESULT)
        // must not creep back in.
        assert!(
            !script.contains("[void]$vol.SetMute"),
            "SetMute HRESULT must not be discarded",
        );
        assert!(
            !script.contains("[void]$vol.GetMute"),
            "GetMute HRESULT must not be discarded",
        );
    }
}
