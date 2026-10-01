//! `wd hotkey capture` — diagnostic CLI that installs the PTT
//! listener for a bounded window, prints every OS key event and every
//! chord-level lifecycle transition the coordinator emits, then exits.
//!
//! Serves three purposes:
//!
//! * debugging PTT wedges (`does the listener see my chord at all?`),
//! * verifying the hotkey install path works on the running platform, and
//! * headless smoke-testing that the listener installs without crashing
//!   (`--for 0.5` in the wayland-user-smoke script — see audit item 2).
//!
//! The plain-text output is line-oriented for grep-ability; `--json` switches
//! to JSONL so callers can pin against a stable schema. Both formats route
//! through the same [`CaptureEvent`] value type so the formatter is a pure
//! function — see [`format_plain`] / [`format_json`] and their unit tests.
//!
//! The command deliberately does NOT modify `runtime.rs` — it goes straight
//! to [`super::install_hotkey_with_focus_snapshot`], whose shared install
//! funnel is the same surface the native runtime uses under the hood. That
//! keeps the diagnostic and the shipping path in lockstep without a shim.

#[path = "capture/actions.rs"]
mod actions;
#[path = "capture/command.rs"]
mod command;
#[path = "capture/events.rs"]
mod events;
#[path = "capture/raw_tap.rs"]
mod raw_tap;

pub use command::{handle_hotkey_command, validate_driver_flag};
pub(crate) use events::format_captured_chord;
pub use events::{format_json, format_plain, CaptureEvent, OUTPUT_PREFIX};
