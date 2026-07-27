//! WASAPI backend for [`super::SystemAudioDucker`]: enumerate audio
//! sessions on the default render endpoint, skip our own PID, lower
//! every session louder than the target, and remember the previous
//! volume so `restore` can put it back. Mirrors pycaw's
//! `AudioUtilities.GetAllSessions()` + `ISimpleAudioVolume` loop in
//! `src/python/whisper_dictate/vp_audio_ducking.py`.
//!
//! Failure model: [`duck`] returns an Err on the *fatal* setup path
//! (COM init / device enumerator / session manager). Per-session
//! errors inside the loop are silently skipped -- one broken session
//! can't hide the rest. Matches Python, which wraps the whole block in
//! `try/except Exception` and clears the list on any error.
//!
//! Kept in its own file so [`super`] stays focused on the trait +
//! config surface and slots under the AGENTS.md 500 LOC modularity
//! guideline (the `mod.rs` docs + trait alone are ~230 LOC).

use std::process;

use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
};

/// One audio session that [`duck`] lowered, held so [`restore`] can put
/// the volume back. `previous_volume` is what the session had before we
/// changed it (a value in `[0.0, 1.0]`, WASAPI's own master-volume
/// scale, matching pycaw's `GetMasterVolume`).
///
/// SAFETY: `ISimpleAudioVolume` is a COM interface pointer and is not
/// `Send` by default (windows-rs is conservative because COM apartments
/// can have thread-affinity). We store it inside a `Send`-asserted
/// newtype because the whole `SystemAudioDucker` only touches this
/// vector from ONE thread at a time -- the production session lives
/// inside an `Arc<Mutex<...>>` so only the lock holder ever calls
/// `enter()` / `exit()` / drop, and WASAPI's simple-audio-volume
/// interface is thread-safe under serialised access. This matches
/// pycaw's own single-threaded usage pattern in `vp_audio_ducking.py`.
pub(super) struct LoweredSession {
    volume: SendableVolume,
    previous_volume: f32,
}

/// `Send`-asserted newtype around `ISimpleAudioVolume`. See the SAFETY
/// note on [`LoweredSession`] for the invariant that keeps this sound.
struct SendableVolume(ISimpleAudioVolume);

// SAFETY: see the LoweredSession doc-comment. Serialised access via
// the session `Mutex` is the invariant.
unsafe impl Send for SendableVolume {}

/// Lower every audio session on the default render endpoint that is
/// currently louder than `target_volume`, returning one handle per
/// lowered session so [`restore`] can put them back.
pub(super) fn duck(target_volume: f32) -> Result<Vec<LoweredSession>, String> {
    // COM init: `S_OK` (first init) and `S_FALSE` (already initialised
    // for this thread) are both fine; a real failure (e.g.
    // RPC_E_CHANGED_MODE) surfaces so the caller warn_once's it.
    // Matches pycaw / comtypes semantics.
    //
    // SAFETY: `CoInitializeEx` is thread-safe and the returned HRESULT
    // is inspected below. We deliberately do NOT call `CoUninitialize`
    // -- other threads / crates in the process may rely on COM staying
    // initialised (cpal itself calls `CoInitializeEx` for WASAPI on the
    // capture thread), and Python's pycaw path never uninitialises
    // either.
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if hr.is_err() {
            return Err(format!("CoInitializeEx failed: {hr:?}"));
        }
    }

    // Every COM call below runs on this thread, which we just
    // initialised. The returned `Result` type surfaces any HRESULT
    // failure.
    let sessions = unsafe { enumerate_and_duck(target_volume) };
    sessions.map_err(|e| format!("WASAPI session enumeration: {e}"))
}

/// Restore each lowered session to its previous volume. Iterates in
/// reverse order to match pycaw's `for volume, previous in
/// reversed(self._sessions)` loop. Returns the number of sessions
/// whose volume was actually written back; per-session write failures
/// are silently skipped (`try/except: pass` parity).
pub(super) fn restore(sessions: Vec<LoweredSession>) -> usize {
    let mut restored = 0usize;
    for entry in sessions.into_iter().rev() {
        // SAFETY: SetMasterVolume takes a normalised float and a raw
        // `*const GUID` event-context. A null pointer skips the notify
        // context -- matches pycaw's `SetMasterVolume(value, None)`.
        let ok = unsafe {
            entry
                .volume
                .0
                .SetMasterVolume(entry.previous_volume, std::ptr::null())
        }
        .is_ok();
        if ok {
            restored += 1;
        }
    }
    restored
}

/// The COM loop itself, split out so [`duck`] stays focused on the
/// pre-check + error mapping. Marked `unsafe` because every call inside
/// is a raw COM invocation.
unsafe fn enumerate_and_duck(target_volume: f32) -> windows::core::Result<Vec<LoweredSession>> {
    let enumerator: IMMDeviceEnumerator =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia)? };
    let manager: IAudioSessionManager2 = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let session_enum = unsafe { manager.GetSessionEnumerator()? };
    let count = unsafe { session_enum.GetCount()? };
    let current_pid = process::id();

    let mut lowered: Vec<LoweredSession> = Vec::new();
    for i in 0..count {
        let Ok(control) = (unsafe { session_enum.GetSession(i) }) else {
            continue;
        };
        let control2: IAudioSessionControl2 = match control.cast() {
            Ok(c) => c,
            // Not every session exposes IAudioSessionControl2 (rare
            // legacy paths). Skip rather than aborting the whole
            // enumeration -- matches Python's per-session `try/except`
            // swallow inside pycaw.
            Err(_) => continue,
        };
        // Skip our own process: never duck the dictation cue sounds,
        // matches `if session.Process.pid == current_pid: continue` in
        // Python.
        let pid = unsafe { control2.GetProcessId() }.unwrap_or(0);
        if pid == current_pid {
            continue;
        }
        let volume: ISimpleAudioVolume = match control.cast() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let previous = match unsafe { volume.GetMasterVolume() } {
            Ok(v) => v,
            Err(_) => continue,
        };
        if previous <= target_volume {
            // Nothing to lower -- session is already at or below the
            // target. Matches Python's `if previous > self.target_volume`
            // guard.
            continue;
        }
        if unsafe { volume.SetMasterVolume(target_volume, std::ptr::null()) }.is_err() {
            continue;
        }
        lowered.push(LoweredSession {
            volume: SendableVolume(volume),
            previous_volume: previous,
        });
    }
    Ok(lowered)
}
