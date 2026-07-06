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
//! # Multi-role muting
//!
//! Codex P2 (windows.rs:96, PR #440) — the previous implementation only
//! muted the `eConsole` role default endpoint. On Windows systems where
//! the user has a separate communications or multimedia default (for
//! example a USB headset for Teams/Zoom while speakers remain the console
//! default), meeting apps often render through `eCommunications` or
//! `eMultimedia`, so the auto-mute silently protected the wrong
//! endpoint while the audio most likely to leak into the mic kept
//! playing. This backend now touches the default endpoint for *every*
//! role (0 = eConsole, 1 = eMultimedia, 2 = eCommunications) and saves
//! each role's prior mute state at pin time so the restore returns
//! each endpoint to exactly the state the user had before we touched
//! it — even when the three roles map to three different physical
//! devices with different prior mute states.
//!
//! The runner boundary matches the Linux side: real code shells out to
//! `powershell.exe`; tests substitute an in-memory recorder without
//! spawning anything. Keeping the backends symmetric also means the
//! save/restore state machine in [`crate::output_mute::state`] never
//! learns about either OS.

use std::process::Command;
use std::sync::Mutex;

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
/// `IAudioEndpointVolume` interface, then either enumerate the default
/// endpoint for each render role and report its mute state (`get_all`),
/// or drive `SetMute` on each role's default endpoint from the
/// caller-supplied per-role state (`set_all`).
///
/// The script prints one line per role for `get_all`
/// (`ROLE:<role>:<0|1>`) plus a final `OK`, and a single `OK` for
/// `set_all` on success. Every COM call is wrapped in
/// `Marshal.ThrowExceptionForHR` so a non-zero HRESULT propagates as a
/// PowerShell non-zero exit + stderr — the Rust caller distinguishes
/// real success from silent failure.
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
    // Codex P2 (windows.rs:96, PR #440) — activate the default render
    // endpoint for an arbitrary role (0=eConsole, 1=eMultimedia,
    // 2=eCommunications) so we can operate on each of them in turn
    // rather than only the console default.
    public static IAudioEndpointVolume ActivateForRole(uint role) {
        var mmType = Type.GetTypeFromCLSID(new Guid("BCDE0395-E52F-467C-8E3D-C4579291692E"));
        var enumerator = (IMMDeviceEnumerator)Activator.CreateInstance(mmType);
        IMMDevice device;
        Marshal.ThrowExceptionForHR(enumerator.GetDefaultAudioEndpoint(0, role, out device));
        var iid = typeof(IAudioEndpointVolume).GUID;
        object o;
        Marshal.ThrowExceptionForHR(device.Activate(ref iid, 23, IntPtr.Zero, out o));
        return (IAudioEndpointVolume)o;
    }
}
'@ -ReferencedAssemblies System.Runtime.InteropServices | Out-Null
# Codex P2 (windows.rs:96, PR #440) — every action iterates roles
# 0/1/2 so we touch every render-role default endpoint. If a system
# has only one physical device the three activations return handles
# for the same endpoint and the mute/unmute is idempotent; on a
# split setup (headset comms + speakers console) each endpoint sees
# its own save/restore.
switch ($env:VOICEPI_MUTE_ACTION) {
    'get_all' {
        for ($r = 0; $r -lt 3; $r++) {
            $vol = [OutputMuteHelper]::ActivateForRole($r)
            $m = $false
            [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.GetMute([ref]$m))
            "ROLE:${r}:$(if ($m) { 1 } else { 0 })"
        }
        'OK'
    }
    'set_all' {
        # VOICEPI_MUTE_TARGETS: three comma-separated 0/1 values, one
        # per role (index 0=eConsole, 1=eMultimedia, 2=eCommunications).
        # Missing or unrecognised values default to "leave unchanged",
        # which we implement as "read then write the same value" so a
        # transient parse hiccup can't accidentally flip a mute.
        $targets = ($env:VOICEPI_MUTE_TARGETS -split ',')
        for ($r = 0; $r -lt 3; $r++) {
            $vol = [OutputMuteHelper]::ActivateForRole($r)
            $desired = $null
            if ($r -lt $targets.Length) {
                $t = $targets[$r].Trim()
                if ($t -eq '1') { $desired = $true }
                elseif ($t -eq '0') { $desired = $false }
            }
            if ($desired -eq $null) {
                # No explicit target for this role -- read + write same.
                $m = $false
                [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.GetMute([ref]$m))
                $desired = $m
            }
            $ctx = [Guid]::Empty
            [Runtime.InteropServices.Marshal]::ThrowExceptionForHR($vol.SetMute($desired, [ref]$ctx))
        }
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
///
/// Codex P2 (windows.rs:96, PR #440) — takes `targets` so `set_all` can
/// pass the per-role desired state via `VOICEPI_MUTE_TARGETS` in one
/// PowerShell launch. `None` is used by actions that don't need it
/// (`get_all`) and lets the recorder assert on it too.
pub trait PowerShellRunner: Send + Sync {
    fn run(
        &self,
        script: &str,
        action: &str,
        targets: Option<&str>,
    ) -> Result<PowerShellResult, MuteError>;
}

/// Real-subprocess implementation of [`PowerShellRunner`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPowerShell;

impl PowerShellRunner for SystemPowerShell {
    fn run(
        &self,
        script: &str,
        action: &str,
        targets: Option<&str>,
    ) -> Result<PowerShellResult, MuteError> {
        let mut command = Command::new("powershell.exe");
        command
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .env("VOICEPI_MUTE_ACTION", action);
        if let Some(targets) = targets {
            command.env("VOICEPI_MUTE_TARGETS", targets);
        }
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

/// Snapshot of the default render endpoint mute state per role.
///
/// Index 0 = eConsole, 1 = eMultimedia, 2 = eCommunications — the three
/// documented [ERole](https://learn.microsoft.com/en-us/windows/win32/api/mmdeviceapi/ne-mmdeviceapi-erole)
/// values. Captured at pin time so the restore returns each endpoint
/// exactly to the state it was in before we touched it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorRoleState {
    per_role: [bool; 3],
}

impl PriorRoleState {
    fn all_muted(&self) -> bool {
        self.per_role.iter().all(|m| *m)
    }
    fn as_target_string(&self) -> String {
        format!(
            "{},{},{}",
            if self.per_role[0] { "1" } else { "0" },
            if self.per_role[1] { "1" } else { "0" },
            if self.per_role[2] { "1" } else { "0" },
        )
    }
}

/// CoreAudio-via-PowerShell output-mute backend.
///
/// Codex P2 (windows.rs:96, PR #440) — caches the pre-recording per-role
/// mute state populated by [`Self::pin_current_endpoint`] so the restore
/// on `set_mute(false)` can hand each role its ORIGINAL mute value
/// rather than blanket-unmuting endpoints the user had deliberately
/// muted before recording started.
pub struct WindowsBackend<R: PowerShellRunner = SystemPowerShell> {
    runner: R,
    prior_role_state: Mutex<Option<PriorRoleState>>,
}

impl Default for WindowsBackend<SystemPowerShell> {
    fn default() -> Self {
        Self {
            runner: SystemPowerShell,
            prior_role_state: Mutex::new(None),
        }
    }
}

impl<R: PowerShellRunner> WindowsBackend<R> {
    /// Build a backend around an arbitrary runner. Used by unit tests.
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            prior_role_state: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> Option<PriorRoleState> {
        *self
            .prior_role_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    fn store_snapshot(&self, snap: Option<PriorRoleState>) {
        *self
            .prior_role_state
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = snap;
    }
}

impl<R: PowerShellRunner> OutputMuteBackend for WindowsBackend<R> {
    fn get_mute(&self) -> Result<bool, MuteError> {
        // Codex P2 (windows.rs:96, PR #440) — return whether ALL three
        // role-default endpoints are muted, based on the snapshot
        // captured by [`Self::pin_current_endpoint`]. `state.rs` uses
        // this to decide whether to skip the mute entirely; treating
        // "any endpoint unmuted" as "not muted" lets us go on to mute
        // the unmuted ones and later restore each endpoint's original
        // state. If pin has not run yet (should not happen — the
        // controller always pins before get) we fall back to a live
        // query to avoid a hard failure.
        if let Some(snap) = self.snapshot() {
            return Ok(snap.all_muted());
        }
        let snap = self.query_all_role_states()?;
        self.store_snapshot(Some(snap));
        Ok(snap.all_muted())
    }

    fn set_mute(&self, muted: bool) -> Result<(), MuteError> {
        // Codex P2 (windows.rs:96, PR #440) — mute all three role
        // defaults on set_mute(true); on set_mute(false) restore each
        // role to its ORIGINAL mute state so an endpoint that was
        // already muted before recording stays muted.
        let targets = if muted {
            "1,1,1".to_owned()
        } else if let Some(snap) = self.snapshot() {
            snap.as_target_string()
        } else {
            // No snapshot -> nothing to restore, but be safe and
            // unmute everything.
            "0,0,0".to_owned()
        };
        let result = self
            .runner
            .run(POWERSHELL_SCRIPT, "set_all", Some(&targets))?;
        if !result.status_ok {
            let action = if muted { "mute" } else { "unmute" };
            return Err(MuteError::OsFailure(format!(
                "powershell {action} failed: {}",
                result.stderr.trim(),
            )));
        }
        if result.stdout.lines().any(|line| line.trim() == "OK") {
            Ok(())
        } else {
            let action = if muted { "mute" } else { "unmute" };
            Err(MuteError::UnexpectedOutput(format!(
                "powershell {action} produced no OK line: {:?}",
                result.stdout,
            )))
        }
    }

    fn pin_current_endpoint(&self) -> Result<(), MuteError> {
        // Codex P2 (windows.rs:96, PR #440) — snapshot the per-role
        // default-endpoint mute state so `set_mute(false)` can restore
        // each role to its ORIGINAL value, not blanket-unmute all.
        let snap = self.query_all_role_states()?;
        self.store_snapshot(Some(snap));
        Ok(())
    }

    fn clear_endpoint_pin(&self) {
        self.store_snapshot(None);
    }
}

impl<R: PowerShellRunner> WindowsBackend<R> {
    fn query_all_role_states(&self) -> Result<PriorRoleState, MuteError> {
        let result = self.runner.run(POWERSHELL_SCRIPT, "get_all", None)?;
        if !result.status_ok {
            return Err(MuteError::OsFailure(format!(
                "powershell get_all failed: {}",
                result.stderr.trim(),
            )));
        }
        parse_get_all(&result.stdout)
    }
}

/// Parse the multi-role `ROLE:<role>:<0|1>` output emitted by the
/// `get_all` branch of the PowerShell script.
///
/// Every role in `{0, 1, 2}` must have exactly one line. A missing role
/// surfaces as `UnexpectedOutput` so a script edit that accidentally
/// dropped an iteration fails loudly at pin time rather than silently
/// omitting an endpoint from the restore.
pub fn parse_get_all(stdout: &str) -> Result<PriorRoleState, MuteError> {
    let mut per_role = [false; 3];
    let mut seen = [false; 3];
    for line in stdout.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("ROLE:") else {
            continue;
        };
        let mut parts = rest.split(':');
        let (Some(idx_raw), Some(state_raw)) = (parts.next(), parts.next()) else {
            return Err(MuteError::UnexpectedOutput(format!(
                "malformed ROLE line: {trimmed:?}",
            )));
        };
        let idx: usize = idx_raw.trim().parse().map_err(|_| {
            MuteError::UnexpectedOutput(format!("non-numeric role index: {idx_raw:?}"))
        })?;
        if idx >= 3 {
            return Err(MuteError::UnexpectedOutput(format!(
                "role index out of range 0..=2: {idx}",
            )));
        }
        let muted = match state_raw.trim() {
            "1" | "True" | "true" => true,
            "0" | "False" | "false" => false,
            other => {
                return Err(MuteError::UnexpectedOutput(format!(
                    "unrecognised ROLE mute value: {other:?}",
                )))
            }
        };
        per_role[idx] = muted;
        seen[idx] = true;
    }
    if !seen.iter().all(|s| *s) {
        return Err(MuteError::UnexpectedOutput(format!(
            "missing ROLE line(s) in get_all output: {stdout:?}",
        )));
    }
    Ok(PriorRoleState { per_role })
}

// Codex P2 (windows.rs:96, PR #440) — tests live in a sibling file
// (`windows_tests.rs`) so the impl file stays under AGENTS.md's
// ~500-LOC modularity cap. Prior single-role impl + tests inline
// weighed 493 lines; multi-role adds substantial state so the split
// prevents future creep.
#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
