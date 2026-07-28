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
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::Duration;

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

/// True when `vk` is one of the virtual-key codes that the ongoing
/// PTT investigation cares about — F1..F12, Pause, and every side of
/// Ctrl / Shift / Alt / Meta.
///
/// Rationale (Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPg): the
/// pure [`should_log_raw_hook_event`] sampling drops every event
/// after the initial burst except multiples of
/// [`RAW_HOOK_TRACE_EVERY`]. In a session with even a modest amount
/// of typing, an F9 or Pause press delivered as event 201..249 will
/// therefore leave NO `[win/raw-hook]` line, and the documented
/// decision tree misclassifies the missing line as an upstream hook
/// consuming F9. Investigated-key events must always log regardless
/// of the sampling gate — the ordinary-typing suppression still
/// applies to non-PTT-eligible keys, so the log volume stays bounded.
///
/// Keeping the predicate pure (no OS calls, no state) is what lets
/// the companion `win_raw_hook_tests.rs` pin it without spawning any
/// hook / pump thread.
pub(crate) fn is_investigated_vk(vk: u32) -> bool {
    match vk {
        // F1..F12 — VK_F1 = 0x70 through VK_F12 = 0x7B.
        0x70..=0x7B => true,
        // VK_PAUSE — reported as a PTT drop in the same investigation
        // (the "F-key + Pause + Ctrl" family the raw-hook trace was
        // built for).
        0x13 => true,
        // Ctrl family: VK_CONTROL (generic) = 0x11, VK_LCONTROL = 0xA2,
        // VK_RCONTROL = 0xA3.
        0x11 | 0xA2 | 0xA3 => true,
        // Shift family: VK_SHIFT = 0x10, VK_LSHIFT = 0xA0, VK_RSHIFT = 0xA1.
        0x10 | 0xA0 | 0xA1 => true,
        // Alt family: VK_MENU = 0x12, VK_LMENU = 0xA4, VK_RMENU = 0xA5.
        0x12 | 0xA4 | 0xA5 => true,
        // Meta family: VK_LWIN = 0x5B, VK_RWIN = 0x5C.
        0x5B | 0x5C => true,
        _ => false,
    }
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
    if crate::diag::trace_enabled() {
        // The docs guarantee lparam is a valid KBDLLHOOKSTRUCT
        // pointer when nCode == HC_ACTION (0). Any other nCode value
        // is not defined for WH_KEYBOARD_LL today; we still deref
        // (nCode >= 0 filter above) but bail on null to be safe
        // against a future Windows change. The deref moves ABOVE the
        // sampling decision so an investigated key (F1..F12 / Pause /
        // Ctrl-Shift-Alt-Meta variants) always logs even when the
        // rate-limit would otherwise drop it — Codex P2 #651
        // discussion PRRT_kwDOSfNjQs6UTvPg. Ordinary desktop typing
        // still falls under the sampling gate so the tee file stays
        // bounded on long sessions.
        let kb_ptr = lparam as *const KBDLLHOOKSTRUCT;
        if !kb_ptr.is_null() {
            let kb = unsafe { *kb_ptr };
            let investigated = is_investigated_vk(kb.vkCode);
            if investigated || should_log_raw_hook_event(n) {
                let wm = wm_message_name(wparam);
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
                     flags=0x{flags:04x} injected={injected} extended={extended} \
                     investigated={investigated}",
                    vk = kb.vkCode,
                    scan = kb.scanCode,
                    flags = kb.flags,
                );
            }
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), n_code, wparam, lparam) }
}

/// Bounded window [`install_with_installer`] blocks on for the pump
/// thread to signal success / failure from inside itself. `SetWindowsHookExW`
/// is a synchronous Win32 call and returns essentially immediately in
/// healthy sessions; 500 ms is more than an order of magnitude of margin
/// for a loaded box. If the pump signals nothing in that window we treat
/// the install as a bounded-time failure so the caller's boolean
/// contract stays honest.
const INSTALL_READY_TIMEOUT: Duration = Duration::from_millis(500);

/// Abstraction over the Windows `SetWindowsHookExW` call so
/// [`install_with_installer`] is testable without invoking the real Win32
/// API from a unit test. Production uses [`Win32HookInstaller`] which
/// forwards to `SetWindowsHookExW`; tests use a synthetic implementation
/// (e.g. `FailingHookInstaller`) that reports failure without touching
/// the OS.
///
/// The trait is owned (`self`) rather than shared, mirroring the one-shot
/// nature of the call — the pump thread consumes the installer, calls it,
/// and never uses it again.
pub(crate) trait HookInstaller: Send + 'static {
    /// Install the LL hook and return `true` on success, `false` on
    /// failure (the production impl treats a null `HHOOK` return as
    /// failure). No blocking work — a healthy `SetWindowsHookExW`
    /// returns in microseconds.
    fn install(self) -> bool;
}

/// Production installer that calls the real `SetWindowsHookExW` and
/// treats a null return as failure. Never allocates.
pub(crate) struct Win32HookInstaller;

impl HookInstaller for Win32HookInstaller {
    fn install(self) -> bool {
        // SAFETY: SetWindowsHookExW with WH_KEYBOARD_LL is safe to
        // call from any thread; the callback pointer is a valid extern
        // "system" fn with the right ABI; hMod is NULL and dwThreadId
        // is 0, which is the documented way to install a global LL
        // hook.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                Some(ll_keyboard_hook_proc),
                std::ptr::null_mut(),
                0,
            )
        };
        !hook.is_null()
    }
}

/// Install the parallel LL-hook diagnostic thread once. Idempotent —
/// second and subsequent calls are silent no-ops (see [`INSTALLED`]).
/// No-op when [`crate::diag::trace_enabled`] returns false — the
/// caller (`whisper-dictate-gui::main`) does not need to gate at
/// the call site because this function does it internally. That
/// keeps the call site simple: "just call it every time; it decides
/// whether to run".
///
/// Returns `true` when this call actually installed the hook AND the
/// pump thread's `SetWindowsHookExW` returned non-null within
/// [`INSTALL_READY_TIMEOUT`], `false` in every other case (already
/// installed, gate off, spawn failure, hook API failure, timeout).
/// The boolean is used by the caller for the one-line "installed /
/// skipped" diagnostic marker so operators can see in the log whether
/// the hook is live. Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPp:
/// previous versions returned `true` as soon as the pump thread
/// spawned, so a `SetWindowsHookExW` failure inside that thread left
/// the caller announcing a live hook that never was.
pub fn install() -> bool {
    install_with_installer(Win32HookInstaller)
}

/// Testable core of [`install`] — takes an owned [`HookInstaller`] so
/// the unit tests can simulate a hook-API failure without invoking any
/// Win32 code. Every other side effect (the `INSTALLED` latch, the
/// diagnostic log lines, the sync channel semantics) is shared with
/// the production path so this is a real regression seat, not a
/// parallel implementation.
pub(crate) fn install_with_installer<I: HookInstaller>(installer: I) -> bool {
    if !crate::diag::trace_enabled() {
        return false;
    }
    if INSTALLED.swap(true, Ordering::AcqRel) {
        // Someone already installed. Fine — the LL hook is
        // process-lifetime; a second install would leak another
        // pump thread for no incremental signal.
        return false;
    }
    // Sync channel with capacity 1 — the pump thread sends exactly one
    // signal (Started on success, Failed on hook-API failure) then
    // stops touching the channel. mpsc::sync_channel is Send so the
    // sender clone crosses the thread boundary cleanly.
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let spawn_result = thread::Builder::new()
        .name("vp-hotkey-win-raw-hook".to_owned())
        .spawn(move || {
            if !installer.install() {
                crate::diag::log!(
                    "[win/raw-hook] SetWindowsHookExW returned NULL - diagnostic hook \
                     not installed. The rdev hook may still be working; this is a \
                     diagnostic-only failure."
                );
                let _ = ready_tx.send(Err("SetWindowsHookExW returned NULL".to_owned()));
                return;
            }
            // Only after the OS confirms the hook is live do we
            // signal Started — that's the contract the caller's
            // boolean promises. Emit the diagnostic marker on the
            // same branch so the tee file records the exact moment
            // the hook became active.
            crate::diag::log!(
                "[win/raw-hook] parallel WH_KEYBOARD_LL diagnostic hook installed \
                 on dedicated pump thread"
            );
            let _ = ready_tx.send(Ok(()));
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
                let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
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
    if let Err(err) = spawn_result {
        crate::diag::log!("[win/raw-hook] failed to spawn diagnostic pump thread: {err}");
        // Clear the installed latch so a follow-up install call
        // (unlikely in production, but possible in tests) can
        // retry. Ordering::Release pairs with the AcqRel swap
        // above so the retry sees a fresh state.
        INSTALLED.store(false, Ordering::Release);
        return false;
    }
    // Block up to INSTALL_READY_TIMEOUT for the pump thread to signal.
    // On success we latch HOOK_THREAD_INSTALLED and return true; on
    // any other outcome (Failed signal from the pump thread, timeout,
    // or a disconnected channel) we RELEASE the INSTALLED latch so a
    // caller retrying after a transient failure can actually retry.
    // Codex P2 #651 discussion PRRT_kwDOSfNjQs6UTvPp.
    match ready_rx.recv_timeout(INSTALL_READY_TIMEOUT) {
        Ok(Ok(())) => {
            let _ = HOOK_THREAD_INSTALLED.set(true);
            true
        }
        Ok(Err(_)) | Err(_) => {
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
