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

#[cfg(feature = "rust-hotkeys")]
pub mod driver_common;

#[cfg(feature = "rust-hotkeys")]
pub mod rdev_driver;

// evdev backend is Linux-only — it reads `/dev/input` directly, which is the
// only listener that works under Wayland (rdev's X11 XRecord is deaf there).
#[cfg(all(feature = "rust-hotkeys", target_os = "linux"))]
pub mod evdev_driver;

// Re-export the always-compiled tracker types at the manager level so call
// sites can keep using `manager::KeyTracker` / `manager::RawKeyEvent` etc.
// without caring about the sub-module split.
pub use tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput, FOREIGN_KEY_EXPIRY};

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
/// Parsed from `VOICEPI_HOTKEY_DRIVER=auto|rdev|evdev` in [`driver_from_env`]
/// and from the `--driver` flag in the `whisper-dictate hotkey capture` CLI
/// (which sets the env var before calling into `install_hotkey`).
#[cfg(feature = "rust-hotkeys")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind {
    /// Auto-detect per session: evdev on Linux Wayland, rdev everywhere else.
    Auto,
    /// Force rdev (X11 on Linux, WH_KEYBOARD_LL on Windows, CGEventTap on
    /// macOS). On Linux Wayland this listener is deaf — reported as a
    /// startup failure via `SpawnError::ListenerStartup`.
    Rdev,
    /// Force evdev (Linux only). On non-Linux targets [`spawn_with_raw_tap`]
    /// falls back to rdev with a warning; the caller sees the rdev name in
    /// the install envelope.
    Evdev,
}

#[cfg(feature = "rust-hotkeys")]
impl DriverKind {
    /// Parse a `VOICEPI_HOTKEY_DRIVER` / `--driver` value. Returns `None` for
    /// unrecognised values so callers can fall back to `Auto` instead of
    /// hard-erroring on a typo. Accepts `x11` as a friendly alias for `rdev`
    /// (both mean "the X11-style global hook" on Linux) and `wayland` as an
    /// alias for `evdev` (the only Wayland-capable backend).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "rdev" | "x11" => Some(Self::Rdev),
            "evdev" | "wayland" => Some(Self::Evdev),
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
/// The `Evdev` variant is Linux-only. On non-Linux targets it silently falls
/// back to rdev — the caller sees `"rdev"` in the returned name.
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
        // `Auto` is resolved by `resolve_driver` — should never reach here.
        _ => spawn_rdev(injection_guard, on_output, raw_tap),
    }
}

/// Resolve `Auto` to the backend that fits the current session. On non-Linux
/// targets `Auto` is always rdev (there is no evdev to fall back on). `Rdev`
/// and `Evdev` are returned unchanged so an explicit override wins over the
/// auto-detect.
#[cfg(feature = "rust-hotkeys")]
fn resolve_driver(kind: DriverKind) -> DriverKind {
    match kind {
        DriverKind::Rdev | DriverKind::Evdev => kind,
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

#[cfg(all(test, feature = "rust-hotkeys"))]
mod tests {
    use super::*;

    #[test]
    fn driver_kind_parse_accepts_canonical_names() {
        assert_eq!(DriverKind::parse("auto"), Some(DriverKind::Auto));
        assert_eq!(DriverKind::parse("rdev"), Some(DriverKind::Rdev));
        assert_eq!(DriverKind::parse("evdev"), Some(DriverKind::Evdev));
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
    fn resolve_driver_passes_explicit_choice_unchanged() {
        // An explicit override MUST NOT be overridden by session detection.
        // Otherwise the CLI's `--driver evdev` on X11 would silently pick
        // rdev anyway, defeating the escape hatch.
        assert_eq!(resolve_driver(DriverKind::Rdev), DriverKind::Rdev);
        assert_eq!(resolve_driver(DriverKind::Evdev), DriverKind::Evdev);
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
