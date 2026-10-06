//! [`InjectBackend`] impl that wraps the cross-platform [`Injector`]
//! dispatcher.
//!
//! Gated on the `rust-injection` cargo feature so default builds never
//! pull `enigo` into the dep graph. The wrapped [`Injector`] already
//! owns the platform-detection + helper-chain logic — see
//! `crate::injection::dispatcher` for the dispatch rules (enigo on
//! Windows / macOS / Linux-X11; helper chain on Linux/Wayland).
//!
//! In supported builds the coordinator sink constructs
//! `ProductionInjectBackend` (backed by `EnigoInjectBackend`) via
//! `make_real_session`; missing cargo features or construction failures
//! fall back to the stub injector.
//!
//! # Pre-injection cleanup
//!
//! Calling `Injector::inject_text` directly leaves two gaps. This
//! wrapper closes both inside `inject()` so a Rust `DictateSession`
//! can swap the stub backend for this one without a separate
//! caller-side dance:
//!
//! 1. **Clipboard ownership for paste mode.** The dispatcher's
//!    `Paste(_)` arm only sends the keystroke; its module doc explicitly
//!    states the caller is responsible for populating the clipboard
//!    first. A Rust-native session hands this wrapper the transcript
//!    itself, so without owning the copy/restore step we would silently
//!    paste whatever the user already had on their clipboard. The
//!    wrapper accepts an optional [`Clipboard`] impl via
//!    [`Self::with_clipboard`]; on `Paste(_)` it stashes the previous
//!    contents, writes the transcript, sends the chord, **waits for the
//!    paste-read window to elapse** (2.0 s — Wayland's `wl-copy` and
//!    slower GUI apps read the clipboard lazily so restoring instantly
//!    races the paste itself), then
//!    restores the previous value through a generation-tracked restore. The delay is
//!    parametrisable through [`Self::with_restore_delay`] so unit tests
//!    can pass [`Duration::ZERO`]. Paste-mode injection without a
//!    configured clipboard returns [`InjectError::Backend`] rather than
//!    silently pasting stale data — surfacing the misconfiguration
//!    loudly instead of mangling a transcript. The restore delay runs
//!    on a detached background thread so [`InjectBackend::inject`]
//!    returns as soon as the chord has been dispatched — without that
//!    split, every paste-mode utterance would block
//!    `DictateSession::stop_and_transcribe` (and therefore the next PTT)
//! for the full 2 s clipboard-restore window.
//! 2. **Stale push-to-talk modifiers.** A modifier PTT (Shift / Ctrl
//!    Alt / Cmd) is held physically THROUGH the dictation: when the
//!    inject burst lands, the OS still sees the modifier down, so a
//!    typing burst becomes `Ctrl+<char>` shortcuts and a paste chord
//!    gets warped. The wrapper calls
//!    [`Injector::release_held_modifiers`] before delegating, sweeping
//!    the full Shift / Alt / Ctrl / Cmd set.
//!
//! Both steps fail-soft (log + continue) when reasonable: a missing
//! modifier release is
//! strictly less bad than failing the inject entirely and dropping the
//! transcript. The clipboard write is the exception — there a failure
//! means we'd paste stale data, so we abort BEFORE sending the chord.
//!
//! # Interior mutability
//!
//! [`InjectBackend::inject`] takes `&self` (the session keeps an
//! immutable handle to the backend across utterances), but
//! [`Injector::inject_text`] takes `&mut self` because the lazy
//! enigo-construction stash needs to mutate the trait-object slot. The
//! optional [`Clipboard`] backend also needs `&mut self` (read / write
//! are mutating on every real impl). Bridging the two requires a
//! `Mutex` (or equivalent interior-mutability primitive); we use a
//! single `Mutex` over a small inner struct so a paste injection can
//! drive both the injector and the clipboard inside the same critical
//! section. Injection is coarse-grained and off-the-hot-path (it
//! follows transcription by definition; never contended in practice),
//! so the lock is cheap.
//!
//! Whether the wrapper itself is `Send` / `Sync` is driven by the
//! wrapped [`Injector`]'s auto-traits — `Injector` carries a
//! `Box<dyn InjectorBackend>` (no `Send` bound), so neither auto-derives
//! today. PR 5 / the supervisor can layer its own `Arc<Mutex<…>>` on
//! top if a session needs to cross threads.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex,
};
use std::time::Duration;

use crate::dictate::session::types::{InjectBackend, InjectError};
use crate::hotkey::{InjectionBracket, InjectionGuard};
use crate::injection::paste::{vk, Clipboard};
use crate::injection::{InjectMethod, Injector};

/// Pre-arm grace opened just before the injector calls `SendInput` for
/// the first time. Covers the microseconds between raising the
/// [`InjectionGuard`] bracket and the first LL-hook event landing (a
/// fast machine dispatches it in single-digit microseconds so we don't
/// want a race between "arm the counter" and "issue SendInput"). See
/// [`crate::hotkey::inject_guard`] for the full timing model.
const INJECT_PRE_GRACE: Duration = Duration::from_millis(50);

/// Post-arm grace armed AFTER the last `SendInput` returns.
/// `WH_KEYBOARD_LL` events reach rdev's callback via the installing
/// thread's message pump, which runs on a different thread than the
/// injector — the pump can trail `SendInput`'s return by tens to a
/// couple-hundred milliseconds under load. 200 ms sits comfortably
/// above every measurement I've seen on Windows 11 with the LL-hook
/// timeout at its default 300 ms, and small enough that a user who
/// genuinely re-presses PTT within a fifth of a second of the paste
/// chord notices at most one dropped press. With the bracket pattern
/// the horizon-only slack is less load-bearing, so 200 ms suffices.)
const INJECT_POST_GRACE: Duration = Duration::from_millis(200);

/// VKs the wrapper releases before every injection: the full
/// Shift / Alt / Ctrl / Cmd sweep.
///
/// Side-specific variants (`VK_R*`) are listed alongside the generic VKs
/// because Win32 distinguishes the two at the keyboard layer: a PTT
/// binding like `ctrl_r` or `shift_r+ctrl_r` leaves the right-side
/// scancode logically down and the generic `VK_CONTROL`/`VK_SHIFT`
/// release does NOT clear it.
///
/// Order is fixed and asserted on by the unit tests so an accidental
/// reorder is caught even when the resulting behaviour would be
/// identical at runtime.
pub(crate) const STALE_MODIFIER_VKS: &[u16] = &[
    vk::VK_SHIFT,
    vk::VK_RSHIFT,
    vk::VK_MENU,
    vk::VK_RMENU,
    vk::VK_CONTROL,
    vk::VK_RCONTROL,
    vk::VK_LWIN,
    vk::VK_RWIN,
];

/// How long to wait between sending the paste chord and restoring the
/// previous clipboard contents. Paste targets (especially Wayland's
/// `wl-copy` which
/// serves clipboard content at request time, and slower GUI apps) may
/// read the clipboard lazily / asynchronously — restoring instantly
/// races against the very paste we just triggered and the target ends up
/// with the user's previous clipboard contents instead of the dictated
/// text. inject.rs:266.
pub(crate) const DEFAULT_CLIPBOARD_RESTORE_DELAY: Duration = Duration::from_millis(2000);

/// Process-wide serialization for OS text injection. Every backend wraps
/// its own injector+clipboard in a private `Mutex`, so two distinct
/// backend instances (the dictation session's runtime backend, the UI's
/// shared backends, and a paste-last hotkey worker's per-press backend)
/// could otherwise type interleaved keystrokes into whatever window has
/// focus. The per-backend lock already serializes re-entrant use of ONE
/// instance; this pipeline lock closes the cross-instance gap: each
/// `inject_using` call AND the window activation that precedes it
/// ([`lock_pipeline`] callers) bracket as one unit, so concurrent
/// injection paths queue instead of interleaving and an activation can
/// never steal focus in the middle of another path's keystroke burst
/// .
///
/// The lock is REENTRANT: same-thread `lock_pipeline` calls nest so an
/// activation + injection pair on one thread brackets as a unit while
/// the `inject_using` inside still takes the lock. Nesting must unwind
/// in LIFO order — every call site holds the guard in a local binding,
/// so drop order follows scope order.
///
/// Holding the lock for a burst is off-the-hot-path (injection follows
/// transcription by definition) and cannot deadlock: restore timers and
/// explicit clipboard writes take only the per-backend restore lock,
/// never this one, and no inject path re-enters `inject_using` on a
/// different thread's stack.
struct PipelineState {
    /// `(owner thread, nesting depth)`; `None` when unlocked.
    owner: Mutex<Option<(std::thread::ThreadId, u64)>>,
    available: std::sync::Condvar,
}

static INJECTION_PIPELINE: PipelineState = PipelineState {
    owner: Mutex::new(None),
    available: Condvar::new(),
};

/// Take the process-wide injection pipeline lock. Reentrant on the
/// owning thread; other threads block until the owner unwinds fully.
pub(crate) fn lock_pipeline() -> PipelineGuard {
    let state = &INJECTION_PIPELINE;
    let me = std::thread::current().id();
    let mut owner = state
        .owner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    loop {
        let wait = match owner.as_ref().map(|(held, _)| *held) {
            None => {
                *owner = Some((me, 1));
                false
            }
            Some(held) if held == me => {
                if let Some((_, depth)) = owner.as_mut() {
                    *depth += 1;
                }
                false
            }
            Some(_) => true,
        };
        if !wait {
            break;
        }
        owner = state
            .available
            .wait(owner)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
    PipelineGuard {
        _not_send: std::marker::PhantomData,
    }
}

/// Held while a thread owns the pipeline lock. Deliberately `!Send` (the
/// raw-pointer phantom) so a guard can never migrate to another thread
/// and corrupt the owner accounting.
pub(crate) struct PipelineGuard {
    _not_send: std::marker::PhantomData<*mut u8>,
}

impl Drop for PipelineGuard {
    fn drop(&mut self) {
        let state = &INJECTION_PIPELINE;
        let mut owner = state
            .owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((_, depth)) = owner.as_mut() {
            *depth -= 1;
            if *depth == 0 {
                *owner = None;
                state.available.notify_all();
            }
        }
    }
}

/// Interior state guarded by a single `Mutex`. Keeping the injector and
/// the clipboard under the same lock lets a `Paste(_)` injection drive
/// both atomically: the clipboard write must happen between the
/// modifier release and the paste chord, with no interleaving from a
/// parallel `inject()` call on the same backend.
struct State {
    injector: Injector,
    /// Caller-supplied clipboard backend. Required for paste-mode
    /// injection; left `None` for typing-only wrappers (and for the
    /// tests that exercise the typing path). Boxed-dyn rather than a
    /// generic parameter so the wrapper can sit behind
    /// `Box<dyn InjectBackend>` in the dictate-session sink without
    /// leaking a phantom type parameter.
    ///
    /// Wrapped in `Arc<Mutex<…>>` (and bounded `+ Send`) so the
    /// detached restore thread spawned by `inject_via_paste` can hold
    /// its own handle — see [`spawn_clipboard_restore`]
    /// inject.rs:337. The outer `State` mutex serialises inject calls;
    /// the inner clipboard mutex separately serialises clipboard
    /// access between the inject thread (initial save+write) and the
    /// background restore thread (deferred restore). Both locks are
    /// held only for the duration of a single OS clipboard call so
    /// contention is negligible.
    clipboard: Option<Arc<Mutex<Box<dyn Clipboard + Send>>>>,
    restore: Arc<Mutex<RestoreState>>,
    /// Window to activate immediately before the next keystroke burst,
    /// INSIDE the pipeline lock. `ProductionInjectBackend::prepare_target`
    /// plants this instead of activating directly so focus changes can
    /// never interleave with another injection path's burst; paste-last
    /// plants the window captured at press
    /// time for the same reason. Consumed (taken) by the next
    /// `inject_using` on this backend.
    pending_window: Option<crate::platform::foreground_window::WindowInfo>,
}

#[derive(Default)]
pub(crate) struct RestoreState {
    generation: u64,
    original: Option<String>,
    injected: Option<String>,
    active: bool,
}

/// Production [`InjectBackend`] wrapping [`Injector`].
///
/// Construction is cheap — [`Injector::new`] does no system calls. The
/// underlying enigo backend (on Windows / macOS) is constructed lazily
/// inside [`Injector::inject_text`] on first use, or eagerly via
/// [`Injector::with_backend`] when a caller (e.g. a unit test) wants to
/// install a recording fake.
///
/// The chosen [`InjectMethod`] is fixed at construction (typing /
/// paste / explicit shortcut). For "no preference" the caller passes
/// [`InjectMethod::Paste(None)`] which lets the dispatcher pick the
/// platform-appropriate shortcut at dispatch time (incl. the Linux
/// terminal-aware heuristic).
///
/// **Paste mode requires a [`Clipboard`] backend.** Pair the wrapper
/// with [`Self::with_clipboard`] before handing it to any code path
/// that may call [`Self::inject`] under a paste method; otherwise the
/// paste arm surfaces [`InjectError::Backend`] rather than silently
/// sending the chord against stale clipboard data.
pub struct EnigoInjectBackend {
    /// `Mutex` over the combined injector + clipboard state so the
    /// trait can be implemented for `&self` even though both inner
    /// pieces need exclusive access during a single `inject` call.
    inner: Mutex<State>,
    method: InjectMethod,
    /// Delay between sending the paste chord and restoring the previous
    /// clipboard contents — see [`DEFAULT_CLIPBOARD_RESTORE_DELAY`] for
    /// the rationale (inject.rs:266). Tests override to
    /// `Duration::ZERO` via [`Self::with_restore_delay`] so they don't
    /// pay the 2 s wall-clock wait per paste assertion.
    restore_delay: Duration,
    /// Optional shared self-injection guard obtained from
    /// [`crate::hotkey::HotkeyHandle::injection_guard`]. When set (or
    /// when the process-wide `inject_guard::global` slot has been
    /// populated by `install_hotkey`), the wrapper brackets the guard
    /// around the SendInput bursts. The hotkey driver's callback then
    /// drops every OS key event it sees while the guard is active,
    /// preventing the app's own injected keystrokes from feeding back
    /// into the PTT tracker on Windows (the wedge that leaves PTT
    /// "works once, then dead"; the analogous Linux/Wayland feedback
    /// path is filtered at the device level).
    ///
    /// `None` when no hotkey subsystem is active (unit tests without
    /// the guard, headless CI, non-worker-rust binaries): the arm
    /// becomes a no-op and behaviour is unchanged.
    injection_guard: Option<Arc<InjectionGuard>>,
    /// Shared with the runtime supervisor so Stop can interrupt a long
    /// character-by-character typing burst between key events.
    cancellation: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for EnigoInjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Lock-free debug: avoid blocking on a `Mutex` we may not be
        // able to acquire (e.g. another inject in flight on a worker
        // thread when the supervisor's debug-print fires). Print the
        // outer fields only — the wrapped Injector has its own Debug
        // and the supervisor can interrogate via Self::method().
        f.debug_struct("EnigoInjectBackend")
            .field("method", &self.method)
            .field("restore_delay", &self.restore_delay)
            .field(
                "injection_guard",
                &self
                    .injection_guard
                    .as_ref()
                    .map(|_| "<Arc<InjectionGuard>>"),
            )
            .field(
                "cancellation",
                &self.cancellation.as_ref().map(|_| "<Arc<AtomicBool>>"),
            )
            .field("inner", &"<Mutex<State>>")
            .finish()
    }
}

impl EnigoInjectBackend {
    /// Build a backend around a pre-configured [`Injector`].
    ///
    /// The caller is expected to have set the target title / process
    /// xkb layout on the [`Injector`] via the builder methods before
    /// handing it over — those values do not change across utterances
    /// in a single session (the target is the focused window at the
    /// moment of dictation, which the supervisor refreshes between
    /// sessions).
    ///
    /// No clipboard is wired by default — pair with
    /// [`Self::with_clipboard`] for paste mode. Typing-only wrappers
    /// can skip the clipboard step.
    pub fn new(injector: Injector, method: InjectMethod) -> Self {
        Self {
            inner: Mutex::new(State {
                injector,
                clipboard: None,
                restore: Arc::new(Mutex::new(RestoreState::default())),
                pending_window: None,
            }),
            method,
            restore_delay: DEFAULT_CLIPBOARD_RESTORE_DELAY,
            injection_guard: None,
            cancellation: None,
        }
    }

    /// Connect the backend to the runtime lifecycle flag. A false value
    /// stops typing at the next character boundary.
    pub fn with_cancellation_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancellation = Some(flag);
        self
    }

    /// Install the shared self-injection guard obtained from
    /// [`crate::hotkey::HotkeyHandle::injection_guard`]. Once set, every
    /// `inject()` call brackets the guard around its `SendInput` bursts
    /// so the hotkey listener drops the app's own injected keystrokes
    /// instead of feeding them into the PTT tracker. Without this the
    /// tracker's bare-modifier rule 1 can trip on an injected foreign
    /// key and silently block the *next* PTT press for the full 10 s
    /// self-heal window — the exact wedge the user reports on Windows.
    /// See [`crate::hotkey::inject_guard`] for the full rationale.
    ///
    /// Optional: a wrapper built without a guard AND with no process-
    /// wide slot populated (unit tests, headless CI, binaries without a
    /// hotkey subsystem) behaves identically to the old code path — the
    /// arm becomes a no-op.
    pub fn with_injection_guard(mut self, guard: Arc<InjectionGuard>) -> Self {
        self.injection_guard = Some(guard);
        self
    }

    /// Install a [`Clipboard`] backend used by paste mode to save the
    /// previous contents, write the transcript, and restore the
    /// previous value after the paste chord fires.
    /// Required for [`InjectMethod::Paste`] — typing mode ignores it
    /// entirely so a typing-only wrapper can skip this step.
    pub fn with_clipboard(mut self, clipboard: Box<dyn Clipboard + Send>) -> Self {
        // `get_mut` skips the lock entirely; safe because `self` is owned
        // here so no other reference to the Mutex can exist.
        //
        // Wrap in `Arc<Mutex<…>>` at install time so the inject path can
        // share the handle with the detached restore thread without
        // re-allocating per call. The `+ Send` bound on the trait
        // object is what lets the inner `Mutex` (and therefore the
        // `Arc`) be `Sync` — required to cross the thread boundary
        // into the restore worker.
        self.inner
            .get_mut()
            .expect("freshly-constructed mutex is uncontended")
            .clipboard = Some(Arc::new(Mutex::new(clipboard)));
        self
    }

    /// Override the post-chord clipboard-restore delay. Production uses
    /// [`DEFAULT_CLIPBOARD_RESTORE_DELAY`] (2 s); tests pass
    /// [`Duration::ZERO`] to skip the wall-clock wait.
    ///
    /// Set on the wrapper rather than on `Injector` because the delay is
    /// a property of the wrapping clipboard-restore semantics (which only
    /// this layer owns) — `Injector::inject_text` doesn't know that a
    /// guard is in flight.
    pub fn with_restore_delay(mut self, delay: Duration) -> Self {
        self.restore_delay = delay;
        self
    }

    /// The chosen injection method (typing / paste-with-shortcut).
    /// Exposed for observability / debug.
    pub fn method(&self) -> InjectMethod {
        self.method
    }

    /// Plant a window to activate at the START of the next injection,
    /// inside the pipeline lock, so focus changes can never interleave
    /// with another injection path's keystroke burst. `None` clears a
    /// pending window (callers whose flow short-circuits before
    /// injecting use this to avoid activating a stale target later).
    pub fn set_pending_window(
        &self,
        window: Option<crate::platform::foreground_window::WindowInfo>,
    ) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending_window = window;
    }

    /// Update the target used by a later injection while retaining the
    /// backend's clipboard-restore coordinator.
    pub fn set_target(&self, title: &str, process: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.injector.set_target(title, process);
    }

    /// Update the Linux keyboard layout without replacing the shared backend.
    pub fn set_xkb_layout(&self, layout: &str) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.injector.set_xkb_layout(layout);
    }

    /// The shared clipboard-restore coordinator handle.
    pub(crate) fn restore_handle(&self) -> Arc<Mutex<RestoreState>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore
            .clone()
    }

    /// Adopt a shared restore coordinator in place. The backend is
    /// usually already shared via `Arc` when registration notices a
    /// pending restore cycle on a retained backend, so the coordinator
    /// is swapped through the inner lock instead of a builder
    /// .
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn adopt_restore_handle(&self, restore: Arc<Mutex<RestoreState>>) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore = restore;
    }

    /// Replace the backend's restore coordinator with a shared handle so
    /// overlapping paste cycles (session + paste-last) coordinate their
    /// original/generation bookkeeping instead of each restoring its own
    /// view of the clipboard .
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn with_restore_handle(mut self, restore: Arc<Mutex<RestoreState>>) -> Self {
        self.inner
            .get_mut()
            .expect("freshly-constructed mutex is uncontended")
            .restore = restore;
        self
    }

    /// Stop a pending restore from reclaiming the clipboard after the user
    /// explicitly copies a transcript from the UI.
    pub fn cancel_pending_restore(&self) {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut restore = state
            .restore
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        restore.generation = restore.generation.wrapping_add(1);
        restore.original = None;
        restore.injected = None;
        restore.active = false;
    }

    /// Keep a pending paste restore alive if an explicit clipboard write
    /// fails. Holding the restore lock also prevents its timer from racing a
    /// successful write before we retire the old restore generation.
    #[cfg(any(target_os = "windows", test))]
    pub(crate) fn with_restore_guard<T, E>(
        &self,
        write: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut restore = state
            .restore
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let result = write();
        if result.is_ok() {
            restore.generation = restore.generation.wrapping_add(1);
            restore.original = None;
            restore.injected = None;
            restore.active = false;
        }
        result
    }

    pub fn has_pending_restore(&self) -> bool {
        let state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active = state
            .restore
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active;
        active
    }

    /// The configured clipboard-restore delay. Exposed primarily for
    /// tests asserting the default; production callers should not need
    /// to read this back.
    pub fn restore_delay(&self) -> Duration {
        self.restore_delay
    }
}

impl EnigoInjectBackend {
    /// Same as [`InjectBackend::inject`] but uses `method` for this call
    /// instead of `self.method`. Split out so the profile-matcher path in
    /// [`crate::runtime::rust_session_inject::ProductionInjectBackend`]
    /// can hot-swap Typing / Paste per utterance without carrying a
    /// separate `EnigoInjectBackend` per mode.
    /// runtime/rust_session_inject.rs:146 -- a saved profile with
    /// `inject_mode=paste` previously landed on the constructor's
    /// `InjectMethod::Typing` because the wrapper had no way to override
    /// it, so paste silently degraded to typing.
    ///
    /// The pre-injection cleanup (stale-modifier release, self-injection
    /// guard bracket) is identical to the trait impl and shared here so
    /// both entry points behave the same.
    pub fn inject_using(&self, text: &str, method: InjectMethod) -> Result<(), InjectError> {
        // Recover from a poisoned mutex: a panic inside a previous
        // injection (e.g. enigo failing in an unexpected way) would
        // poison the lock. Neither the injector nor the clipboard
        // carry invariants violated by a panic — `enigo::Enigo` does
        // not retain incoherent state across calls and the
        // `Clipboard` trait is stateless apart from the backing OS
        // surface — so we recover the inner value and proceed rather
        // than wedging the session forever.
        let cancellation = self.cancellation.clone();
        let should_continue = || {
            cancellation
                .as_ref()
                .is_none_or(|flag| flag.load(Ordering::Acquire))
        };
        if !should_continue() {
            return Err(InjectError::Backend("injection cancelled".to_owned()));
        }
        // Serialize against every other backend instance (see the
        // `INJECTION_PIPELINE` docs): the session's runtime backend, the
        // UI's shared backends, and a paste-last worker's per-press
        // backend must never type interleaved keystrokes. Reentrant so a
        // same-thread activation bracket around this call nests cleanly.
        let _pipeline = lock_pipeline();
        // A thread that queued while another burst owned the pipeline can
        // sit behind it for seconds, and Stop/Restart may have flipped
        // the cancellation flag meanwhile. Recheck before any focus
        // change or keystroke so a stopped runtime never steals focus or
        // types into a replacement session's window.
        if !should_continue() {
            return Err(InjectError::Backend("injection cancelled".to_owned()));
        }
        let mut lock = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let state = &mut *lock;

        // Consume a pending target activation INSIDE the pipeline lock so
        // focus can never change mid-burst (see `set_pending_window` and
        // ).
        if let Some(window) = state.pending_window.take() {
            #[cfg(any(target_os = "windows", target_os = "linux"))]
            {
                crate::platform::window_enumeration::activate_window_with_id(
                    window.target_id.as_deref().unwrap_or_default(),
                    window.title.as_deref().unwrap_or_default(),
                    window.process.as_deref().unwrap_or_default(),
                )
                .map_err(InjectError::Backend)?;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            #[cfg(not(any(target_os = "windows", target_os = "linux")))]
            {
                let _ = &window;
            }
        }

        // Resolve the self-injection guard for THIS call: prefer an
        // explicitly-installed guard (test path via
        // `with_injection_guard`), fall back to the process-wide slot
        // that `install_hotkey` populates in production. The `Option`
        // stays `None` on binaries with no hotkey subsystem — the
        // bracket then becomes a no-op and behaviour is unchanged.
        //
        // `active_guard` is dropped at end-of-function; the `arm_end`
        // in [`InjectionBracket`] fires from `Drop` so an early
        // `?`/panic path inside the burst still closes the bracket
        // (and covers the LL-hook drain with the post-grace horizon).
        let active_guard = self
            .injection_guard
            .clone()
            .or_else(crate::hotkey::inject_guard::global);
        let _bracket = active_guard
            .as_deref()
            .map(|g| InjectionBracket::open(g, INJECT_PRE_GRACE, INJECT_POST_GRACE));

        // Pre-injection cleanup #1: drop any modifiers still held from
        // a push-to-talk chord. Failures are logged + ignored;
        // losing a release would land the burst as shortcuts but
        // failing the inject would lose the transcript outright.
        //
        // The bracket is already open here — the release-modifiers
        // SendInput calls are covered.
        if let Err(e) = state
            .injector
            .release_held_modifiers_cancellable(STALE_MODIFIER_VKS, &should_continue)
        {
            crate::diag::log!("[inject] stale-modifier release failed: {e:#}");
        }
        if !should_continue() {
            return Err(InjectError::Backend("injection cancelled".to_owned()));
        }

        match method {
            InjectMethod::Typing => state
                .injector
                .inject_text_cancellable(text, InjectMethod::Typing, &should_continue)
                .result
                .map_err(|e| InjectError::Backend(format!("{e:#}"))),
            InjectMethod::Paste(_) => {
                inject_via_paste(state, text, method, self.restore_delay, &should_continue)
            }
        }
        // `_bracket` drops here, extending the horizon by the post-arm
        // grace (covers WH_KEYBOARD_LL drain latency after the last
        // SendInput returns).
    }

    pub(crate) fn is_safe_auto_fallback(error: &InjectError) -> bool {
        matches!(
            error,
            InjectError::Backend(message)
                if message.starts_with("paste injection requires a clipboard backend")
                    || message.starts_with("clipboard write failed")
                    || message.contains("no Linux paste helper")
        )
    }
}

impl InjectBackend for EnigoInjectBackend {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        self.inject_using(text, self.method)
    }
}

/// Paste-mode injection arm: own the clipboard copy/restore even when
/// the caller is a Rust-native `DictateSession` (detached restore so
/// the inject thread is never blocked by the wall-clock wait).
///
/// Pulled into a free function so the borrow story stays obvious — the
/// caller destructures `state` once and the function owns the disjoint
/// `&mut Injector` + `Arc<Mutex<…>>` clipboard handle from there.
fn inject_via_paste(
    state: &mut State,
    text: &str,
    method: InjectMethod,
    restore_delay: Duration,
    should_continue: &dyn Fn() -> bool,
) -> Result<(), InjectError> {
    let clipboard = state.clipboard.as_ref().cloned().ok_or_else(|| {
        InjectError::Backend(
            "paste injection requires a clipboard backend; call \
             EnigoInjectBackend::with_clipboard before using Paste(_)"
                .to_owned(),
        )
    })?;
    let restore = state.restore.clone();

    // Save the previous clipboard + write the transcript. If the write
    // fails we abort BEFORE sending the chord — pasting whatever was
    // already on the clipboard would silently inject the wrong text.
    //
    // The inner clipboard mutex is released as soon as the save+write
    // finishes so the detached restore thread (or a concurrent
    // unrelated reader) is not blocked behind the injector call below.
    if !should_continue() {
        return Err(InjectError::Backend("injection cancelled".to_owned()));
    }
    let generation = {
        let mut restore_guard = restore
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut clip_guard = clipboard
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = clip_guard.read();
        let started_cycle = !restore_guard.active;
        if started_cycle {
            restore_guard.original = current;
            restore_guard.active = true;
        } else if current.is_some() && current.as_deref() != restore_guard.injected.as_deref() {
            // The user copied something after our previous paste. That newer
            // selection becomes the canonical backup for the extended cycle.
            restore_guard.original = current;
        }
        if !clip_guard.write(text) {
            if started_cycle {
                restore_guard.original = None;
                restore_guard.injected = None;
                restore_guard.active = false;
            }
            return Err(InjectError::Backend(
                "clipboard write failed; refusing to send paste shortcut \
                 against stale clipboard contents"
                    .to_owned(),
            ));
        }
        restore_guard.injected = Some(text.to_owned());
        restore_guard.generation = restore_guard.generation.wrapping_add(1);
        restore_guard.generation
    };

    let inject_result = if should_continue() {
        state
            .injector
            .inject_text_cancellable(text, method, should_continue)
            .result
            .map_err(|e| InjectError::Backend(format!("{e:#}")))
    } else {
        Err(InjectError::Backend("injection cancelled".to_owned()))
    };

    // Hand the paste-guard restore off to a detached daemon thread so
    // this function (and therefore `InjectBackend::inject`) returns as
    // soon as the chord has been dispatched. Without that split,
    // `DictateSession::stop_and_transcribe` would sit on the 2 s
    // clipboard-restore window before emitting `ProcessingFinished`,
    // which gates the next PTT — every paste-mode utterance would add
    // a fixed 2 s wait before the user could speak again. The restore
    // runs on a detached daemon thread.
    //
    // The restore runs irrespective of the inject result — the user's
    // prior clipboard contents are sacred, and the restore coordinator
    // gates the write on the clipboard still holding OUR text
    // so a mid-paste user copy is never clobbered.
    spawn_clipboard_restore(
        clipboard,
        restore,
        generation,
        text.to_owned(),
        restore_delay,
    );

    inject_result
}

/// Detached daemon-thread restore for the paste-mode clipboard coordinator.
///
/// Holds the cloned `Arc<Mutex<…>>` clipboard handle + the
/// generation plus injected text so the background thread can reject
/// stale overlapping restores. Best-effort: a spawn failure (essentially impossible
/// on production OSes) silently drops the restore — losing a restore
/// is strictly less bad than panicking the inject thread.
///
/// The thread is intentionally NOT joined: it's keyed to a fixed
/// timeout and the parent process owns its lifetime via the daemon
/// model. Tests synchronise on the
/// observable side effect (the restore write landing on the recording
/// clipboard) rather than on the thread handle.
fn spawn_clipboard_restore(
    clipboard: Arc<Mutex<Box<dyn Clipboard + Send>>>,
    restore: Arc<Mutex<RestoreState>>,
    generation: u64,
    injected: String,
    restore_delay: Duration,
) {
    let _ = std::thread::Builder::new()
        .name("inject-paste-restore".to_owned())
        .spawn(move || {
            if !restore_delay.is_zero() {
                std::thread::sleep(restore_delay);
            }
            let mut restore_guard = restore
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if restore_guard.generation != generation || !restore_guard.active {
                return;
            }
            let mut clip_guard = clipboard
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if clip_guard.read().as_deref() == Some(injected.as_str()) {
                if let Some(previous) = restore_guard.original.as_deref() {
                    if !clip_guard.write(previous) {
                        // Keep the canonical backup and active generation so
                        // a later paste cycle can retry restoration.
                        return;
                    }
                }
            }
            restore_guard.original = None;
            restore_guard.injected = None;
            restore_guard.active = false;
        });
}

// Test modules live in sibling files (the `inject.rs` parent is a
// file-form module, so without `#[path]` Rust would look for
// `dictate/backends/inject/<name>.rs` — keeping the tests next to
// their target file is more readable).
#[cfg(test)]
#[path = "inject_cleanup_tests.rs"]
mod inject_cleanup_tests;
#[cfg(test)]
#[path = "inject_restore_tests.rs"]
mod inject_restore_tests;
#[cfg(test)]
#[path = "inject_test_support.rs"]
mod inject_test_support;
#[cfg(test)]
#[path = "inject_tests.rs"]
mod inject_tests;
