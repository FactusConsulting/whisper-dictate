//! state policy for the Windows RegisterHotKey driver.

use super::ParsedChord;

/// State the message loop mutates: currently-registered chord (used to
/// unregister on rebind / shutdown), and — while a chord is active
/// the trigger VK so we can poll for its release.
pub(crate) struct LoopState {
    /// The chord currently registered with the OS, or `None` if
    /// `UnregisterHotKey` was the last action (or if we never
    /// registered).
    pub(crate) registered: Option<ParsedChord>,
    /// Optional one-shot action registered on the same owner thread.
    pub(crate) copy_last_registered: Option<ParsedChord>,
    /// Optional one-shot action registered on the same owner thread.
    pub(crate) paste_last_registered: Option<ParsedChord>,
    /// Optional one-shot mode shortcuts registered on the same owner
    /// thread, indexed by [`ModeHotkeyKind`] slot (cycle, raw, clean).
    pub(crate) mode_hotkeys: [Option<ParsedChord>; 3],
    /// `Some(vk)` while a WM_HOTKEY press has been reported to the
    /// coordinator but the corresponding release has not yet fired.
    /// Poll `GetAsyncKeyState(vk)` between messages to detect release.
    pub(crate) pressed_trigger: Option<u32>,
}

impl LoopState {
    pub(crate) fn new() -> Self {
        Self {
            registered: None,
            copy_last_registered: None,
            paste_last_registered: None,
            mode_hotkeys: [None, None, None],
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
