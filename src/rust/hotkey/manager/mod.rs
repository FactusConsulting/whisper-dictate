//! Hotkey manager — owns the `rdev` global key-event listener in its own
//! thread and translates raw OS key events into the side-aware press /
//! release / cancel signals the coordinator consumes.
//!
//! Two layers, separated so the bulk is testable without `rdev` and so the
//! production logic stays within the repo-wide ≤500-LOC per file rule
//! (`AGENTS.md`):
//!
//! * [`tracker`] (always compiled) — a pure state machine that takes a
//!   stream of [`RawKeyEvent`]s and the user's PTT binding and emits
//!   [`TrackerOutput`]s (`ChordPress`, `ChordRelease`, `ChordCancel`).
//!   Holds the side-aware target/foreign membership using
//!   [`super::modifier_match::modifier_matches`] and the rising-edge latch
//!   so key-repeat never re-fires a press. Mirrors the Python
//!   `_PynputListener` semantics so behaviour is preserved.
//!
//! * [`rdev_driver`] (`#[cfg(feature = "rust-hotkeys")]`) — translates the
//!   platform `rdev::Event` into a [`RawKeyEvent`] and feeds the tracker.
//!   `rdev::listen()` is blocking and its native handles are not `Send` /
//!   `Sync`, hence the dedicated thread + mpsc-command API for register /
//!   unregister so the rest of the runtime can talk to it without touching
//!   the raw listener. Surfaces startup failures (no X display, missing
//!   accessibility permission, ...) to the caller of [`spawn`] so the
//!   supervisor can fall back to Python.

pub mod tracker;

#[cfg(feature = "rust-hotkeys")]
pub mod rdev_driver;

// Re-export the always-compiled tracker types at the manager level so call
// sites can keep using `manager::KeyTracker` / `manager::RawKeyEvent` etc.
// without caring about the sub-module split.
pub use tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput, FOREIGN_KEY_EXPIRY};

#[cfg(feature = "rust-hotkeys")]
pub use rdev_driver::{is_rdev_supported_name, spawn, ManagerHandle, ManagerThread, SpawnError};
