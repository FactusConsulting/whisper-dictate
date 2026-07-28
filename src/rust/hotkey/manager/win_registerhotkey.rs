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
//! from rc.10 (PR #646) showed exactly this pattern in
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
    _w_param: WPARAM,
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

/// A user's PTT chord parsed into the (modifier mask, trigger VK) shape
/// `RegisterHotKey` accepts, plus the trigger's canonical name so we can
/// tee it into the diagnostic log line the caller reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedChord {
    pub mods: u32,
    pub vk: u32,
    pub trigger_name: String,
    /// Human-readable form (`"ctrl+shift+f9"`) for diagnostic lines. Not
    /// used by the register call itself.
    pub display: String,
}

/// Parse a `+`-separated list of PTT key names into the RegisterHotKey
/// shape. Accepts every name the rdev driver accepts (see
/// `rdev_driver::RDEV_SUPPORTED_NAMES`) plus the modifier-family aliases
/// (`ctrl` → MOD_CONTROL, `alt_gr` → MOD_ALT, ...). Rejects:
///
/// * **modifier-only chords** — RegisterHotKey requires exactly one
///   non-modifier trigger VK. This is the driver's single hard
///   limitation vs. the rdev backend and is surfaced with an
///   actionable error message so the caller can fall back or ask the
///   user to reconfigure.
/// * **multiple trigger keys** — only one VK per registration; a
///   chord like `f9+f10` is rejected explicitly.
/// * **unsupported trigger names** — anything outside the accepted
///   set (function keys, `space`, `esc`, `tab`, `enter`, and the
///   printable ASCII keys the OS maps 1:1 to a VK).
///
/// The name → VK table intentionally mirrors `rdev_driver::key_to_name`
/// so a chord that installs cleanly on rdev also installs cleanly on
/// this driver — modulo the modifier-only limitation. Names outside
/// that table produce an error that names the offending segment.
pub fn parse_chord(names: &[String]) -> Result<ParsedChord, String> {
    if names.is_empty() {
        return Err("chord is empty".to_owned());
    }
    let mut mods: u32 = 0;
    let mut trigger: Option<(u32, String)> = None;
    let mut segments: Vec<String> = Vec::with_capacity(names.len());
    for raw in names {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        segments.push(name.clone());
        // Side-specific modifier aliases (`ctrl_l`, `alt_r`, …) cannot be
        // honoured faithfully by RegisterHotKey: the OS `MOD_*` flags
        // trigger on EITHER side of the modifier family, so a
        // `ctrl_r+f9` binding registered as `MOD_CONTROL|VK_F9` would
        // also fire for `ctrl_l+f9` — the opposite of what the user
        // configured. Reject side-specific names at parse time so the
        // supervisor's install path falls back to rdev (which the
        // tracker does track per-side accurately). See Codex review of
        // PR #650 (discussion_r3663290089).
        if is_side_specific_modifier(&name) {
            return Err(format!(
                "chord key {name:?} names a side-specific modifier that \
                 the Windows RegisterHotKey driver cannot honour \
                 (MOD_* flags fire on either side of the modifier \
                 family). Use the generic name (`ctrl` / `shift` / \
                 `alt` / `win`) if either side is acceptable, or set \
                 VOICEPI_HOTKEY_DRIVER=rdev to keep the side-specific \
                 binding."
            ));
        }
        if let Some(bit) = modifier_bit(&name) {
            mods |= bit;
            continue;
        }
        let vk = vk_from_trigger_name(&name).ok_or_else(|| {
            format!(
                "chord key {name:?} is not supported by the Windows \
                 RegisterHotKey driver (accepted triggers: f1-f12, space, \
                 esc, tab, enter, and single ASCII letter/digit keys)"
            )
        })?;
        if trigger.is_some() {
            return Err(format!(
                "chord has more than one non-modifier key ({} and {name}); \
                 RegisterHotKey supports at most one trigger VK per chord",
                trigger.as_ref().unwrap().1
            ));
        }
        trigger = Some((vk, name));
    }
    let (vk, trigger_name) = trigger.ok_or_else(|| {
        "chord contains only modifiers; the Windows RegisterHotKey driver \
         requires at least one non-modifier trigger key (function key, \
         letter, digit, space, escape, tab, or enter). Set \
         VOICEPI_HOTKEY_DRIVER=rdev to keep a modifier-only binding, \
         or add a trigger key to the chord."
            .to_owned()
    })?;
    let display = segments.join("+");
    Ok(ParsedChord {
        mods,
        vk,
        trigger_name,
        display,
    })
}

/// Modifier name → `MOD_*` flag mapping. Accepts every alias the tracker
/// / rdev driver accepts so a chord that parses on either of those
/// parses here too (except for the modifier-only case rejected in
/// [`parse_chord`] and the side-specific-alias case which [`parse_chord`]
/// rejects up-front via [`is_side_specific_modifier`]).
fn modifier_bit(name: &str) -> Option<u32> {
    match name {
        "ctrl" | "ctrl_l" | "ctrl_r" => Some(MOD_CONTROL),
        "shift" | "shift_l" | "shift_r" => Some(MOD_SHIFT),
        // alt_gr / right_alt / ralt map to Alt too — the OS reports
        // AltGr as Ctrl+Alt, but the Windows MOD_ALT flag DOES fire on
        // the right-Alt key in the common European-layout setup.
        "alt" | "alt_l" | "alt_r" | "alt_gr" | "right_alt" | "ralt" => Some(MOD_ALT),
        "cmd" | "cmd_l" | "cmd_r" | "win" | "win_l" | "win_r" => Some(MOD_WIN),
        _ => None,
    }
}

/// True when `name` is a modifier alias that names a specific side
/// (left vs. right) of the key. `RegisterHotKey`'s `MOD_*` flags fire on
/// EITHER side, so a side-specific binding cannot be honoured faithfully:
/// registering `ctrl_r+f9` as `MOD_CONTROL|VK_F9` would also fire for
/// `ctrl_l+f9`. [`parse_chord`] uses this to reject the chord up-front so
/// the supervisor's install path falls back to rdev (which tracks
/// modifier sides accurately).
///
/// Includes `alt_gr` / `right_alt` / `ralt` because those all name the
/// right-Alt key specifically on European layouts; a user asking for
/// AltGr in their chord does NOT want left-Alt to also trigger it.
pub(crate) fn is_side_specific_modifier(name: &str) -> bool {
    matches!(
        name,
        "ctrl_l"
            | "ctrl_r"
            | "shift_l"
            | "shift_r"
            | "alt_l"
            | "alt_r"
            | "alt_gr"
            | "right_alt"
            | "ralt"
            | "cmd_l"
            | "cmd_r"
            | "win_l"
            | "win_r"
    )
}

/// Trigger name → Windows virtual-key code. Deliberately limited to the
/// keys the rdev driver already accepts as triggers, so smoke scripts
/// don't have to distinguish "rdev-only" from "register-only" chords.
/// Letters and digits are accepted via ASCII passthrough (the VK for
/// `A`..`Z` is the ASCII byte itself; same for `0`..`9`).
pub(crate) fn vk_from_trigger_name(name: &str) -> Option<u32> {
    let vk: u32 = match name {
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "space" => 0x20,
        "esc" => 0x1B,
        "tab" => 0x09,
        "enter" => 0x0D,
        "pause" => 0x13,
        other => {
            let bytes = other.as_bytes();
            if bytes.len() == 1 {
                let b = bytes[0];
                if b.is_ascii_alphabetic() {
                    b.to_ascii_uppercase() as u32
                } else if b.is_ascii_digit() {
                    b as u32
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    };
    Some(vk)
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

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    let loop_on_output = Arc::clone(&on_output);
    let join = thread::Builder::new()
        .name("vp-hotkey-win-rh".to_owned())
        .spawn(move || {
            let _ = ready_tx.send(Ok(()));
            crate::diag::log!(
                "[hotkey/win_registerhotkey] msg-loop thread started \
                 (bypasses WH_KEYBOARD_LL hook chain)"
            );
            run_msg_loop(cmd_rx, loop_on_output);
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

/// State the message loop mutates: currently-registered chord (used to
/// unregister on rebind / shutdown), and — while a chord is active —
/// the trigger VK so we can poll for its release.
pub(crate) struct LoopState {
    /// The chord currently registered with the OS, or `None` if
    /// `UnregisterHotKey` was the last action (or if we never
    /// registered).
    pub(crate) registered: Option<ParsedChord>,
    /// `Some(vk)` while a WM_HOTKEY press has been reported to the
    /// coordinator but the corresponding release has not yet fired.
    /// Poll `GetAsyncKeyState(vk)` between messages to detect release.
    pub(crate) pressed_trigger: Option<u32>,
}

impl LoopState {
    pub(crate) fn new() -> Self {
        Self {
            registered: None,
            pressed_trigger: None,
        }
    }
}

/// Pure edge classifier for the message loop's inputs, so the state
/// transitions can be unit-tested without spawning a real Windows
/// msg loop. Each variant matches one thing the loop reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopStimulus {
    /// A `WM_HOTKEY` message was drained from the queue.
    WmHotkey,
    /// The idle path polled `GetAsyncKeyState` and observed the trigger
    /// currently held (still "down").
    PollTriggerDown,
    /// The idle path polled `GetAsyncKeyState` and observed the trigger
    /// released (high bit clear).
    PollTriggerUp,
}

/// What the loop should emit to `on_output` after processing one
/// stimulus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoopEmit {
    None,
    Press,
    Release,
}

/// Apply one stimulus to the loop's state and return what to emit.
///
/// Extracted from the message-loop body so the press/release state
/// machine is unit-testable end-to-end — the exact regression the
/// rdev backend hit on Windows GUI (first press fires, subsequent
/// presses silently swallowed) has to be provably impossible on this
/// driver, and the check is a two-cycle assertion over this helper.
///
/// Contract:
/// * `WmHotkey` when the trigger is NOT already pressed → emit
///   `Press` and mark `pressed_trigger`. Requires a chord to be
///   registered (else the state was already inconsistent — return
///   None so we don't emit for an unregistered chord).
/// * `WmHotkey` while the trigger IS already pressed → treat as an
///   OS repeat (MOD_NOREPEAT is set, so this shouldn't happen, but
///   be defensive) and drop it.
/// * `PollTriggerUp` while a chord is pressed → clear
///   `pressed_trigger` and emit `Release`.
/// * `PollTriggerDown` while pressed → no-op (still held).
/// * Any poll while `pressed_trigger` is None → no-op (nothing to
///   release; polls only run when we've observed a press).
pub(crate) fn advance_state(state: &mut LoopState, stimulus: LoopStimulus) -> LoopEmit {
    match stimulus {
        LoopStimulus::WmHotkey => {
            let Some(chord) = state.registered.as_ref() else {
                return LoopEmit::None;
            };
            if state.pressed_trigger.is_some() {
                // Duplicate press despite MOD_NOREPEAT — should not
                // happen but keep the tracker clean if it does.
                return LoopEmit::None;
            }
            state.pressed_trigger = Some(chord.vk);
            LoopEmit::Press
        }
        LoopStimulus::PollTriggerUp => {
            if state.pressed_trigger.take().is_some() {
                LoopEmit::Release
            } else {
                LoopEmit::None
            }
        }
        LoopStimulus::PollTriggerDown => LoopEmit::None,
    }
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
                    emit_transition(
                        &mut state,
                        LoopStimulus::WmHotkey,
                        &on_output,
                        "WM_HOTKEY press",
                    );
                }
                continue; // loop back to drain more messages / commands
            }
        }

        // No message pending. If a chord is armed, poll its release
        // via GetAsyncKeyState (WM_HOTKEY does NOT fire on key-up).
        //
        // Codex P2 review of PR #650: the earlier revision polled ONLY
        // the trigger VK, so releasing a required modifier while still
        // holding the trigger (e.g. releasing Ctrl on `ctrl+f9` while
        // F9 is still down) never emitted the ChordRelease — recording
        // stayed active until F9 too was released, out-of-sync with
        // what the user perceived as chord end. Now the poll also asks
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

/// The outcome of pre-validating a `Register` command before any OS
/// state changes. Extracted from [`handle_command`] so the "validate
/// BEFORE unregister" contract (Codex P1 review of PR #650 — see
/// `discussion_r3663290080`) has a direct unit test.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RegisterPlan {
    /// Parse succeeded. Caller should unregister the current chord,
    /// then install `chord` via `RegisterHotKey`.
    Install(ParsedChord),
    /// Parse failed. Caller MUST leave the previous binding intact
    /// (no `UnregisterHotKey`) and ack the error back — otherwise a
    /// rebind attempt with a bad chord leaves the process without any
    /// listener at all.
    Reject(String),
}

/// Pure planner: parse the target chord and translate the result into
/// the plan `handle_command` executes. No OS calls; no state mutation.
pub(crate) fn plan_register(targets: &[String]) -> RegisterPlan {
    match parse_chord(targets) {
        Ok(chord) => RegisterPlan::Install(chord),
        Err(msg) => RegisterPlan::Reject(msg),
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
            // Codex P1 review of PR #650: on a resume-with-new-chord
            // path the original ordering was "unregister → parse". If
            // parse failed (side-specific modifier, unsupported trigger,
            // etc.) the previously-working chord was already torn down
            // AND the new one never installed, leaving the process
            // without a listener while the supervisor's `resume` path
            // only logs the error. Parsing first, then unregistering,
            // preserves the working binding when the caller sends a
            // bad new chord — the supervisor can then keep Python or
            // recreate with rdev.
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
        ManagerCommand::Unregister { ack } => {
            unregister_current(state);
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

fn cleanup(state: &mut LoopState) {
    unregister_current(state);
    state.pressed_trigger = None;
}

/// True when the specified virtual key is currently held down. Reads
/// the high bit of `GetAsyncKeyState`, which is set for any key
/// currently pressed at the physical (not window-focused) layer —
/// exactly the signal we need for a chord release the RegisterHotKey
/// event stream never surfaces.
fn async_key_down(vk: u32) -> bool {
    // GetAsyncKeyState returns a signed short; the high bit (0x8000) is
    // the "currently down" flag.
    (unsafe { GetAsyncKeyState(vk as i32) } as u16) & 0x8000 != 0
}

// Windows virtual-key codes for the generic modifier keys (either side).
// Exported at module scope so the smoke test / release-polling helper can
// share the values with the parse layer.
const VK_SHIFT: u32 = 0x10;
const VK_CONTROL: u32 = 0x11;
const VK_MENU: u32 = 0x12; // Alt
const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;

/// Collect the VKs that must be down for the chord's `MOD_*` mask to be
/// considered "still held". `MOD_WIN` has no unified VK so BOTH LWIN and
/// RWIN must be inspected; if either is down, the Win-family requirement
/// is satisfied — hence the return shape is a list of "any-of" groups.
///
/// Extracted as a pure helper so the modifier-release logic is unit-
/// testable without a real Windows message loop. See Codex P2 review of
/// PR #650 (discussion_r3663290087).
pub(crate) fn required_modifier_vk_groups(mods: u32) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    if mods & MOD_CONTROL != 0 {
        groups.push(vec![VK_CONTROL]);
    }
    if mods & MOD_SHIFT != 0 {
        groups.push(vec![VK_SHIFT]);
    }
    if mods & MOD_ALT != 0 {
        groups.push(vec![VK_MENU]);
    }
    if mods & MOD_WIN != 0 {
        // Win-family: no unified VK, poll BOTH sides.
        groups.push(vec![VK_LWIN, VK_RWIN]);
    }
    groups
}

/// True iff every modifier family named in `mods` has at least one of its
/// VKs currently held. Consulted from the release-polling path so a
/// mid-hold modifier release fires the chord release even while the
/// trigger key is still down.
fn required_modifiers_down(mods: u32) -> bool {
    required_modifier_vk_groups(mods)
        .into_iter()
        .all(|group| group.into_iter().any(async_key_down))
}

// Unit tests live in the companion file `win_registerhotkey_tests.rs`
// so the regression-test discipline scanner sees a matching file next
// to the production module.
