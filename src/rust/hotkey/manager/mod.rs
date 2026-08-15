//! Hotkey manager — owns the global key-event listener in its own thread and
//! translates raw OS key events into the side-aware press / release / cancel
//! signals the coordinator consumes.
//!
//! Layers, separated so the bulk is testable without any platform crate and so
//! production logic stays within the repo-wide ≤500-LOC per file rule
//! (`AGENTS.md`):
//!
//! * [`tracker`] (always compiled) — a pure state machine that takes a
//!   stream of [`RawKeyEvent`]s and the user's PTT binding and emits
//!   [`TrackerOutput`]s (`ChordPress`, `ChordRelease`, `ChordCancel`).
//!   Holds the side-aware target/foreign membership using
//!   [`super::modifier_match::modifier_matches`] and the rising-edge latch
//!   so key-repeat never re-fires a press.
//!
//! * [`driver_common`] (`#[cfg(feature = "rust-hotkeys")]`) — the backend-
//!   agnostic half: the [`ManagerHandle`] / [`ManagerThread`] / [`SpawnError`]
//!   contract and the manager thread that swaps the active binding via an
//!   mpsc command API (the OS listener runs on its own thread with non-`Send`
//!   handles, so the rest of the runtime talks to it only through this
//!   channel). Both drivers construct their sender via
//!   [`driver_common::manager_channel`] and start the manager thread via
//!   [`driver_common::spawn_manager_thread`].
//!
//! * [`rdev_driver`] / [`evdev_driver`] (`#[cfg(feature = "rust-hotkeys")]`) —
//!   the two platform listeners. rdev drives X11 / Windows / macOS via the
//!   global hook; evdev reads `/dev/input` directly on Linux, the only path
//!   that observes global keys under a Wayland compositor (rdev's X11 XRecord
//!   is deaf there). [`spawn`] / [`spawn_with_raw_tap`] pick between them per
//!   session — see their docs. Both surface startup failures (no X display /
//!   missing accessibility permission for rdev; no readable keyboard node for
//!   evdev) to the caller. The evdev driver also excludes whisper-dictate's
//!   own injection uinput devices (ydotool / wtype / kwtype / dotool / enigo)
//!   from enumeration so injected text cannot feed back into the PTT tracker
//!   — that's the v1.20.2 #467 fix, baked in at driver introduction.

pub mod tracker;

// Companion tests for `tracker.rs` that need the process-wide diag
// sink or the env-var lock — kept out of the inline `#[cfg(test)]
// mod tests` inside `tracker.rs` so that file stays under the 500-LOC
// modularity rule. The pure-state-machine tests stay inline.
#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tracker_tests;

#[cfg(feature = "rust-hotkeys")]
pub mod driver_common;

#[cfg(feature = "rust-hotkeys")]
pub mod rdev_driver;

// Companion tests for `rdev_driver.rs`. Extracted from an inline
// `#[cfg(test)] mod tests` so the regression-test discipline scanner sees
// a matching test file next to the production module — see
// `src/tests/python/test_regression_test_discipline.py`.
#[cfg(all(test, feature = "rust-hotkeys"))]
#[path = "rdev_driver_tests.rs"]
mod rdev_driver_tests;

// evdev backend is Linux-only — it reads `/dev/input` directly, which is the
// only listener that works under Wayland (rdev's X11 XRecord is deaf there).
#[cfg(all(feature = "rust-hotkeys", target_os = "linux"))]
pub mod evdev_driver;

// RegisterHotKey backend is Windows-only. It bypasses the WH_KEYBOARD_LL
// hook chain that rdev's global hook rides on — a workaround for third-
// party apps (Steam / Logitech Options+/G HUB / screen-capture tools)
// installing LL hooks in the GUI process context that filter function
// keys and Ctrl before our hook sees them. Diagnosed on PR #646 rc.10
// GUI diagnostic log: letters + Windows key + digits reached rdev, but
// f9 / ctrl_l / pause never did — a signature of an upstream LL-hook
// filter. RegisterHotKey delivers WM_HOTKEY after the hook chain runs,
// so the chord fires reliably.
#[cfg(all(feature = "rust-hotkeys", target_os = "windows"))]
pub mod win_registerhotkey;

// Companion tests for `win_registerhotkey.rs`. Extracted from an inline
// `#[cfg(test)] mod tests` so the regression-test discipline scanner
// sees a matching test file next to the production module.
#[cfg(all(test, feature = "rust-hotkeys", target_os = "windows"))]
#[path = "win_registerhotkey_tests.rs"]
mod win_registerhotkey_tests;

// Parallel WH_KEYBOARD_LL diagnostic hook. Windows-only — see the
// module docs for the "did F9 physically reach the process" story.
// Not feature-gated on `rust-hotkeys` because the diagnostic must
// work even on the stock build where the rest of the hotkey stack is
// compiled out (a user with a broken PTT reporting a wedge should be
// able to opt in via `VOICEPI_LOG=trace` without a rebuild).
//
// Independent of the RegisterHotKey backend above: this hook only
// OBSERVES (never consumes) and runs in parallel with whichever
// driver is active, so a `trace`-level operator investigating a new
// wedge can see the LL-chain behaviour even when the runtime has
// switched to the RegisterHotKey path.
//
// Compiled on every platform since the raw-hook trace-line formatter
// and its rate limiter were split out of the `#![cfg(windows)]` gate —
// the Win32 wiring inside the module carries per-item `#[cfg(windows)]`
// so nothing platform-specific leaks into a Linux build.
pub mod win_raw_hook;

// Companion tests for `win_raw_hook.rs`. The pure-helper tests run
// everywhere; the genuinely-Windows ones (the `install()` gate) carry
// their own `#[cfg(windows)]` inside the file.
#[cfg(test)]
#[path = "win_raw_hook_tests.rs"]
mod win_raw_hook_tests;

// Re-export the always-compiled tracker types at the manager level so call
// sites can keep using `manager::KeyTracker` / `manager::RawKeyEvent` etc.
// without caring about the sub-module split.
pub use tracker::{
    is_chord_key, KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput, FOREIGN_KEY_EXPIRY,
};

// Manager plumbing (`ManagerHandle` / `ManagerThread` / `SpawnError` and the
// driver-agnostic `RawTap` value type) comes from `driver_common` so both
// drivers reuse it — the extraction is what lets the manager-level `spawn`
// selector below dispatch to either backend without a trait-mismatch on the
// `R: RawTap` bound.
//
// The rdev-side `spawn` (no raw tap) is intentionally NOT re-exported:
// production callers always go through the selector's [`spawn_with_raw_tap`]
// below, and the only remaining callers of the rdev-only `spawn` are its own
// sibling unit tests. Keeping the surface narrow means fewer places to update
// if the signature grows again (e.g. #507 added `injection_guard` to the
// rdev callback path).
#[cfg(feature = "rust-hotkeys")]
pub use driver_common::{ManagerHandle, ManagerThread, NoopRawTap, RawTap, SpawnError};

// The install-time validator for PTT chord names comes from the rdev module
// today because the rdev backend's supported-name table is the tightest
// (both drivers accept the same names but rdev is the most restrictive
// physical map). Kept re-exported from here so a future driver swap doesn't
// churn the call sites.
#[cfg(feature = "rust-hotkeys")]
pub use rdev_driver::is_rdev_supported_name;

/// Which OS listener to install. `Auto` picks per session (evdev on Linux
/// Wayland, rdev everywhere else). Explicit variants are the escape hatch for
/// debugging / smoke scripts that need to pin a specific backend.
///
/// Parsed from `VOICEPI_HOTKEY_DRIVER=auto|rdev|evdev|register` in
/// [`driver_from_env`] and from the `--driver` flag in the
/// `whisper-dictate hotkey capture` CLI (which sets the env var before
/// calling into `install_hotkey`). The `register` value is Windows-only
/// and selects the `RegisterHotKey` backend that bypasses the
/// WH_KEYBOARD_LL hook chain — see [`win_registerhotkey`] for the root
/// cause explanation.
#[cfg(feature = "rust-hotkeys")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    /// Auto-detect per session: evdev on Linux Wayland, rdev everywhere else.
    /// Windows keeps rdev as the auto default so the CLI `dictate-run` verb
    /// (console subsystem — no LL-hook interference observed) is unaffected;
    /// the GUI binary opts INTO `Register` explicitly in its `main` so the
    /// GUI-subsystem LL-hook-chain interference is bypassed. See
    /// [`resolve_driver`] for the resolution table.
    Auto,
    /// Force rdev (X11 on Linux, WH_KEYBOARD_LL on Windows, CGEventTap on
    /// macOS). On Linux Wayland this listener is deaf — reported as a
    /// startup failure via `SpawnError::ListenerStartup`.
    Rdev,
    /// Force evdev (Linux only). On non-Linux targets [`spawn_with_raw_tap`]
    /// falls back to rdev with a warning; the caller sees the rdev name in
    /// the install envelope.
    Evdev,
    /// Force Windows `RegisterHotKey` (Windows only). Bypasses the
    /// WH_KEYBOARD_LL hook chain so third-party LL hooks
    /// (Steam / Logitech Options+ / G HUB / screen-capture tools) cannot
    /// filter the chord out of the chain. Modifier-only chords are NOT
    /// supported by RegisterHotKey — install fails with a clear message.
    /// On non-Windows targets [`spawn_with_raw_tap`] falls back to rdev
    /// with a warning; the caller sees the rdev name in the install
    /// envelope.
    Register,
}

#[cfg(feature = "rust-hotkeys")]
impl DriverKind {
    /// Parse a `VOICEPI_HOTKEY_DRIVER` / `--driver` value. Returns `None` for
    /// unrecognised values so callers can fall back to `Auto` instead of
    /// hard-erroring on a typo. Accepts `x11` as a friendly alias for `rdev`
    /// (both mean "the X11-style global hook" on Linux), `wayland` as an
    /// alias for `evdev` (the only Wayland-capable backend), and
    /// `win_registerhotkey` / `wm_hotkey` as verbose aliases for `register`.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "rdev" | "x11" => Some(Self::Rdev),
            "evdev" | "wayland" => Some(Self::Evdev),
            "register" | "win_registerhotkey" | "wm_hotkey" => Some(Self::Register),
            _ => None,
        }
    }
}

/// Read the [`DriverKind`] preference from `VOICEPI_HOTKEY_DRIVER`. Falls back
/// to `Auto` when unset, empty, or holding an unrecognised value.
#[cfg(feature = "rust-hotkeys")]
pub fn driver_from_env() -> DriverKind {
    std::env::var("VOICEPI_HOTKEY_DRIVER")
        .ok()
        .and_then(|v| DriverKind::parse(&v))
        .unwrap_or(DriverKind::Auto)
}

/// The concrete driver `spawn_with_raw_tap` decided to use, returned alongside
/// the manager pair so the caller can surface it (install envelope, log lines,
/// diagnostic CLI's `driver=` field). Kept as a `&'static str` because the
/// value is stable per install and cheap to pass around.
#[cfg(feature = "rust-hotkeys")]
pub const DRIVER_NAME_RDEV: &str = "rdev";
#[cfg(feature = "rust-hotkeys")]
pub const DRIVER_NAME_EVDEV: &str = "evdev";
/// Diagnostic label for the Windows RegisterHotKey backend. Prefixed
/// `win_` so a Linux-only grep for "rdev" / "evdev" can't accidentally
/// match it, and named after the actual Win32 API for support-thread
/// searchability (users pasting "why is my WhisperDictate log saying
/// win_registerhotkey" reach real docs).
#[cfg(feature = "rust-hotkeys")]
pub const DRIVER_NAME_REGISTER: &str = "win_registerhotkey";

/// The driver name [`spawn_with_driver`] would pick for `kind`, WITHOUT
/// spawning anything.
///
/// The PTT ownership guard runs before any listener starts (it must, so a
/// refused process leaves no threads behind) but still wants to record
/// which backend this process intended to use, so the NEXT process's
/// refusal message can say "the holder is on win_registerhotkey". Resolving
/// `Auto` here rather than reading the name off the spawned handle is what
/// makes that possible.
///
/// Platform-inapplicable explicit selections are normalized to the same rdev
/// fallback the spawn shims use, so preflight and ownership diagnostics do not
/// claim that `evdev` was installed on Windows/macOS or that RegisterHotKey was
/// installed outside Windows. Chord-shape fallback is applied by the caller
/// before it asks for this label.
#[cfg(feature = "rust-hotkeys")]
pub fn driver_label(kind: DriverKind) -> &'static str {
    match resolve_driver(kind) {
        DriverKind::Evdev => {
            #[cfg(target_os = "linux")]
            {
                DRIVER_NAME_EVDEV
            }
            #[cfg(not(target_os = "linux"))]
            {
                DRIVER_NAME_RDEV
            }
        }
        DriverKind::Register => {
            #[cfg(target_os = "windows")]
            {
                DRIVER_NAME_REGISTER
            }
            #[cfg(not(target_os = "windows"))]
            {
                DRIVER_NAME_RDEV
            }
        }
        _ => DRIVER_NAME_RDEV,
    }
}

/// Spawn the OS key-event listener, picking the backend that actually works on
/// the running session:
///
/// * **Linux + Wayland** → [`evdev_driver`] (reads `/dev/input` directly; the
///   only path that sees global keys under a Wayland compositor).
/// * **Linux + X11**, **Windows**, **macOS** → [`rdev_driver`] (the global
///   hook / XRecord path).
///
/// The choice can be forced with `VOICEPI_HOTKEY_DRIVER=auto|rdev|evdev` (also
/// accepts `x11`/`wayland` as aliases) for debugging or as an escape hatch.
/// Everything downstream ([`ManagerHandle`], the tracker, the coordinator) is
/// backend-agnostic, so callers never care which fired — except for the
/// [`&'static str`] this returns alongside the pair, which is the driver name
/// the diagnostic CLI reports in its install envelope.
///
/// `injection_guard` is threaded through to whichever backend `resolve_driver`
/// picks. On rdev it closes the Windows self-injection PTT wedge #507 landed;
/// on evdev it is the belt-and-braces second layer behind device-enumeration
/// exclusion (`INJECTION_DEVICE_MARKERS`) so a future non-`/dev/input`
/// injection path (libei / portals) is still filtered.
#[cfg(feature = "rust-hotkeys")]
pub fn spawn<F>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    spawn_with_raw_tap(injection_guard, on_output, NoopRawTap)
}

/// Same as [`spawn`] but also invokes `raw_tap` for every raw OS key event
/// BEFORE the tracker sees it (and before the injection guard's check — the
/// diagnostic `hotkey capture` CLI still sees suppressed events, only the
/// tracker is shielded). The tap runs on the listener thread (or, for
/// evdev, on the per-device reader thread) — keep it cheap and non-blocking.
///
/// Returns the driver name (`"rdev"` / `"evdev"`) alongside the pair so the
/// caller can surface it verbatim (install envelope, log lines). The value is
/// stable for the lifetime of the returned handle.
#[cfg(feature = "rust-hotkeys")]
pub fn spawn_with_raw_tap<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let kind = driver_from_env();
    spawn_with_driver(kind, injection_guard, on_output, raw_tap)
}

/// Underlying dispatch used by [`spawn`] / [`spawn_with_raw_tap`]. Exposed for
/// unit tests that want to pin the selection without setting the process-wide
/// env var (which would race other threads).
///
/// The `Evdev` variant is Linux-only, `Register` is Windows-only; on the
/// wrong OS each silently falls back to rdev — the caller sees `"rdev"` in
/// the returned name.
///
/// The `Register` variant additionally falls back to rdev if the
/// RegisterHotKey install fails on Windows (invalid chord / already-owned
/// hotkey), with a diagnostic-log line so the fallback is inspectable.
#[cfg(feature = "rust-hotkeys")]
pub fn spawn_with_driver<F, R>(
    kind: DriverKind,
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let effective = resolve_driver(kind);
    match effective {
        DriverKind::Evdev => spawn_evdev(injection_guard, on_output, raw_tap),
        DriverKind::Register => spawn_register(injection_guard, on_output, raw_tap),
        // `Auto` is resolved by `resolve_driver` — should never reach here.
        _ => spawn_rdev(injection_guard, on_output, raw_tap),
    }
}

/// Resolve `Auto` to the backend that fits the current session.
///
/// * Linux Wayland → `Evdev` (the only Wayland-capable backend).
/// * Linux X11, Windows, macOS → `Rdev`.
///
/// Windows `Auto` DELIBERATELY stays on `Rdev` even though the
/// GUI-subsystem process context can hit LL-hook chain interference
/// (see [`win_registerhotkey`] for the diagnosis). The CLI verbs
/// (`dictate-run`, `hotkey capture`) run under the console subsystem
/// and are not affected — keeping their default on rdev avoids
/// regressing modifier-only chord support for the audience that
/// hasn't been bitten by the wedge. The GUI binary explicitly opts
/// into `Register` in its `main` by setting
/// `VOICEPI_HOTKEY_DRIVER=register` before install, which then routes
/// through this same resolver.
///
/// Explicit `Rdev` / `Evdev` / `Register` are returned unchanged so a
/// deliberate override always wins over the auto-detect.
#[cfg(feature = "rust-hotkeys")]
fn resolve_driver(kind: DriverKind) -> DriverKind {
    match kind {
        DriverKind::Rdev | DriverKind::Evdev | DriverKind::Register => kind,
        DriverKind::Auto => {
            #[cfg(target_os = "linux")]
            {
                if is_wayland_session() {
                    return DriverKind::Evdev;
                }
                DriverKind::Rdev
            }
            #[cfg(not(target_os = "linux"))]
            {
                DriverKind::Rdev
            }
        }
    }
}

/// True when the process is running under a Wayland session. Checks both
/// `XDG_SESSION_TYPE=wayland` and a non-empty `WAYLAND_DISPLAY`, since some
/// launch environments set only one.
#[cfg(all(feature = "rust-hotkeys", target_os = "linux"))]
fn is_wayland_session() -> bool {
    let session_type_wayland = std::env::var("XDG_SESSION_TYPE")
        .map(|v| v.eq_ignore_ascii_case("wayland"))
        .unwrap_or(false);
    let has_wayland_display = std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    session_type_wayland || has_wayland_display
}

// -----------------------------------------------------------------------
// Driver bridges — thin wrappers that call the concrete backend and tag
// the returned pair with the driver name so the caller can surface it.
// -----------------------------------------------------------------------

#[cfg(feature = "rust-hotkeys")]
fn spawn_rdev<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (h, t) = rdev_driver::spawn_with_raw_tap(injection_guard, on_output, raw_tap)?;
    Ok((DRIVER_NAME_RDEV, h, t))
}

/// Linux-only evdev spawn. On non-Linux targets this shim falls back to rdev
/// so a `VOICEPI_HOTKEY_DRIVER=evdev` override on the wrong OS still installs
/// SOMETHING (with a stderr warning) rather than hard-failing. The returned
/// driver name is `"rdev"` on that path so the caller isn't misled.
#[cfg(all(feature = "rust-hotkeys", target_os = "linux"))]
fn spawn_evdev<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (h, t) = evdev_driver::spawn_with_raw_tap(injection_guard, on_output, raw_tap)?;
    Ok((DRIVER_NAME_EVDEV, h, t))
}

#[cfg(all(feature = "rust-hotkeys", not(target_os = "linux")))]
fn spawn_evdev<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    eprintln!(
        "[hotkey] VOICEPI_HOTKEY_DRIVER=evdev requested on non-Linux target; \
         falling back to rdev (evdev is /dev/input and Linux-only)"
    );
    spawn_rdev(injection_guard, on_output, raw_tap)
}

/// Windows-only RegisterHotKey spawn. Bypasses the LL-hook chain
/// (see [`win_registerhotkey`] module docs for the root-cause
/// diagnosis). The spawn itself only creates a message-loop thread and
/// is unlikely to fail; actual chord-registration failures (invalid
/// chord, hotkey already owned by another app, modifier-only binding)
/// surface later, from `ManagerHandle::register` inside
/// `install_hotkey_with_raw_tap` — the retry-with-rdev fallback lives
/// there so it can consume the failed handle cleanly rather than
/// leaking a half-installed manager thread here.
#[cfg(all(feature = "rust-hotkeys", target_os = "windows"))]
fn spawn_register<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let (h, t) = win_registerhotkey::spawn_with_raw_tap(injection_guard, on_output, raw_tap)?;
    Ok((DRIVER_NAME_REGISTER, h, t))
}

#[cfg(all(feature = "rust-hotkeys", not(target_os = "windows")))]
fn spawn_register<F, R>(
    injection_guard: std::sync::Arc<crate::hotkey::inject_guard::InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> std::result::Result<(&'static str, ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    eprintln!(
        "[hotkey] VOICEPI_HOTKEY_DRIVER=register requested on non-Windows \
         target; falling back to rdev (RegisterHotKey is a USER32 API \
         and Windows-only)"
    );
    spawn_rdev(injection_guard, on_output, raw_tap)
}

#[cfg(all(test, feature = "rust-hotkeys"))]
mod tests {
    use super::*;

    #[test]
    fn driver_kind_parse_accepts_canonical_names() {
        assert_eq!(DriverKind::parse("auto"), Some(DriverKind::Auto));
        assert_eq!(DriverKind::parse("rdev"), Some(DriverKind::Rdev));
        assert_eq!(DriverKind::parse("evdev"), Some(DriverKind::Evdev));
        assert_eq!(DriverKind::parse("register"), Some(DriverKind::Register));
    }

    #[test]
    fn driver_kind_parse_accepts_register_verbose_aliases() {
        // Both the friendly short form (`register`) and the Win32-API-
        // named verbose forms (`win_registerhotkey` / `wm_hotkey`) map
        // to the same driver so support-thread pastes of any of them
        // land the operator on the right backend.
        assert_eq!(
            DriverKind::parse("win_registerhotkey"),
            Some(DriverKind::Register)
        );
        assert_eq!(DriverKind::parse("wm_hotkey"), Some(DriverKind::Register));
        assert_eq!(DriverKind::parse("REGISTER"), Some(DriverKind::Register));
    }

    #[test]
    fn resolve_driver_passes_register_through_unchanged() {
        // Explicit `Register` must NEVER be silently reinterpreted —
        // the whole reason the GUI opts in is to bypass rdev's hook
        // chain. A future refactor that reinterpreted Register into
        // Rdev on non-Windows silently would defeat the purpose (and
        // is fine, since on non-Windows the spawn-side shim falls
        // back to rdev explicitly with a warning). The resolver stays
        // pure so the fallback happens at spawn time where the
        // operator sees the diagnostic line.
        assert_eq!(resolve_driver(DriverKind::Register), DriverKind::Register);
    }

    #[test]
    fn driver_kind_parse_accepts_x11_and_wayland_aliases() {
        // The CLI accepts session-name aliases so users can reach for the
        // display server name they know (`--driver wayland`) rather than
        // the crate name.
        assert_eq!(DriverKind::parse("x11"), Some(DriverKind::Rdev));
        assert_eq!(DriverKind::parse("wayland"), Some(DriverKind::Evdev));
    }

    #[test]
    fn driver_kind_parse_is_case_insensitive_and_trims() {
        assert_eq!(DriverKind::parse(" AUTO "), Some(DriverKind::Auto));
        assert_eq!(DriverKind::parse("Evdev"), Some(DriverKind::Evdev));
        assert_eq!(DriverKind::parse("\tRDEV\n"), Some(DriverKind::Rdev));
    }

    #[test]
    fn driver_kind_parse_empty_is_auto() {
        // Empty string / whitespace means "not set" — treat as auto so the
        // env var can be present-but-empty without breaking behaviour.
        assert_eq!(DriverKind::parse(""), Some(DriverKind::Auto));
        assert_eq!(DriverKind::parse("   "), Some(DriverKind::Auto));
    }

    #[test]
    fn driver_kind_parse_unknown_returns_none() {
        // A typo must NOT silently map to Auto here (callers fall back on
        // None) so the CLI can surface an actionable error.
        assert_eq!(DriverKind::parse("uinput"), None);
        assert_eq!(DriverKind::parse("libinput"), None);
        assert_eq!(DriverKind::parse("garbage"), None);
    }

    #[test]
    fn driver_label_names_the_backend_the_ptt_lock_will_record() {
        // The PTT ownership record is written before any listener spawns,
        // so this is the only place the intended backend name comes from.
        // A label that lied would put the wrong driver into the next
        // process's refusal message.
        assert_eq!(driver_label(DriverKind::Rdev), DRIVER_NAME_RDEV);
        assert_eq!(
            driver_label(DriverKind::Evdev),
            if cfg!(target_os = "linux") {
                DRIVER_NAME_EVDEV
            } else {
                DRIVER_NAME_RDEV
            }
        );
        assert_eq!(
            driver_label(DriverKind::Register),
            if cfg!(target_os = "windows") {
                DRIVER_NAME_REGISTER
            } else {
                DRIVER_NAME_RDEV
            }
        );
    }

    #[test]
    fn driver_label_resolves_auto_rather_than_reporting_it() {
        // `Auto` is not a backend; recording it would tell a blocked user
        // nothing. It must resolve to whatever this session would use.
        let label = driver_label(DriverKind::Auto);
        assert!(
            [DRIVER_NAME_RDEV, DRIVER_NAME_EVDEV].contains(&label),
            "Auto must resolve to a concrete backend, got {label}"
        );
    }

    #[test]
    fn resolve_driver_passes_explicit_choice_unchanged() {
        // An explicit override MUST NOT be overridden by session detection.
        // Otherwise the CLI's `--driver evdev` on X11 would silently pick
        // rdev anyway, defeating the escape hatch.
        assert_eq!(resolve_driver(DriverKind::Rdev), DriverKind::Rdev);
        assert_eq!(resolve_driver(DriverKind::Evdev), DriverKind::Evdev);
        assert_eq!(resolve_driver(DriverKind::Register), DriverKind::Register);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_driver_auto_picks_wayland_or_x11_backend() {
        // Auto-resolves based on the ambient session. Guard the env with
        // the crate lock and restore afterwards so we don't race the other
        // env-mutating tests in the binary.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev_type = std::env::var("XDG_SESSION_TYPE").ok();
        let prev_display = std::env::var("WAYLAND_DISPLAY").ok();

        std::env::set_var("XDG_SESSION_TYPE", "wayland");
        std::env::remove_var("WAYLAND_DISPLAY");
        assert_eq!(resolve_driver(DriverKind::Auto), DriverKind::Evdev);

        std::env::set_var("XDG_SESSION_TYPE", "x11");
        std::env::remove_var("WAYLAND_DISPLAY");
        assert_eq!(resolve_driver(DriverKind::Auto), DriverKind::Rdev);

        // WAYLAND_DISPLAY alone also counts as Wayland (some launchers
        // don't set XDG_SESSION_TYPE).
        std::env::remove_var("XDG_SESSION_TYPE");
        std::env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert_eq!(resolve_driver(DriverKind::Auto), DriverKind::Evdev);

        match prev_type {
            Some(v) => std::env::set_var("XDG_SESSION_TYPE", v),
            None => std::env::remove_var("XDG_SESSION_TYPE"),
        }
        match prev_display {
            Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
            None => std::env::remove_var("WAYLAND_DISPLAY"),
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn resolve_driver_auto_is_rdev_on_non_linux() {
        // Windows / macOS have no evdev to fall back on — Auto must always
        // resolve to rdev regardless of any XDG env leakage.
        assert_eq!(resolve_driver(DriverKind::Auto), DriverKind::Rdev);
    }

    #[test]
    fn driver_from_env_reads_env_var() {
        // End-to-end env-var round-trip. Uses the crate lock so we don't
        // race the parse-only tests in the same binary.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("VOICEPI_HOTKEY_DRIVER").ok();

        std::env::remove_var("VOICEPI_HOTKEY_DRIVER");
        assert_eq!(driver_from_env(), DriverKind::Auto);

        std::env::set_var("VOICEPI_HOTKEY_DRIVER", "evdev");
        assert_eq!(driver_from_env(), DriverKind::Evdev);

        std::env::set_var("VOICEPI_HOTKEY_DRIVER", "rdev");
        assert_eq!(driver_from_env(), DriverKind::Rdev);

        std::env::set_var("VOICEPI_HOTKEY_DRIVER", "not-a-driver");
        // Unknown value falls back to Auto — a typo must not park PTT on
        // an accidentally-picked backend.
        assert_eq!(driver_from_env(), DriverKind::Auto);

        match prev {
            Some(v) => std::env::set_var("VOICEPI_HOTKEY_DRIVER", v),
            None => std::env::remove_var("VOICEPI_HOTKEY_DRIVER"),
        }
    }
}
