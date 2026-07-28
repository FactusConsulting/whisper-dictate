//! Parallel `WH_KEYBOARD_LL` diagnostic hook (Windows only).
//!
//! Sole purpose: answer the question "did F9 physically reach our
//! process at all?" for the ongoing v1.22.0-rc.10 Windows PTT
//! investigation where rdev's own LL-hook callback never sees F9,
//! ctrl_l, or Pause events even though letters, space and arrows come
//! through fine.
//!
//! # What this module is (and isn't)
//!
//! It installs a **SECOND** `WH_KEYBOARD_LL` low-level keyboard hook,
//! independent of rdev's, on its own dedicated thread with its own
//! message pump. Every event delivered to the callback is logged with
//! its WM message type (KEYDOWN / KEYUP / SYSKEYDOWN / SYSKEYUP),
//! virtual-key code, scan code, flags, injected-flag, and
//! extended-flag. The callback ALWAYS returns `CallNextHookEx`
//! immediately — this hook never consumes events, it just observes.
//!
//! It is NOT a replacement for rdev. It is NOT a fallback listener. It
//! IS a targeted diagnostic that runs alongside rdev so we can
//! triangulate: if this hook sees F9 but rdev's callback does not,
//! rdev is dropping the event internally; if neither sees F9, some
//! higher-priority hook in the chain (an antivirus with keylogger
//! prevention, Logi G HUB, NVIDIA Broadcast, ...) is consuming F9
//! before either hook is called; if this hook doesn't install at all,
//! the diagnostic itself is broken. The three cases produce distinct
//! evidence in the tee'd `gui-diagnostic.log`.
//!
//! # Rate limiting
//!
//! The LL hook fires for **every desktop-wide key press and release**
//! — an interactive session generates ~5–20 events per second and a
//! sustained-typing burst can hit >100 events/second. Logging every
//! one would flood the tee file (which is append-mode across
//! sessions) and skew the very timing we are trying to measure. The
//! same discipline the pre-existing rdev per-event trace uses
//! ([`crate::hotkey::manager::rdev_driver::should_log_raw_event`],
//! constants scaled up here for the higher raw-event volume) applies:
//! the first [`RAW_HOOK_INITIAL_TRACE`] events always log, then every
//! [`RAW_HOOK_TRACE_EVERY`]th event thereafter. Rate-limit decisions
//! are made in the pure [`should_log_raw_hook_event`] helper so the
//! entire policy is unit-testable without going near the Windows API.
//!
//! # Gate
//!
//! The install is opt-in behind `VOICEPI_LOG=trace` — see
//! [`crate::diag::trace_enabled`]. When the gate is off, [`install`]
//! is a no-op. When the gate is on, the hook thread is spawned once
//! at supervisor start and runs for the process lifetime (LL hooks
//! cannot be safely uninstalled without ending the message-pump
//! thread; the diagnostic is a process-lifetime resource by
//! construction).

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::thread;

/// Number of leading raw events that always emit a trace line. Set
/// higher than the rdev boundary trace's initial burst
/// ([`crate::hotkey::manager::rdev_driver::RAW_EVENT_INITIAL_TRACE`]
/// via its `pub(crate)` helper) because a fresh install landing in
/// the middle of an active session should still show the first
/// hundred-or-so key events so the user's F9 press attempt in the
/// first few seconds after install is guaranteed to log.
pub(crate) const RAW_HOOK_INITIAL_TRACE: u64 = 200;

/// After the initial burst, only every N-th event logs. Bigger than
/// the rdev boundary trace's `RAW_EVENT_TRACE_EVERY` (100) because
/// this hook fires on every desktop-wide keydown/keyup, not just the
/// ones that reach rdev's own callback path. 50 → one line per short
/// typing burst in steady-state use.
pub(crate) const RAW_HOOK_TRACE_EVERY: u64 = 50;

/// Rate-limit decision for the raw-hook per-event trace. Pure helper
/// so the policy is unit-testable without spawning the hook thread.
/// Returns `true` for events `1..=RAW_HOOK_INITIAL_TRACE`, then for
/// every [`RAW_HOOK_TRACE_EVERY`]-th event thereafter. Zero always
/// returns `false` — the counter is 1-indexed (a stray 0 would
/// indicate a caller bug).
pub(crate) fn should_log_raw_hook_event(n: u64) -> bool {
    if n == 0 {
        return false;
    }
    if n <= RAW_HOOK_INITIAL_TRACE {
        return true;
    }
    n.is_multiple_of(RAW_HOOK_TRACE_EVERY)
}

/// Translate a `wParam` from the LL-hook callback into the stable
/// short string used in the trace line's `wm=` field. Pure helper so
/// the mapping stays consistent across install sites and is
/// unit-testable without touching the Windows API.
///
/// The four WM values a `WH_KEYBOARD_LL` hook can deliver are
/// `WM_KEYDOWN` (0x0100), `WM_KEYUP` (0x0101), `WM_SYSKEYDOWN`
/// (0x0104), `WM_SYSKEYUP` (0x0105). Anything else is unexpected and
/// returned as `"WM_UNKNOWN(0x…)"` so a future Windows change that
/// starts delivering a new message type is inspectable rather than
/// silently classified as a keydown.
pub(crate) fn wm_message_name(wparam: usize) -> String {
    match wparam as u32 {
        0x0100 => "WM_KEYDOWN".to_owned(),
        0x0101 => "WM_KEYUP".to_owned(),
        0x0104 => "WM_SYSKEYDOWN".to_owned(),
        0x0105 => "WM_SYSKEYUP".to_owned(),
        other => format!("WM_UNKNOWN(0x{other:04x})"),
    }
}

// ---------------------------------------------------------------------
// Windows-side wiring (only compiles under target_os = "windows").
// ---------------------------------------------------------------------

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
};

/// Per-hook running total of raw events seen by the callback. Used by
/// the rate limiter in [`should_log_raw_hook_event`]. `AtomicU64`
/// because the callback is called from the OS's LL-hook thread — the
/// counter must be lock-free.
static RAW_HOOK_EVENT_COUNT: AtomicU64 = AtomicU64::new(0);

/// One-shot install latch. LL hooks are a per-thread resource and the
/// diagnostic hook runs for the process lifetime, so a second install
/// call from a rogue path is a bug — this latch turns the second call
/// into a silent no-op so tests / duplicate call sites don't flood the
/// process with hook threads.
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Preserved for the `install()` accessor so the log line reports
/// which hook thread id owns the pump. Set once at install time.
static HOOK_THREAD_INSTALLED: OnceLock<bool> = OnceLock::new();

/// LL-hook callback. Called from the OS on the LL-hook thread every
/// time a keyboard event is delivered to this process's hook chain.
/// This callback:
///
/// 1. Increments the per-hook event counter.
/// 2. Consults the rate limiter and, when the event index qualifies,
///    formats and emits a single grep-friendly trace line via
///    [`crate::diag::log!`].
/// 3. Unconditionally forwards to `CallNextHookEx` — this hook
///    NEVER consumes events, so no interaction with other hooks in
///    the chain (rdev's own LL hook included) is affected.
///
/// The `trace_enabled()` check inside the emit branch is redundant
/// with the install-time gate — kept anyway so that a future
/// mid-session level-flip is honoured immediately without needing to
/// tear the hook thread down.
unsafe extern "system" fn ll_keyboard_hook_proc(
    n_code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Per the Win32 docs: if nCode < 0, the hook procedure must pass
    // the message to CallNextHookEx without further processing and
    // should return the value returned by CallNextHookEx. Values >= 0
    // are the ones we can inspect.
    if n_code < 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), n_code, wparam, lparam) };
    }
    let n = RAW_HOOK_EVENT_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if crate::diag::trace_enabled() && should_log_raw_hook_event(n) {
        // The docs guarantee lparam is a valid KBDLLHOOKSTRUCT
        // pointer when nCode == HC_ACTION (0). Any other nCode value
        // is not defined for WH_KEYBOARD_LL today; we still deref
        // (nCode >= 0 filter above) but bail on null to be safe
        // against a future Windows change.
        let kb_ptr = lparam as *const KBDLLHOOKSTRUCT;
        if !kb_ptr.is_null() {
            let kb = unsafe { *kb_ptr };
            let wm = wm_message_name(wparam as usize);
            // Extended-key bit (bit 0) and injected bit (bit 4) —
            // per KBDLLHOOKSTRUCT docs. The injected bit is the same
            // one InjectionGuard reads on the rdev callback side, so
            // if a "keydown vk=0x78 injected=true" line appears here
            // it's a stray SendInput from within our own process (a
            // regression in the injection guard).
            let extended = (kb.flags & 0x01) != 0;
            let injected = (kb.flags & 0x10) != 0;
            crate::diag::log!(
                "[win/raw-hook] #{n} wm={wm} vk=0x{vk:02x} scan=0x{scan:02x} \
                 flags=0x{flags:04x} injected={injected} extended={extended}",
                vk = kb.vkCode,
                scan = kb.scanCode,
                flags = kb.flags,
            );
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), n_code, wparam, lparam) }
}

/// Install the parallel LL-hook diagnostic thread once. Idempotent —
/// second and subsequent calls are silent no-ops (see [`INSTALLED`]).
/// No-op when [`crate::diag::trace_enabled`] returns false — the
/// caller (`whisper-dictate-gui::main`) does not need to gate at
/// the call site because this function does it internally. That
/// keeps the call site simple: "just call it every time; it decides
/// whether to run".
///
/// Returns `true` when this call actually installed the hook, `false`
/// when it was a no-op (either already installed, or the deep gate is
/// off). The boolean is used by the caller for the one-line
/// "installed / skipped" diagnostic marker so operators can see in
/// the log whether the hook is live.
pub fn install() -> bool {
    if !crate::diag::trace_enabled() {
        return false;
    }
    if INSTALLED.swap(true, Ordering::AcqRel) {
        // Someone already installed. Fine — the LL hook is
        // process-lifetime; a second install would leak another
        // pump thread for no incremental signal.
        return false;
    }
    // Spawn a dedicated thread that installs the hook AND runs a
    // message pump. `WH_KEYBOARD_LL` (unlike WH_KEYBOARD) is a global
    // hook that does NOT require the callback DLL to be injected
    // into each target process — the OS calls back on the SAME
    // thread that installed the hook, and that thread MUST have a
    // GetMessageW-style pump running for the callback to fire.
    // Without the pump the OS silently drops every event because
    // there is nobody to dispatch to.
    let spawn_result = thread::Builder::new()
        .name("vp-hotkey-win-raw-hook".to_owned())
        .spawn(move || {
            // SAFETY: SetWindowsHookExW with WH_KEYBOARD_LL is safe
            // to call from any thread; the callback pointer is a
            // valid extern "system" fn with the right ABI; hMod is
            // NULL and dwThreadId is 0, which is the documented way
            // to install a global LL hook.
            let hook = unsafe {
                SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(ll_keyboard_hook_proc),
                    std::ptr::null_mut(),
                    0,
                )
            };
            if hook.is_null() {
                crate::diag::log!(
                    "[win/raw-hook] SetWindowsHookExW returned NULL - diagnostic hook \
                     not installed. The rdev hook may still be working; this is a \
                     diagnostic-only failure."
                );
                return;
            }
            crate::diag::log!(
                "[win/raw-hook] parallel WH_KEYBOARD_LL diagnostic hook installed \
                 on dedicated pump thread"
            );
            // Message pump. GetMessageW blocks until a message
            // arrives or the thread is signalled. LL-hook callbacks
            // are delivered internally by the OS while GetMessageW
            // waits — the returned messages themselves are not
            // interesting to us, but the pump MUST be running for
            // the callback to fire at all. See MSDN "Using Hooks →
            // Registering a Hook Procedure":
            //   "For hook types that monitor the entire system, the
            //    hook procedure must be in a DLL." — except
            //    WH_KEYBOARD_LL and WH_MOUSE_LL, which are the
            //    documented exception.
            //   "You must have a message loop for the low-level
            //    keyboard hooks."
            let mut msg: MSG = unsafe { std::mem::zeroed() };
            loop {
                let ret = unsafe {
                    GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0)
                };
                if ret <= 0 {
                    // 0 = WM_QUIT, -1 = error. Either way, exit the
                    // pump. In production this branch is not
                    // reached — the thread runs for the process
                    // lifetime.
                    break;
                }
                // We deliberately do NOT DispatchMessageW here —
                // there's no window on this thread and we don't
                // need the messages themselves. The LL-hook
                // callback has already fired by the time
                // GetMessageW returns.
            }
        });
    match spawn_result {
        Ok(_) => {
            let _ = HOOK_THREAD_INSTALLED.set(true);
            true
        }
        Err(err) => {
            crate::diag::log!(
                "[win/raw-hook] failed to spawn diagnostic pump thread: {err}"
            );
            // Clear the installed latch so a follow-up install call
            // (unlikely in production, but possible in tests) can
            // retry. Ordering::Release pairs with the AcqRel swap
            // above so the retry sees a fresh state.
            INSTALLED.store(false, Ordering::Release);
            false
        }
    }
}

/// Whether the diagnostic hook is currently installed on a live
/// pump thread. Test-only accessor so a unit test that exercised
/// [`install`] can assert against the latch without duplicating the
/// atomic-load pattern.
#[cfg(test)]
pub(crate) fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}
