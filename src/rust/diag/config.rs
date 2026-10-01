//! Cached diagnostic level and callback gates.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// Standard log level, read once at startup from [`LOG_ENV_VAR`] and
/// cached in a process-wide atomic so trace call sites can check with
/// a single relaxed load — matters because the Windows LL-hook thread
/// fires the check on every desktop-wide key press.
///
/// Names follow the Rust ecosystem convention (`log`/`tracing`) so a
/// user reading `VOICEPI_LOG=debug` recognises what to expect without
/// a per-project glossary:
///
/// * [`Off`] — no diagnostic output at all. Even startup markers are
///   suppressed.
/// * [`Error`] — only errors that stopped something working (rdev
///   listener startup failure, hotkey register failure, ...).
/// * [`Warn`] — errors we recovered from (fallback branch taken,
///   Phase-B degraded, ...).
/// * [`Info`] — normal lifecycle events: startup markers, the rdev listener
///   heartbeat, rate-limited per-event trace, and session start / stop events.
///   This is the default for release binaries.
/// * [`Debug`] — internal state transitions: rdev callback boundary
///   trace, chord matcher trace, coordinator state transitions,
///   session dispatch. Bump to this level when investigating a wedge
///   the info-level heartbeat cannot pinpoint.
/// * [`Trace`] — everything, including the parallel Windows
///   `WH_KEYBOARD_LL` hook that dumps every desktop-wide key event's
///   virtual-key / scan code / flags. High volume (500+ lines/min of
///   typing); only for active debugging.
///
/// Unknown values default to [`Info`] (the release default) plus a
/// one-shot warning so a `VOICEPI_LOG=debgu` typo is visible in the
/// log rather than silently downgraded to `Off`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    /// Numeric encoding for the [`LEVEL`] atomic. Ordered so a
    /// monotone comparison (`current >= threshold as u8`) works; call
    /// sites should still prefer the [`info_enabled`] /
    /// [`debug_enabled`] / [`trace_enabled`] helpers for readability.
    pub(super) const fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Off,
            1 => Self::Error,
            2 => Self::Warn,
            4 => Self::Debug,
            5 => Self::Trace,
            _ => Self::Info,
        }
    }

    /// Parse a raw env-var value. Case-insensitive; whitespace
    /// trimmed. Returns `None` for unknown values so [`init_from_env`]
    /// can warn and pick the [`Info`] default rather than silently
    /// promoting a typo. Accepts the standard names PLUS the older
    /// numeric aliases the rest of the settings surface uses (`0`/`1`)
    /// so a user typing `VOICEPI_LOG=1` gets the same behaviour they
    /// know from `VOICEPI_DEBUG=1`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" | "none" => Some(Self::Off),
            "error" | "err" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            // Empty is treated the same as unset → the release default
            // (Info). Truthy synonyms map to Info too so an existing
            // habit of "VOICEPI_LOG=1" doesn't accidentally jump to
            // Debug or Trace and flood the tee file.
            "" | "info" | "1" | "true" | "yes" | "on" | "default" => Some(Self::Info),
            "debug" | "dbg" => Some(Self::Debug),
            "trace" | "verbose" | "all" | "full" => Some(Self::Trace),
            _ => None,
        }
    }

    /// Short lowercase name for the log-emitted startup line. Stable
    /// across releases so grep strings in support runbooks
    /// (`grep VOICEPI_LOG=debug`) keep working.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Env-var name that selects the [`LogLevel`]. Mirrors `RUST_LOG` in
/// naming so a Rust developer intuits what it does without reading
/// the docs. Documented in `docs/CONFIGURATION.md` under the
/// "Diagnostic env vars" section.
pub const LOG_ENV_VAR: &str = "VOICEPI_LOG";

/// Process-wide cached level. Written once by [`init_from_env`], read
/// on every trace call site. Atomic (not `OnceLock<LogLevel>`) so the
/// check on the LL-hook hot path is a single relaxed load — a
/// `OnceLock::get()` would go through the `once_cell::race` guard,
/// which is heavier than we can afford in the raw-hook callback.
///
/// Default is [`LogLevel::Info`] before [`init_from_env`] runs so the
/// tiny handful of startup lines emitted BEFORE the env var is read
/// (`install_gui_diagnostic_log` completion) are still visible. The
/// moment `init_from_env` runs, the resolved value overwrites this.
pub(super) static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info.as_u8());

/// One-shot latch so [`init_from_env`] logging the "unknown value"
/// warning is idempotent — a second call from a test won't re-emit
/// the warning line.
static UNKNOWN_VALUE_WARNED: OnceLock<()> = OnceLock::new();

/// Read [`LOG_ENV_VAR`] from the environment and cache the resolved
/// [`LogLevel`] in the process-wide atomic. Called exactly once from
/// `whisper-dictate-gui::main` right after
/// [`install_gui_diagnostic_log`] so the startup marker line already
/// benefits from the picked level.
///
/// Returns the resolved level so the caller can include it in the
/// startup marker.
///
/// * Unset / empty → [`LogLevel::Info`] (release default).
/// * A known value → that level.
/// * An unknown value → [`LogLevel::Info`] plus a one-shot warning
///   emitted through [`log`] so the operator sees the typo.
pub fn init_from_env() -> LogLevel {
    let raw = std::env::var(LOG_ENV_VAR).unwrap_or_default();
    configure_level(&raw)
}

/// Apply an already-resolved native diagnostic level without consulting or
/// mutating the process environment.
pub(crate) fn configure_level(raw: &str) -> LogLevel {
    let resolved = match LogLevel::parse(raw) {
        Some(level) => level,
        None => {
            if UNKNOWN_VALUE_WARNED.set(()).is_ok() {
                crate::diag::log!(
                    "[diag] unknown {LOG_ENV_VAR}={raw:?}; defaulting to `info`. \
                     Accepted values: off, error, warn, info, debug, trace."
                );
            }
            LogLevel::Info
        }
    };
    LEVEL.store(resolved.as_u8(), Ordering::Relaxed);
    resolved
}

/// Current log level. Cheap — one relaxed atomic load.
pub fn current_level() -> LogLevel {
    LogLevel::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// True when the level is [`LogLevel::Info`] or more verbose. Gate
/// for normal lifecycle diagnostics: startup markers, the rdev
/// listener heartbeat, rate-limited per-event trace, and session-start events.
/// This level is enabled by default for release binaries.
#[inline]
pub fn info_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Info.as_u8()
}

/// True when the level is [`LogLevel::Debug`] or more verbose. Gate
/// for internal-state trace: rdev callback boundary, chord matcher,
/// coordinator state transitions, session dispatch refuse/emit
/// branches. Off by default — users opt in with
/// `VOICEPI_LOG=debug` when investigating a wedge that info-level
/// alone can't pinpoint.
#[inline]
pub fn debug_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Debug.as_u8()
}

/// True when the level is [`LogLevel::Trace`]. Gate for the
/// highest-volume layer: the parallel Windows `WH_KEYBOARD_LL` hook
/// that dumps every desktop-wide key event. Off by default; opt in
/// with `VOICEPI_LOG=trace` only when actively debugging a
/// key-drop where debug-level cannot see the event on either side.
#[inline]
pub fn trace_enabled() -> bool {
    LEVEL.load(Ordering::Relaxed) >= LogLevel::Trace.as_u8()
}

/// True when ANY diagnostic that fires from inside an OS input callback
/// can actually be emitted at the current level.
///
/// Every callback-path call site is individually gated: the rdev boundary
/// trace and the tracker `[chord]` trace by [`debug_enabled`], the rdev
/// per-event sample by [`info_enabled`], the Windows raw-hook dump by
/// [`trace_enabled`]. The most permissive of those is [`info_enabled`],
/// so that IS the union — below `info` not one of them can fire and the
/// off-callback writer has no work at all.
///
/// ## Why this deserves a name
///
/// It is the precondition for treating a dead
/// [`async_writer_result`] as FATAL. The rdev listener refuses to install
/// its OS hook when the writer thread failed to spawn, on the reasoning
/// that hooking would leave the operator reading a `gui-diagnostic.log`
/// whose callback trace cannot exist. That reasoning only holds while
/// something would have been written: at `off` / `error` / `warn` the
/// failed writer is UNUSED, so aborting turns a working Rust-hotkey
/// install into a Python fallback over a diagnostic nobody asked for -
/// and at `off` without even a line saying why. Spelling the condition
/// out here keeps the rdev driver and
/// [`crate::hotkey::manager::win_raw_hook::install_gate`] (which already
/// short-circuits on its own `trace_enabled` gate) making the same
/// judgement for the same reason.
#[inline]
pub fn callback_diagnostics_enabled() -> bool {
    info_enabled()
}

/// Test-only accessor to reset the cached level so a later test
/// can drive [`init_from_env`] from a fresh state without leaking the
/// previous test's env-var choice.
#[cfg(test)]
pub(crate) fn reset_level_for_tests() {
    LEVEL.store(LogLevel::Info.as_u8(), Ordering::Relaxed);
}

/// Test-only: force the cached level, so a test can assert a gate's
/// behaviour across all six levels without going through the environment
/// (which is process-global and races other suites). Callers MUST hold
/// `crate::diag_test_lock::DIAG_WRITER_LOCK` and reset afterwards.
///
/// Its only caller (`rdev_driver_tests::…`) lives behind `rust-hotkeys`,
/// so on a feature set that omits it — the devcontainer dev-loop runs
/// `--features ui-egui-glow` alone — this compiles with no callers and
/// trips `-D dead-code`. The seam is still wanted there: a future
/// non-hotkey test asserting a level gate should reach for this rather
/// than mutating `VOICEPI_LOG` process-globally.
#[cfg(test)]
#[cfg_attr(not(feature = "rust-hotkeys"), allow(dead_code))]
pub(crate) fn set_level_for_tests(level: LogLevel) {
    LEVEL.store(level.as_u8(), Ordering::Relaxed);
}
