//! Production [`InjectBackend`] for the rust-session sink that honors
//! `VOICEPI_INJECT_MODE=print` (Codex P2 #423 finding 4).
//!
//! Why this exists on top of [`crate::dictate::backends::EnigoInjectBackend`]:
//!
//! * The stale-modifier release pre-step (finding 3) is already owned
//!   by [`EnigoInjectBackend::inject`] itself -- it loops over the
//!   side-specific `STALE_MODIFIER_VKS` set before every typing burst
//!   (Codex P2 #417 inject.rs:110 + Codex P2 #423 inject.rs:102
//!   handled inside `dictate/backends/inject.rs`). The Enigo arm of
//!   this wrapper therefore just delegates straight through.
//! * The `print (stdout only)` mode the Python `vp_inject._inject`
//!   path takes when `VOICEPI_INJECT_MODE=print` was not modeled in
//!   the Rust backend layer at all. Adding it inside
//!   `EnigoInjectBackend` would entangle the dry-run branch with the
//!   real-OS-injection code path; modelling it as an enum variant
//!   here keeps the responsibility split clean and lets the supervisor
//!   construction site read the env var without dragging
//!   process-wide-env knowledge into the backend module.
//!
//! Gating: `#[cfg(feature = "rust-injection")]` -- depends on
//! [`EnigoInjectBackend`] which only exists with that feature on. The
//! parent [`super::rust_session_real_backends`] module further gates
//! on `whisper-rs-local` so the rust-session real path requires both
//! features.

use std::sync::Mutex;

use crate::dictate::backends::EnigoInjectBackend;
use crate::dictate::session::types::{InjectBackend, InjectError};
#[cfg(test)]
use crate::injection::PasteShortcut;
use crate::injection::{InjectMethod, Injector};

/// Env var that drives the inject-mode selection. Same name the Python
/// settings layer reads (`vp_cli.py:75` / `settings_schema.json:116`).
pub(crate) const INJECT_MODE_ENV: &str = "VOICEPI_INJECT_MODE";

/// Parsed value of the inject-mode env var. Pure helper so the env
/// parse is unit-testable without going through `std::env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InjectModeChoice {
    /// Choose per utterance. On Wayland, non-ASCII text is pasted atomically
    /// because virtual-keyboard typing can silently drop layout characters.
    Auto,
    /// Type characters one-by-one. Selected only by explicit `type`.
    Typing,
    /// Send the platform paste shortcut. Selected by `paste`. The
    /// underlying [`EnigoInjectBackend`] now owns the clipboard
    /// copy/restore itself (Codex P1 #419 inject.rs:266); when the
    /// sink uses this variant a `Clipboard` backend MUST be supplied
    /// via [`ProductionInjectBackend::for_paste_with_clipboard`] or
    /// the inject will surface `InjectError::Backend("paste injection
    /// requires a clipboard backend; ...")`.
    Paste,
    /// Skip injection entirely; print `  (heard) {text}` to stdout.
    /// Mirrors the Python `vp_inject._inject` "print" branch
    /// (`vp_inject.py:603-606`).
    Print,
}

/// Map an [`InjectModeChoice`] to the [`InjectMethod`] the underlying
/// [`EnigoInjectBackend`] should use for this call. `Print` has no
/// backing method (the wrapper short-circuits before delegating), so
/// this fn defaults to `Typing` for that arm -- callers must gate on
/// `Print` before invoking. Codex P1 #619
/// runtime/rust_session_inject.rs:146: mapping lives in one place so a
/// future `inject_mode` variant cannot silently disagree between
/// `from_env`, `for_choice`, and the runtime dispatch.
pub(crate) fn enigo_method_for(choice: InjectModeChoice) -> InjectMethod {
    match choice {
        InjectModeChoice::Auto | InjectModeChoice::Typing | InjectModeChoice::Print => {
            InjectMethod::Typing
        }
        // `None` defers the paste-shortcut pick to the dispatcher (which
        // reads the target window on Linux; the enigo path collapses to
        // `PasteShortcut::default()` on Windows/macOS). A profile that
        // wants to pin a specific shortcut can wire the enigo backend
        // via `with_explicit_paste_shortcut`.
        InjectModeChoice::Paste => InjectMethod::Paste(None),
    }
}

impl InjectModeChoice {
    pub(crate) fn from_env_value(raw: Option<&str>) -> Self {
        let trimmed = raw.unwrap_or("").trim().to_ascii_lowercase();
        match trimmed.as_str() {
            "print" => Self::Print,
            "paste" => Self::Paste,
            "type" => Self::Typing,
            _ => Self::Auto,
        }
    }
}

/// Production [`InjectBackend`] for the rust-session sink. Built from
/// the live `VOICEPI_INJECT_MODE` env var by [`Self::from_env`].
///
/// # Structure (Codex P1 #607)
///
/// Previously an enum with `Enigo(...)` + `Print` variants -- swap-in
/// at construction time. The profile-matcher wiring needed a way for a
/// per-utterance profile with `inject_mode=print` to short-circuit even
/// when the initial mode was `type`, and vice versa. Hoisting the
/// active mode into a [`Mutex<InjectModeChoice>`] with an always-present
/// `EnigoInjectBackend` (constructed lazily via `Injector::new`, which is
/// cheap and does NOT talk to the OS until the first inject) is the
/// smallest change that lets profile overrides hot-swap the strategy
/// without an app restart -- matching Python's live-reload of the
/// `inject_mode` config key. The Enigo instance is always constructed so
/// a profile can override Print -> Type / Paste at any time; the Print
/// variant simply short-circuits `inject` to stdout.
pub(crate) struct ProductionInjectBackend {
    /// Active mode for the NEXT [`Self::inject`] call. Updated by
    /// [`InjectBackend::apply_profile_overrides`] when the matched
    /// profile carries an `inject_mode` key. Wrapped in [`Mutex`] so
    /// `inject(&self, ...)` still respects the trait's borrow.
    active_mode: Mutex<InjectModeChoice>,
    /// The Enigo backend used for [`InjectModeChoice::Typing`] /
    /// [`InjectModeChoice::Paste`]. Always constructed so a profile
    /// override can flip from Print to a real OS inject without an app
    /// restart. `Injector::new` is a cheap struct init that does not
    /// touch the OS.
    enigo: EnigoInjectBackend,
}

impl std::fmt::Debug for ProductionInjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProductionInjectBackend")
            .field(
                "active_mode",
                &*self.active_mode.lock().unwrap_or_else(|p| p.into_inner()),
            )
            .field("enigo", &self.enigo)
            .finish()
    }
}

impl ProductionInjectBackend {
    /// Build the variant indicated by `VOICEPI_INJECT_MODE`. Reads the
    /// env once at construction; the per-utterance profile matcher
    /// (Codex P1 #607) can flip the active mode between presses via
    /// [`InjectBackend::apply_profile_overrides`].
    ///
    /// Paste mode is honored end-to-end (Codex P1 #619
    /// runtime/rust_session_inject.rs:146). Under the hood we always
    /// keep a single `EnigoInjectBackend` and hot-swap the actual
    /// injection method at `inject()` time via
    /// [`EnigoInjectBackend::inject_using`], so a matched profile with
    /// `inject_mode=paste` (or an env value of `paste`) actually sends
    /// the paste chord rather than silently collapsing to per-character
    /// typing. A `Clipboard` backend is still required for the paste
    /// path to succeed at runtime -- callers wire it via
    /// [`EnigoInjectBackend::with_clipboard`] on the underlying
    /// backend; without one the paste arm surfaces
    /// `InjectError::Backend("paste injection requires a clipboard
    /// backend; ...")` (Codex P1 #419 inject.rs:266), which is an
    /// accurate, loud failure rather than the silent-typing regression
    /// this PR removed.
    pub(crate) fn from_env() -> Self {
        let raw = std::env::var(INJECT_MODE_ENV).ok();
        Self::for_choice(InjectModeChoice::from_env_value(raw.as_deref()))
    }

    /// Build for a specific choice. Split out so tests can construct
    /// each variant without setting env vars.
    pub(crate) fn for_choice(choice: InjectModeChoice) -> Self {
        // The constructor argument to `EnigoInjectBackend::new` is now
        // only the *starting* method -- the actual per-call method is
        // read from `active_mode` and forwarded through
        // `inject_using`, so a later `apply_profile_overrides` call
        // that flips Typing -> Paste flips the effective behaviour too.
        // Codex P1 #619: profile mode overrides must actually reach
        // the backend method, not just this wrapper's mutex slot.
        let starting = enigo_method_for(choice);
        Self::with_enigo(
            choice,
            EnigoInjectBackend::new(Injector::new(), starting).with_clipboard(Box::new(
                crate::injection::system_clipboard::SystemClipboard::default(),
            )),
        )
    }

    /// Test-only: build with an explicit paste shortcut. Kept for the
    /// legacy `explicit_paste_shortcut_round_trips` test; the wrapping
    /// backend still needs a clipboard for a real paste to fire.
    #[cfg(test)]
    pub(crate) fn with_explicit_paste_shortcut(shortcut: PasteShortcut) -> Self {
        Self::with_enigo(
            InjectModeChoice::Paste,
            EnigoInjectBackend::new(Injector::new(), InjectMethod::Paste(Some(shortcut))),
        )
    }

    /// Test-only: install a pre-built [`EnigoInjectBackend`] (typically
    /// wrapping a recording backend + clipboard fake) so the
    /// profile-override paste behaviour can be verified end-to-end
    /// without touching the real OS. Codex P1 #619 regression coverage.
    #[cfg(test)]
    pub(crate) fn with_enigo_for_test(choice: InjectModeChoice, enigo: EnigoInjectBackend) -> Self {
        Self::with_enigo(choice, enigo)
    }

    fn with_enigo(choice: InjectModeChoice, enigo: EnigoInjectBackend) -> Self {
        Self {
            active_mode: Mutex::new(choice),
            enigo,
        }
    }

    /// The currently-configured [`InjectMethod`], if any. Returns
    /// `None` when the active mode is [`InjectModeChoice::Print`].
    ///
    /// Reads the effective method from the mode Mutex so a
    /// `apply_profile_overrides` that flips `Typing` -> `Paste` at
    /// runtime is reflected here too. Falls back to the enigo
    /// constructor's method for the `Paste` arm only to preserve any
    /// explicit paste shortcut installed via
    /// [`Self::with_explicit_paste_shortcut`]; the default is
    /// `Paste(None)` which lets the dispatcher pick the shortcut at
    /// runtime.
    #[cfg(test)]
    pub(crate) fn method(&self) -> Option<InjectMethod> {
        let mode = *self.active_mode.lock().unwrap_or_else(|p| p.into_inner());
        match mode {
            InjectModeChoice::Print => None,
            InjectModeChoice::Auto => Some(InjectMethod::Typing),
            InjectModeChoice::Typing => Some(InjectMethod::Typing),
            InjectModeChoice::Paste => Some(match self.enigo.method() {
                m @ InjectMethod::Paste(_) => m,
                _ => InjectMethod::Paste(None),
            }),
        }
    }

    /// Snapshot the active mode (test-only introspection for the profile
    /// override coverage). Not meaningful to callers outside tests.
    #[cfg(test)]
    pub(crate) fn active_mode(&self) -> InjectModeChoice {
        *self.active_mode.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl InjectBackend for ProductionInjectBackend {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        let mode = *self.active_mode.lock().unwrap_or_else(|p| p.into_inner());
        match mode {
            InjectModeChoice::Print => {
                // Print to stdout, matching Python's
                // `vp_inject._inject` "print" branch literally so a
                // user grepping their log can pin the strategy that
                // ran. The leading two spaces + `(heard) ` prefix are
                // the exact format `vp_inject.py:605` emits.
                println!("  (heard) {text}");
                Ok(())
            }
            // Modifier release + clipboard ownership live inside
            // `EnigoInjectBackend::inject_using` (Codex P2 #417
            // inject.rs:110 + Codex P1 #419 inject.rs:266). The
            // wrapper forwards the *active mode* explicitly instead of
            // trusting `self.enigo.method`: a profile-driven flip from
            // Typing -> Paste updates the Mutex slot but not the
            // constructor's method field, so passing it through here
            // is what makes the paste-profile actually paste (Codex
            // P1 #619 runtime/rust_session_inject.rs:146).
            InjectModeChoice::Auto => self.enigo.inject_using(text, auto_method(text)),
            other @ (InjectModeChoice::Typing | InjectModeChoice::Paste) => {
                self.enigo.inject_using(text, enigo_method_for(other))
            }
        }
    }

    fn apply_profile_overrides(&self, settings: &std::collections::BTreeMap<String, String>) {
        // `inject_mode`: parsed through the same normaliser the env-var
        // path uses, so `type` / `auto` / unknown all land on Typing and
        // `print` / `paste` map through directly. An unset / blank value
        // RESETs the mode to the ambient env-driven choice so a profile
        // that fired for one utterance cannot leak into the next -- the
        // same reset semantics `SessionConfig` gets via the base_config
        // clone in `apply_active_profile`.
        let override_mode = settings
            .get("inject_mode")
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| InjectModeChoice::from_env_value(Some(v)));
        let new_mode = override_mode.unwrap_or_else(|| {
            InjectModeChoice::from_env_value(std::env::var(INJECT_MODE_ENV).ok().as_deref())
        });
        *self.active_mode.lock().unwrap_or_else(|p| p.into_inner()) = new_mode;
    }
}

fn auto_method(text: &str) -> InjectMethod {
    #[cfg(target_os = "linux")]
    {
        auto_method_for(text, crate::injection::LinuxSession::detect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = text;
        InjectMethod::Typing
    }
}

fn auto_method_for(text: &str, session: crate::injection::LinuxSession) -> InjectMethod {
    if !text.is_ascii()
        && matches!(
            session,
            crate::injection::LinuxSession::OtherWayland
                | crate::injection::LinuxSession::KdeWayland
        )
    {
        InjectMethod::Paste(None)
    } else {
        InjectMethod::Typing
    }
}

#[cfg(test)]
#[path = "rust_session_inject_tests.rs"]
mod tests;
