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
//! * [`driver_common`] — the backend-agnostic half: the `ManagerHandle` /
//!   `ManagerThread` / `SpawnError` contract and the manager thread that
//!   swaps the active binding via an mpsc command API (the OS listener runs
//!   on its own thread with non-`Send` handles, so the rest of the runtime
//!   talks to it only through this channel).
//!
//! * [`rdev_driver`] / [`evdev_driver`] (`#[cfg(feature = "rust-hotkeys")]`) —
//!   the two platform listeners. rdev drives X11 / Windows / macOS via the
//!   global hook; evdev reads `/dev/input` directly on Linux, the only path
//!   that observes global keys under a Wayland compositor (rdev's X11 XRecord
//!   is deaf there). [`spawn`] picks between them per session — see its docs.
//!   Both surface startup failures (no X display / missing accessibility
//!   permission for rdev; no readable keyboard node for evdev) to the caller.

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

#[cfg(feature = "rust-hotkeys")]
pub use driver_common::{ManagerHandle, ManagerThread, SpawnError};

#[cfg(feature = "rust-hotkeys")]
pub use rdev_driver::is_rdev_supported_name;

/// Spawn the OS key-event listener, picking the backend that actually works on
/// the running session:
///
/// * **Linux + Wayland** → [`evdev_driver`] (reads `/dev/input` directly; the
///   only path that sees global keys under a Wayland compositor).
/// * **Linux + X11**, **Windows**, **macOS** → [`rdev_driver`] (the global
///   hook / XRecord path).
///
/// The choice can be forced on Linux with `VOICEPI_HOTKEY_LINUX=evdev|rdev`
/// (also accepts `x11` as an alias for `rdev`) for debugging or as an escape
/// hatch. Everything downstream ([`ManagerHandle`], the tracker, the
/// coordinator) is backend-agnostic, so callers never care which fired.
#[cfg(feature = "rust-hotkeys")]
pub fn spawn<F>(on_output: F) -> std::result::Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    #[cfg(target_os = "linux")]
    {
        if use_evdev() {
            return evdev_driver::spawn(on_output);
        }
    }
    rdev_driver::spawn(on_output)
}

/// Decide whether the evdev backend should be used on Linux. Honours an
/// explicit `VOICEPI_HOTKEY_LINUX` override, otherwise auto-selects evdev for
/// Wayland sessions (where rdev cannot observe global keys).
#[cfg(all(feature = "rust-hotkeys", target_os = "linux"))]
fn use_evdev() -> bool {
    if let Ok(v) = std::env::var("VOICEPI_HOTKEY_LINUX") {
        match v.trim().to_ascii_lowercase().as_str() {
            "evdev" => return true,
            "rdev" | "x11" => return false,
            // Unknown value: fall through to auto-detection.
            _ => {}
        }
    }
    is_wayland_session()
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
