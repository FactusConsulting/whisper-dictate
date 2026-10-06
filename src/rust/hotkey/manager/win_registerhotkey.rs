//! Windows `RegisterHotKey` driver — bypasses the WH_KEYBOARD_LL hook
//! chain so PTT chords fire in the GUI process even when third-party apps
//! (Steam / Logitech Options+ / G HUB, screen-capture tools, …) have LL
//! hooks installed that filter function keys or Ctrl.
//!
//! ## Root cause this fixes
//!
//! `rdev::listen` on Windows installs a `WH_KEYBOARD_LL` hook. LL hooks
//! form a chain — every installed hook runs sequentially for every
//! keystroke, and any hook can consume (`return 1`) the event so
//! downstream hooks (including ours) never see it. The diagnostic log
//! showed exactly this pattern in
//! `whisper-dictate-gui.exe`: letters + digits + Shift + Windows key
//! all reached our rdev callback, but `f9`, `ctrl_l`, and `pause` never
//! did — a signature of an upstream LL hook filtering the "hotkey-shaped"
//! events out of the chain. The same binary running the CLI verb
//! (`whisper-dictate.exe dictate-run`) saw every key fine, because that
//! process's LL-hook context is different (console subsystem, no
//! GUI-scoped filter apps attached).
//!
//! `RegisterHotKey` sits at a different Windows layer: the OS delivers
//! `WM_HOTKEY` through USER32's message routing after the LL-hook chain
//! has already run, so consume-decisions upstream do not block it. The
//! chord fires reliably regardless of the LL-hook chain state.
//!
//! ## Limitations (documented for users)
//!
//! * **Modifier-only chords are NOT supported.** RegisterHotKey requires
//!   at least one non-modifier virtual key (function key, letter, digit,
//!   space, escape, tab, or enter). Bindings like bare `ctrl_l` or
//!   `shift_r+alt_l` fail at register time with a clear message; users
//!   who need modifier-only chords must set `VOICEPI_HOTKEY_DRIVER=rdev`
//!   or configure a function-key chord instead.
//! * **Chord vocabulary is Windows-defined.** Only the four MOD_* flags
//!   (`Alt`, `Control`, `Shift`, `Win`) plus one VK_ virtual key per
//!   binding — see [`parse_chord`] for the accepted trigger names.
//! * **Release is polled** via `GetAsyncKeyState` because `WM_HOTKEY`
//!   only fires on press. Poll interval is [`RELEASE_POLL_INTERVAL`];
//!   release latency is bounded by that interval + one OS scheduling
//!   quantum, comfortably under the 30 ms press-debounce the
//!   coordinator applies downstream.
//!
//! ## Thread model
//!
//! Exactly one dedicated Windows message-loop thread ("vp-hotkey-win-rh")
//! owns:
//!
//! * the `RegisterHotKey` / `UnregisterHotKey` calls,
//! * a private message queue that `WM_HOTKEY` routes to (`hWnd = NULL`
//!   ⇒ the hotkey posts to the thread that registered it), and
//! * the [`ManagerCommand`] channel receiver, drained via `try_recv`
//!   between `PeekMessage` calls.
//!
//! The single-thread design avoids `PostThreadMessage`-based wake-up
//! plumbing and keeps register/unregister on the SAME thread that owns
//! the hotkey — a Windows requirement (`UnregisterHotKey` must be called
//! from the registering thread, else it silently fails).

#![cfg(target_os = "windows")]

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::driver_common::{
    manager_channel, ManagerCommand, ManagerHandle, ManagerThread, RawTap, SpawnError,
};
use super::tracker::TrackerOutput;
use crate::hotkey::inject_guard::InjectionGuard;

mod chord;
mod modifiers;
mod registration;
mod state;

#[cfg(test)]
pub(crate) use chord::{is_side_specific_modifier, vk_from_trigger_name};
pub use chord::{parse_chord, ParsedChord};
#[cfg(test)]
pub(crate) use modifiers::required_modifier_vk_groups;
use modifiers::required_modifiers_down;
pub(crate) use registration::{plan_register, RegisterPlan};
pub(crate) use state::{advance_state, LoopEmit, LoopState, LoopStimulus};

// ---------------------------------------------------------------------------
// Raw Win32 FFI. Declared inline (not via the `windows` crate) so this driver
// pulls in ZERO new cargo deps — the surface area is tiny (5 functions +
// MSG struct + constants) and the FFI signatures are stable Win32 API.
// ---------------------------------------------------------------------------

#[allow(non_camel_case_types)]
type HWND = *mut core::ffi::c_void;
#[allow(non_camel_case_types)]
type BOOL = i32;
#[allow(non_camel_case_types)]
type LPARAM = isize;
#[allow(non_camel_case_types)]
type WPARAM = usize;
#[allow(non_camel_case_types)]
type DWORD = u32;
#[allow(non_camel_case_types)]
type UINT = u32;

#[repr(C)]
struct Point {
    _x: i32,
    _y: i32,
}

#[repr(C)]
struct Msg {
    _hwnd: HWND,
    message: UINT,
    w_param: WPARAM,
    _l_param: LPARAM,
    _time: DWORD,
    _pt: Point,
    _l_private: DWORD,
}

// WM_HOTKEY: delivered to the registering thread when the chord fires.
const WM_HOTKEY: UINT = 0x0312;
// WM_QUIT: end the msg loop on shutdown.
const WM_QUIT: UINT = 0x0012;
const PM_REMOVE: UINT = 0x0001;

// RegisterHotKey modifier flags.
pub(crate) const MOD_ALT: u32 = 0x0001;
pub(crate) const MOD_CONTROL: u32 = 0x0002;
pub(crate) const MOD_SHIFT: u32 = 0x0004;
pub(crate) const MOD_WIN: u32 = 0x0008;
// MOD_NOREPEAT suppresses OS key-repeat (Win 7+). We want a single
// WM_HOTKEY per physical press — the tracker suppresses repeats on rdev,
// but here the OS gives us the flag directly, so use it.
const MOD_NOREPEAT: u32 = 0x4000;

#[link(name = "user32")]
extern "system" {
    fn RegisterHotKey(hWnd: HWND, id: i32, fsModifiers: UINT, vk: UINT) -> BOOL;
    fn UnregisterHotKey(hWnd: HWND, id: i32) -> BOOL;
    fn PeekMessageW(
        lpMsg: *mut Msg,
        hWnd: HWND,
        wMsgFilterMin: UINT,
        wMsgFilterMax: UINT,
        wRemoveMsg: UINT,
    ) -> BOOL;
    fn GetAsyncKeyState(vKey: i32) -> i16;
    fn GetLastError() -> DWORD;
}

/// Poll interval for detecting hotkey RELEASE via `GetAsyncKeyState`.
/// `WM_HOTKEY` only fires on press, so we poll the trigger VK while a
/// chord is active. 15 ms ≈ one Windows scheduler quantum — fast enough
/// that the coordinator's 30 ms press debounce never batches two
/// distinct release edges, slow enough to keep the loop thread near
/// idle when a chord IS held (a few polls per press cycle).
const RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Hotkey id passed to `RegisterHotKey`. Only ever one hotkey per driver
/// instance, so the value is arbitrary — any 0..0xBFFF is legal per the
/// Windows docs.
const HOTKEY_ID: i32 = 1;
const COPY_LAST_HOTKEY_ID: i32 = 2;
const PASTE_LAST_HOTKEY_ID: i32 = 3;
const CYCLE_MODE_HOTKEY_ID: i32 = 4;
const RAW_MODE_HOTKEY_ID: i32 = 5;
const CLEAN_MODE_HOTKEY_ID: i32 = 6;

/// Which mode shortcut a registration slot holds. The three mode
/// shortcuts share one registration/dispatch path — they are one-shot
/// actions that only change settings (never inject), so unlike paste-last
/// they need no suppression during a recording and no ctrl+v restore
/// coordination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeHotkeyKind {
    Cycle,
    Raw,
    Clean,
}

impl ModeHotkeyKind {
    /// Index into [`LoopState::mode_hotkeys`].
    pub(crate) fn slot(self) -> usize {
        match self {
            ModeHotkeyKind::Cycle => 0,
            ModeHotkeyKind::Raw => 1,
            ModeHotkeyKind::Clean => 2,
        }
    }

    /// The `RegisterHotKey` id for this kind.
    fn hotkey_id(self) -> i32 {
        match self {
            ModeHotkeyKind::Cycle => CYCLE_MODE_HOTKEY_ID,
            ModeHotkeyKind::Raw => RAW_MODE_HOTKEY_ID,
            ModeHotkeyKind::Clean => CLEAN_MODE_HOTKEY_ID,
        }
    }

    /// The tracker output the dispatch arm fires for this kind.
    fn output(self) -> TrackerOutput {
        match self {
            ModeHotkeyKind::Cycle => TrackerOutput::CycleMode,
            ModeHotkeyKind::Raw => TrackerOutput::RawMode,
            ModeHotkeyKind::Clean => TrackerOutput::CleanMode,
        }
    }

    /// Human-readable label for diagnostics and ack errors.
    fn label(self) -> &'static str {
        match self {
            ModeHotkeyKind::Cycle => "cycle-mode",
            ModeHotkeyKind::Raw => "raw-mode",
            ModeHotkeyKind::Clean => "clean-mode",
        }
    }

    /// The kinds that share the mode-hotkey registration path, in slot
    /// order. Used by the conflict checks and the unregister-all path.
    const ALL: [ModeHotkeyKind; 3] = [
        ModeHotkeyKind::Cycle,
        ModeHotkeyKind::Raw,
        ModeHotkeyKind::Clean,
    ];
}

/// Spawn the RegisterHotKey driver. Same return shape as
/// [`super::rdev_driver::spawn_with_raw_tap`] so the manager-level
/// selector can dispatch to either backend behind a single Result type.
///
/// `injection_guard` is accepted for signature parity but is a no-op
/// here — RegisterHotKey delivers ONLY the registered chord event
/// (no arbitrary keystrokes to filter for self-injection), so there is
/// nothing the injection guard needs to gate.
///
/// `raw_tap` is likewise accepted but unused: the diagnostic
/// `hotkey capture` verb's raw tap reports every OS keystroke rdev
/// delivers; RegisterHotKey only surfaces `WM_HOTKEY` (which the
/// coordinator sees anyway via `TrackerOutput::ChordPress`). Users who
/// need the per-key trace stream must select `--driver rdev`.
pub fn spawn_with_raw_tap<F, R>(
    _injection_guard: Arc<InjectionGuard>,
    on_output: F,
    _raw_tap: R,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (handle, cmd_rx) = manager_channel();
    let on_output = Arc::new(on_output);

    // Wire the shared liveness
    // atomic (originally added for the rdev driver) to this backend's
    // dedicated message-loop thread as well. Without this, a
    // `self-test hotkey-boot --driver register` run whose
    // `vp-hotkey-win-rh` thread exited or panicked during the hold
    // window would still see `HotkeyHandle::is_listener_alive() ==
    // true` and report PASS on the exact dead-listener regression the
    // signal exists to catch. A drop-guard flips the flag when the
    // thread ends for any reason (return, panic, RegisterHotKey
    // failure that broke `run_msg_loop` out early); a shipping
    // handle's thread runs for the process lifetime, so this only
    // ever fires on the regression path or on the test-only
    // `handle.shutdown()` teardown — both of which are exactly what
    // callers of `is_listener_alive` are asking about.
    let listener_alive_signal = handle.listener_alive_flag();

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let loop_on_output = Arc::clone(&on_output);
    let listener_alive_for_thread = Arc::clone(&listener_alive_signal);
    let join = thread::Builder::new()
        .name("vp-hotkey-win-rh".to_owned())
        .spawn(move || {
            // Drop-guard mirrors the rdev backend's pattern: flip the
            // atomic to `false` when the thread ends for ANY reason
            // (normal return, `run_msg_loop` bailing early, panic
            // unwinding through here). discussion
            // 3664983427.
            struct AliveGuard(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for AliveGuard {
                fn drop(&mut self) {
                    self.0.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
            let _alive_guard = AliveGuard(Arc::clone(&listener_alive_for_thread));
            let _ = ready_tx.send(Ok(()));
            crate::diag::log!(
                "[hotkey/win_registerhotkey] msg-loop thread started \
                 (bypasses WH_KEYBOARD_LL hook chain)"
            );
            // Flip the alive
            // flag to `true` HERE — after any potentially-stalling
            // pre-loop `diag::log!` has returned, and just before we
            // enter `run_msg_loop` which owns the RegisterHotKey
            // subscription for the rest of the thread's lifetime.
            // Same rationale as the rdev branch (see there for the
            // full story): manager-channel default is `false`, so a
            // stalled AppData sink would keep the flag `false` and
            // the boot self-test correctly reports the dead-listener
            // wedge rather than passing on a listener that never
            // reached the OS API.
            listener_alive_for_thread.store(true, std::sync::atomic::Ordering::Relaxed);
            run_msg_loop(cmd_rx, loop_on_output);
            // Same ordering rationale as the
            // rdev branch: flip the atomic BEFORE the post-loop
            // diagnostic log so a stalled diag sink cannot mask the
            // dead-listener state to `is_listener_alive()` callers
            // during their hold window. The drop-guard above is the
            // panic-safety belt; this explicit store is the primary
            // signal on the normal-exit path.
            listener_alive_for_thread.store(false, std::sync::atomic::Ordering::Relaxed);
            crate::diag::log!(
                "[hotkey/win_registerhotkey] msg-loop thread exiting \
                 (WM_QUIT drained or Shutdown command received); \
                 RegisterHotKey chord is no longer live on this process."
            );
        })
        .map_err(|e| {
            SpawnError::ListenerStartup(format!("win_registerhotkey thread spawn failed: {e}"))
        })?;

    match ready_rx.recv_timeout(Duration::from_millis(500)) {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return Err(SpawnError::ListenerStartup(msg)),
        Err(_) => return Err(SpawnError::ListenerHung),
    }

    Ok((handle, ManagerThread::from_join(join)))
}

fn run_msg_loop<F>(cmd_rx: Receiver<ManagerCommand>, on_output: Arc<F>)
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    let mut state = LoopState::new();
    loop {
        // Drain any pending manager commands FIRST. Commands (register,
        // unregister, shutdown) are cheap; the loop pulls all of them
        // before entering the message-poll wait so a rapid re-register
        // does not queue behind a single peek/poll iteration.
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if !handle_command(cmd, &mut state, &on_output) {
                        cleanup(&mut state);
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    cleanup(&mut state);
                    return;
                }
            }
        }

        // Peek at any WM_HOTKEY messages the OS has delivered.
        unsafe {
            let mut msg: Msg = std::mem::zeroed();
            if PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    cleanup(&mut state);
                    return;
                }
                if msg.message == WM_HOTKEY {
                    dispatch_hotkey_message(msg.w_param, &mut state, &on_output);
                }
                continue; // loop back to drain more messages / commands
            }
        }

        // No message pending. If a chord is armed, poll its release
        // via GetAsyncKeyState (WM_HOTKEY does NOT fire on key-up).
        //
        // The poll covers every required VK, not only the trigger:
        // releasing a required modifier while still holding the trigger
        // (e.g. releasing Ctrl on `ctrl+f9` while F9 is still down) is a
        // chord release, out-of-sync with what the user perceives as the
        // chord end if it is not reported. The poll therefore also asks
        // whether the chord's declared modifiers are still down; if any
        // is not, treat as release.
        if let Some(vk) = state.pressed_trigger {
            let mods = state.registered.as_ref().map(|c| c.mods).unwrap_or(0);
            let stimulus = if async_key_down(vk) && required_modifiers_down(mods) {
                LoopStimulus::PollTriggerDown
            } else {
                LoopStimulus::PollTriggerUp
            };
            emit_transition(&mut state, stimulus, &on_output, "GetAsyncKeyState release");
            if stimulus == LoopStimulus::PollTriggerUp {
                continue;
            }
        }

        // Sleep either until the next poll tick or a short generic
        // interval when idle. Keep the sleep short so command latency
        // stays low; the OS scheduler will still park us efficiently.
        let wait = if state.pressed_trigger.is_some() {
            RELEASE_POLL_INTERVAL
        } else {
            Duration::from_millis(30)
        };
        thread::sleep(wait);
    }
}

/// Only the registration owned by this driver may drive push-to-talk.
/// Windows puts the `RegisterHotKey` ID in `wParam`; a second action
/// registration must never be mistaken for the PTT press/release cycle.
pub(crate) fn is_ptt_hotkey_id(id: usize) -> bool {
    id == HOTKEY_ID as usize
}

pub(crate) fn is_copy_last_hotkey_id(id: usize, state: &LoopState) -> bool {
    id == COPY_LAST_HOTKEY_ID as usize && state.copy_last_registered.is_some()
}

pub(crate) fn is_paste_last_hotkey_id(id: usize, state: &LoopState) -> bool {
    id == PASTE_LAST_HOTKEY_ID as usize && state.paste_last_registered.is_some()
}

/// Map a WM_HOTKEY id to its registered mode shortcut, if any.
pub(crate) fn mode_hotkey_for_id(id: usize, state: &LoopState) -> Option<ModeHotkeyKind> {
    ModeHotkeyKind::ALL
        .into_iter()
        .find(|kind| kind.hotkey_id() as usize == id && state.mode_hotkeys[kind.slot()].is_some())
}

pub(crate) fn dispatch_hotkey_message<F>(id: usize, state: &mut LoopState, on_output: &Arc<F>)
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    if is_ptt_hotkey_id(id) {
        emit_transition(state, LoopStimulus::WmHotkey, on_output, "WM_HOTKEY press");
    } else if is_copy_last_hotkey_id(id, state) {
        (on_output)(TrackerOutput::CopyLast);
    } else if is_paste_last_hotkey_id(id, state) {
        // With a modifier-based PTT (e.g. ctrl+f9) the user can press
        // paste-last while the recording is still active. The paste
        // backend releases held modifiers before typing and this loop's
        // async-key poll would then observe the released modifier and
        // emit ChordRelease, ending the recording although the user is
        // still holding PTT. Drop the action while a recording is
        // active; the user can re-press after it ends.
        if state.pressed_trigger.is_none() {
            (on_output)(TrackerOutput::PasteLast);
        }
    } else if let Some(kind) = mode_hotkey_for_id(id, state) {
        // Mode shortcuts only change settings — they never inject, so
        // unlike paste-last they are safe while a recording is active.
        // The persistence is asynchronous (the mode worker writes the
        // config from its own thread), so a transcription pass that
        // reloads post_mode before the write lands keeps the previous
        // mode; the change reaches every pass that starts after it.
        (on_output)(kind.output());
    }
}

pub(crate) fn same_chord(a: &ParsedChord, b: &ParsedChord) -> bool {
    a.mods == b.mods && a.vk == b.vk
}

/// Apply one loop stimulus and, if the pure state helper emits a
/// press / release, fire it through `on_output` and log a diagnostic
/// line naming the source (WM_HOTKEY vs. GetAsyncKeyState) so the
/// tee file distinguishes the two paths.
fn emit_transition<F>(
    state: &mut LoopState,
    stimulus: LoopStimulus,
    on_output: &Arc<F>,
    source: &str,
) where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    match advance_state(state, stimulus) {
        LoopEmit::Press => {
            let chord_label = state
                .registered
                .as_ref()
                .map(|c| c.display.as_str())
                .unwrap_or("<unregistered>");
            crate::diag::log!(
                "[hotkey/win_registerhotkey] press fired via {source} for chord={chord_label}"
            );
            (on_output)(TrackerOutput::ChordPress);
        }
        LoopEmit::Release => {
            crate::diag::log!("[hotkey/win_registerhotkey] release fired via {source}");
            (on_output)(TrackerOutput::ChordRelease);
        }
        LoopEmit::None => {}
    }
}

/// Apply one manager command. Returns `false` on `Shutdown` so the
/// caller can break out of the loop; every other command returns
/// `true` regardless of the OS call's success (the ack channel already
/// carries the outcome to the caller of [`ManagerHandle::register`]).
fn handle_command<F>(cmd: ManagerCommand, state: &mut LoopState, _on_output: &Arc<F>) -> bool
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    match cmd {
        ManagerCommand::Register { targets, ack } => {
            // Validate the NEW chord BEFORE unregistering the old one.
            //
            // If parsing ran after unregistering, a failed parse
            // (side-specific modifier, unsupported trigger, etc.) would
            // tear down the working chord AND never install the new
            // one, leaving the process without a listener while the
            // supervisor's `resume` path only logs the error. Parsing
            // first, then unregistering, preserves the working binding
            // when the caller sends a bad new chord — the supervisor
            // can then keep the existing registration or recreate with rdev.
            //
            // The parse gate is extracted to `plan_register` so the
            // "reject-without-state-change" contract is unit-testable
            // without a live RegisterHotKey install.
            let chord = match plan_register(&targets) {
                RegisterPlan::Install(c) => c,
                RegisterPlan::Reject(msg) => {
                    crate::diag::log!(
                        "[hotkey/win_registerhotkey] parse failed for new chord \
                         (previous binding kept intact): {msg}"
                    );
                    let _ = ack.send(Err(msg));
                    return true;
                }
            };
            if state
                .copy_last_registered
                .as_ref()
                .is_some_and(|action| same_chord(action, &chord))
            {
                let _ = ack.send(Err("PTT and copy-last shortcuts must differ".to_owned()));
                return true;
            }
            if state
                .paste_last_registered
                .as_ref()
                .is_some_and(|action| same_chord(action, &chord))
            {
                let _ = ack.send(Err("PTT and paste-last shortcuts must differ".to_owned()));
                return true;
            }
            // Paste-last's paste burst injects the plain Ctrl+V chord and
            // RegisterHotKey posts WM_HOTKEY for synthetic key events, so
            // with paste-last registered a ctrl+v PTT binding starts an
            // unintended recording on every paste burst. The paste arm refuses the same
            // combination in the other registration order.
            if state.paste_last_registered.is_some() {
                if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                    if same_chord(&chord, &ctrl_v) {
                        let _ = ack.send(Err(
                            "PTT cannot use ctrl+v while paste-last is registered: the injected paste chord would re-trigger PTT"
                                .to_owned(),
                        ));
                        return true;
                    }
                }
            }
            // Parse succeeded — safe to swap the OS registration.
            // RegisterHotKey fails with ERROR_HOTKEY_ALREADY_REGISTERED
            // if the previous binding is still installed, so tear it
            // down here (after the parse gate).
            unregister_current(state);
            let ok = unsafe {
                RegisterHotKey(
                    std::ptr::null_mut(),
                    HOTKEY_ID,
                    chord.mods | MOD_NOREPEAT,
                    chord.vk,
                )
            };
            if ok != 0 {
                crate::diag::log!(
                    "[hotkey/win_registerhotkey] registered chord={} \
                     (mods=0x{:04x} vk=0x{:02x}) hotkey_id={}",
                    chord.display,
                    chord.mods,
                    chord.vk,
                    HOTKEY_ID,
                );
                state.registered = Some(chord);
                state.pressed_trigger = None;
                let _ = ack.send(Ok(()));
            } else {
                let err = unsafe { GetLastError() };
                let msg = format!(
                    "RegisterHotKey failed for chord={} (mods=0x{:04x} \
                     vk=0x{:02x}); GetLastError=0x{:08x} - another app \
                     may already own this chord",
                    chord.display, chord.mods, chord.vk, err
                );
                crate::diag::log!("[hotkey/win_registerhotkey] {msg}");
                let _ = ack.send(Err(msg));
            }
            true
        }
        ManagerCommand::RegisterCopyLast { targets, ack } => {
            let chord = match plan_register(&targets) {
                RegisterPlan::Install(chord) => chord,
                RegisterPlan::Reject(message) => {
                    let _ = ack.send(Err(message));
                    return true;
                }
            };
            if state
                .registered
                .as_ref()
                .is_some_and(|ptt| same_chord(ptt, &chord))
            {
                let _ = ack.send(Err("PTT and copy-last shortcuts must differ".to_owned()));
                return true;
            }
            if state
                .paste_last_registered
                .as_ref()
                .is_some_and(|action| same_chord(action, &chord))
            {
                let _ = ack.send(Err(
                    "copy-last and paste-last shortcuts must differ".to_owned()
                ));
                return true;
            }
            // Paste-last's paste burst injects the plain Ctrl+V chord and
            // RegisterHotKey posts WM_HOTKEY for synthetic key events, so
            // with paste-last registered a ctrl+v copy binding fires
            // copy-last on every paste burst: the copy worker runs
            // mid-paste, cancels paste-last's fresh restore cycle via
            // copy_with_pending_restore_cancelled, and leaves the
            // transcript on the clipboard instead of restoring the
            // user's contents .
            if state.paste_last_registered.is_some() {
                if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                    if same_chord(&chord, &ctrl_v) {
                        let _ = ack.send(Err(
                            "copy-last cannot use ctrl+v while paste-last is registered: the injected paste chord would re-trigger copy-last"
                                .to_owned(),
                        ));
                        return true;
                    }
                }
            }
            unregister_copy_last(state);
            let ok = unsafe {
                RegisterHotKey(
                    std::ptr::null_mut(),
                    COPY_LAST_HOTKEY_ID,
                    chord.mods | MOD_NOREPEAT,
                    chord.vk,
                )
            };
            if ok != 0 {
                crate::diag::log!(
                    "[hotkey/win_registerhotkey] registered copy-last chord={} hotkey_id={}",
                    chord.display,
                    COPY_LAST_HOTKEY_ID,
                );
                state.copy_last_registered = Some(chord);
                let _ = ack.send(Ok(()));
            } else {
                let error = unsafe { GetLastError() };
                let _ = ack.send(Err(format!(
                    "RegisterHotKey failed for copy-last chord={}; GetLastError=0x{error:08x}",
                    chord.display
                )));
            }
            true
        }
        ManagerCommand::RegisterPasteLast { targets, ack } => {
            let chord = match plan_register(&targets) {
                RegisterPlan::Install(chord) => chord,
                RegisterPlan::Reject(message) => {
                    let _ = ack.send(Err(message));
                    return true;
                }
            };
            // The paste arm injects the plain Ctrl+V chord itself and
            // RegisterHotKey posts WM_HOTKEY for synthetic key events, so
            // binding paste-last to exactly ctrl+v re-triggers its own
            // paste the moment the burst starts (the worker clears its
            // busy flag before the message loop drains the synthesized
            // hotkey). Reject it outright .
            if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                if same_chord(&chord, &ctrl_v) {
                    let _ = ack.send(Err(
                        "paste-last cannot use ctrl+v: the injected paste chord would re-trigger the shortcut"
                            .to_owned(),
                    ));
                    return true;
                }
            }
            // Symmetric guard : when
            // copy-last already owns ctrl+v, enabling paste-last would
            // make every paste burst re-trigger copy-last and cancel its
            // fresh restore cycle. The copy arm refuses the same
            // combination in the other registration order.
            if let Some(copy) = state.copy_last_registered.as_ref() {
                if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                    if same_chord(copy, &ctrl_v) {
                        let _ = ack.send(Err(
                            "paste-last cannot be registered while copy-last uses ctrl+v: the injected paste chord would re-trigger copy-last"
                                .to_owned(),
                        ));
                        return true;
                    }
                }
            }
            // Symmetric guard : when
            // PTT already owns ctrl+v, enabling paste-last would start
            // an unintended recording on every paste burst. The PTT arm
            // refuses the same combination in the other order.
            if let Some(ptt) = state.registered.as_ref() {
                if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
                    if same_chord(ptt, &ctrl_v) {
                        let _ = ack.send(Err(
                            "paste-last cannot be registered while PTT uses ctrl+v: the injected paste chord would re-trigger PTT"
                                .to_owned(),
                        ));
                        return true;
                    }
                }
            }
            if state
                .registered
                .as_ref()
                .is_some_and(|ptt| same_chord(ptt, &chord))
            {
                let _ = ack.send(Err("PTT and paste-last shortcuts must differ".to_owned()));
                return true;
            }
            if state
                .copy_last_registered
                .as_ref()
                .is_some_and(|action| same_chord(action, &chord))
            {
                let _ = ack.send(Err(
                    "copy-last and paste-last shortcuts must differ".to_owned()
                ));
                return true;
            }
            unregister_paste_last(state);
            let ok = unsafe {
                RegisterHotKey(
                    std::ptr::null_mut(),
                    PASTE_LAST_HOTKEY_ID,
                    chord.mods | MOD_NOREPEAT,
                    chord.vk,
                )
            };
            if ok != 0 {
                crate::diag::log!(
                    "[hotkey/win_registerhotkey] registered paste-last chord={} hotkey_id={}",
                    chord.display,
                    PASTE_LAST_HOTKEY_ID,
                );
                state.paste_last_registered = Some(chord);
                let _ = ack.send(Ok(()));
            } else {
                let error = unsafe { GetLastError() };
                let _ = ack.send(Err(format!(
                    "RegisterHotKey failed for paste-last chord={}; GetLastError=0x{error:08x}",
                    chord.display
                )));
            }
            true
        }
        ManagerCommand::RegisterCycleMode { targets, ack } => {
            register_mode_hotkey(ModeHotkeyKind::Cycle, targets, ack, state);
            true
        }
        ManagerCommand::RegisterRawMode { targets, ack } => {
            register_mode_hotkey(ModeHotkeyKind::Raw, targets, ack, state);
            true
        }
        ManagerCommand::RegisterCleanMode { targets, ack } => {
            register_mode_hotkey(ModeHotkeyKind::Clean, targets, ack, state);
            true
        }
        ManagerCommand::Unregister { ack } => {
            unregister_current(state);
            unregister_copy_last(state);
            unregister_paste_last(state);
            for kind in ModeHotkeyKind::ALL {
                unregister_mode_hotkey(kind, state);
            }
            state.pressed_trigger = None;
            let _ = ack.send(Ok(()));
            true
        }
        ManagerCommand::Shutdown => false,
    }
}

fn unregister_current(state: &mut LoopState) {
    if state.registered.take().is_some() {
        let ok = unsafe { UnregisterHotKey(std::ptr::null_mut(), HOTKEY_ID) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            crate::diag::log!(
                "[hotkey/win_registerhotkey] UnregisterHotKey failed; \
                 GetLastError=0x{err:08x} (continuing - will re-register on next command)"
            );
        }
    }
}

fn unregister_copy_last(state: &mut LoopState) {
    if state.copy_last_registered.take().is_some() {
        let ok = unsafe { UnregisterHotKey(std::ptr::null_mut(), COPY_LAST_HOTKEY_ID) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            crate::diag::log!(
                "[hotkey/win_registerhotkey] copy-last UnregisterHotKey failed; GetLastError=0x{error:08x}"
            );
        }
    }
}

fn unregister_paste_last(state: &mut LoopState) {
    if state.paste_last_registered.take().is_some() {
        let ok = unsafe { UnregisterHotKey(std::ptr::null_mut(), PASTE_LAST_HOTKEY_ID) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            crate::diag::log!(
                "[hotkey/win_registerhotkey] paste-last UnregisterHotKey failed; GetLastError=0x{error:08x}"
            );
        }
    }
}

/// Register (or replace) one mode shortcut. Mode shortcuts are one-shot
/// actions that only change settings — they never inject, so unlike
/// paste-last there is no ctrl+v restore coordination and no recording
/// suppression. The shared validation shrinks to "registered chords must
/// be distinct so a WM_HOTKEY id maps to exactly one meaning" plus a
/// ctrl+v refusal (RegisterHotKey intercepts the chord globally, so a
/// ctrl+v mode binding would swallow the user's normal pasting).
fn register_mode_hotkey(
    kind: ModeHotkeyKind,
    targets: Vec<String>,
    ack: std::sync::mpsc::Sender<Result<(), String>>,
    state: &mut LoopState,
) {
    let chord = match plan_register(&targets) {
        RegisterPlan::Install(chord) => chord,
        RegisterPlan::Reject(message) => {
            let _ = ack.send(Err(message));
            return;
        }
    };
    if let Ok(ctrl_v) = parse_chord(&["ctrl".to_owned(), "v".to_owned()]) {
        if same_chord(&chord, &ctrl_v) {
            let _ = ack.send(Err(format!(
                "{} shortcut cannot use ctrl+v: the binding would intercept normal pasting",
                kind.label()
            )));
            return;
        }
    }
    if state
        .registered
        .as_ref()
        .is_some_and(|ptt| same_chord(ptt, &chord))
    {
        let _ = ack.send(Err(format!(
            "PTT and {} shortcuts must differ",
            kind.label()
        )));
        return;
    }
    if state
        .copy_last_registered
        .as_ref()
        .is_some_and(|action| same_chord(action, &chord))
    {
        let _ = ack.send(Err(format!(
            "copy-last and {} shortcuts must differ",
            kind.label()
        )));
        return;
    }
    if state
        .paste_last_registered
        .as_ref()
        .is_some_and(|action| same_chord(action, &chord))
    {
        let _ = ack.send(Err(format!(
            "paste-last and {} shortcuts must differ",
            kind.label()
        )));
        return;
    }
    for other in ModeHotkeyKind::ALL {
        if other == kind {
            continue;
        }
        if state.mode_hotkeys[other.slot()]
            .as_ref()
            .is_some_and(|action| same_chord(action, &chord))
        {
            let _ = ack.send(Err(format!(
                "{} and {} shortcuts must differ",
                other.label(),
                kind.label()
            )));
            return;
        }
    }
    unregister_mode_hotkey(kind, state);
    let ok = unsafe {
        RegisterHotKey(
            std::ptr::null_mut(),
            kind.hotkey_id(),
            chord.mods | MOD_NOREPEAT,
            chord.vk,
        )
    };
    if ok != 0 {
        crate::diag::log!(
            "[hotkey/win_registerhotkey] registered {} chord={} hotkey_id={}",
            kind.label(),
            chord.display,
            kind.hotkey_id(),
        );
        state.mode_hotkeys[kind.slot()] = Some(chord);
        let _ = ack.send(Ok(()));
    } else {
        let error = unsafe { GetLastError() };
        let _ = ack.send(Err(format!(
            "RegisterHotKey failed for {} chord={}; GetLastError=0x{error:08x}",
            kind.label(),
            chord.display
        )));
    }
}

fn unregister_mode_hotkey(kind: ModeHotkeyKind, state: &mut LoopState) {
    if state.mode_hotkeys[kind.slot()].take().is_some() {
        let ok = unsafe { UnregisterHotKey(std::ptr::null_mut(), kind.hotkey_id()) };
        if ok == 0 {
            let error = unsafe { GetLastError() };
            crate::diag::log!(
                "[hotkey/win_registerhotkey] {} UnregisterHotKey failed; GetLastError=0x{error:08x}",
                kind.label()
            );
        }
    }
}

fn cleanup(state: &mut LoopState) {
    unregister_current(state);
    unregister_copy_last(state);
    unregister_paste_last(state);
    state.pressed_trigger = None;
}

/// True when the specified virtual key is currently held down. Reads
/// the high bit of `GetAsyncKeyState`, which is set for any key
/// currently pressed at the physical (not window-focused) layer
/// exactly the signal we need for a chord release the RegisterHotKey
/// event stream never surfaces.
fn async_key_down(vk: u32) -> bool {
    // GetAsyncKeyState returns a signed short; the high bit (0x8000) is
    // the "currently down" flag.
    (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
}

// Unit tests live in the companion file `win_registerhotkey_tests.rs`
// so the regression-test discipline scanner sees a matching file next
// to the production module.
