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
    /// Type characters one-by-one. Selected by `type`, `auto`, and the
    /// empty / unknown case. (The rust-session sink picks Typing for
    /// `auto` because the cross-platform paste path needs a
    /// [`crate::injection::Clipboard`] backend wired into
    /// [`EnigoInjectBackend::with_clipboard`] -- a follow-up; the
    /// `inject.rs` module-doc "Caller-owned pre-conditions" section
    /// covers the rationale.)
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

impl InjectModeChoice {
    pub(crate) fn from_env_value(raw: Option<&str>) -> Self {
        let trimmed = raw.unwrap_or("").trim().to_ascii_lowercase();
        match trimmed.as_str() {
            "print" => Self::Print,
            "paste" => Self::Paste,
            // `type`, `auto`, empty, anything unknown -> typing. Matches
            // Python's `vp_cli.py:VALID_INJECT_MODES` fallback (unknown
            // -> auto) combined with our PR-5 caveat that "auto" maps
            // to typing because the cross-platform Rust paste path
            // requires a clipboard backend the rust-session sink does
            // not own today.
            _ => Self::Typing,
        }
    }
}

/// Production [`InjectBackend`] for the rust-session sink. Built from
/// the live `VOICEPI_INJECT_MODE` env var by [`Self::from_env`].
pub(crate) enum ProductionInjectBackend {
    /// Real OS injection through the existing
    /// [`EnigoInjectBackend`]. The wrapper itself adds no behaviour
    /// here -- modifier release + clipboard copy/restore live inside
    /// the backend -- but holding the variant separately keeps the
    /// `Print` short-circuit cheap (no `Injector` constructed for the
    /// stdout-only path).
    Enigo(EnigoInjectBackend),
    /// Stdout-only "dry-run" mode. Mirrors Python's
    /// `vp_inject._inject` when `mode == "print"`.
    Print,
}

impl std::fmt::Debug for ProductionInjectBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Enigo(inner) => f
                .debug_tuple("ProductionInjectBackend::Enigo")
                .field(inner)
                .finish(),
            Self::Print => write!(f, "ProductionInjectBackend::Print"),
        }
    }
}

impl ProductionInjectBackend {
    /// Build the variant indicated by `VOICEPI_INJECT_MODE`. Reads the
    /// env once at construction; PR 5 does NOT live-reload the mode
    /// (a Settings change between presses takes effect on the next
    /// supervisor restart, matching the rust-session sink's other
    /// construction-time knobs).
    ///
    /// Paste mode collapses to Typing in this PR -- the underlying
    /// `EnigoInjectBackend` requires a `Clipboard` backend via
    /// `with_clipboard` to drive paste injection (Codex P1 #419
    /// inject.rs:266) and the rust-session sink does not own a
    /// Clipboard impl yet. A user who set
    /// `VOICEPI_INJECT_MODE=paste` therefore lands on the typing
    /// path with an actionable stderr warning so the discrepancy is
    /// visible. Wiring `arboard` is a Wave-6 follow-up.
    pub(crate) fn from_env() -> Self {
        let raw = std::env::var(INJECT_MODE_ENV).ok();
        Self::for_choice(InjectModeChoice::from_env_value(raw.as_deref()))
    }

    /// Build for a specific choice. Split out so tests can construct
    /// each variant without setting env vars.
    pub(crate) fn for_choice(choice: InjectModeChoice) -> Self {
        match choice {
            InjectModeChoice::Print => Self::Print,
            // Paste collapses to Typing -- see `from_env` docs.
            InjectModeChoice::Typing | InjectModeChoice::Paste => Self::Enigo(
                EnigoInjectBackend::new(Injector::new(), InjectMethod::Typing),
            ),
        }
    }

    /// Test-only: build the Enigo variant with an explicit paste
    /// shortcut. The wrapping `EnigoInjectBackend` would still need a
    /// clipboard for the chord to actually fire; this constructor is
    /// only exercised by tests that swap in a recording backend.
    #[cfg(test)]
    pub(crate) fn with_explicit_paste_shortcut(shortcut: PasteShortcut) -> Self {
        Self::Enigo(EnigoInjectBackend::new(
            Injector::new(),
            InjectMethod::Paste(Some(shortcut)),
        ))
    }

    /// The currently-configured [`InjectMethod`], if any. Returns
    /// `None` for the [`Self::Print`] variant.
    #[cfg(test)]
    pub(crate) fn method(&self) -> Option<InjectMethod> {
        match self {
            Self::Enigo(inner) => Some(inner.method()),
            Self::Print => None,
        }
    }
}

impl InjectBackend for ProductionInjectBackend {
    fn inject(&self, text: &str) -> Result<(), InjectError> {
        match self {
            Self::Print => {
                // Print to stdout, matching Python's
                // `vp_inject._inject` "print" branch literally so a
                // user grepping their log can pin the strategy that
                // ran. The leading two spaces + `(heard) ` prefix are
                // the exact format `vp_inject.py:605` emits.
                println!("  (heard) {text}");
                Ok(())
            }
            // Modifier release + clipboard ownership now live inside
            // `EnigoInjectBackend::inject` (Codex P2 #417 inject.rs:110
            // + Codex P1 #419 inject.rs:266); the wrapper just passes
            // the call straight through.
            Self::Enigo(inner) => inner.inject(text),
        }
    }
}

#[cfg(test)]
#[path = "rust_session_inject_tests.rs"]
mod tests;
