//! `evdev` driver layer — the Linux/Wayland global key listener.
//!
//! ## Why this exists
//!
//! [`super::rdev_driver`] speaks X11 (XRecord). Under Wayland the compositor
//! delivers key events straight to the focused Wayland client and never routes
//! them through XWayland's record extension, so the rdev listener is deaf —
//! PTT chords silently never fire. The deleted Python backend used `evdev` for
//! exactly this reason; this driver restores that path in Rust.
//!
//! We read the kernel input devices under `/dev/input/event*` directly (no
//! X server, no compositor cooperation), which behaves identically on X11 and
//! Wayland. The trade-off is that the user must be able to read those nodes —
//! on a stock desktop that means membership of the `input` group. When no node
//! is readable, [`spawn`] returns [`SpawnError::ListenerStartup`] with a hint,
//! and the worker logs it just like an rdev startup failure.
//!
//! ## Threads
//!
//! One blocking reader thread per keyboard device (there are usually one or
//! two), each translating `evdev` events into [`RawKeyEvent`]s and feeding the
//! shared `Mutex<KeyTracker>`, plus the driver-agnostic manager thread from
//! [`super::driver_common`]. Like rdev's `listen()`, `evdev`'s blocking
//! `fetch_events()` cannot be interrupted, so the reader threads leak on
//! shutdown — acceptable because the subsystem is installed once per process.
//!
//! ## Hotplug
//!
//! Devices are enumerated once at [`spawn`] time. A keyboard plugged in later
//! is not picked up until the next worker restart; this matches the practical
//! behaviour of the old Python listener and keeps the driver simple. (A
//! `/dev/input` inotify watch could lift this later.)

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use evdev::{Device, EventType, Key};

use super::driver_common::{manager_channel, spawn_manager_thread, ManagerHandle, ManagerThread};
use super::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};

pub use super::driver_common::SpawnError;

/// Spawn the manager thread plus one blocking `evdev` reader thread per
/// keyboard device. Every tracker output produced by a real key event is
/// dispatched to `on_output`, which the coordinator hooks up to its
/// press/release/cancel events.
///
/// Returns `Err(SpawnError::ListenerStartup)` when no readable keyboard node
/// is found under `/dev/input` — typically a permissions problem (the user is
/// not in the `input` group). The message is surfaced to the worker so the
/// user gets an actionable hint instead of a silently-dead hotkey.
pub fn spawn<F>(on_output: F) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    let devices = open_keyboards();
    if debug_enabled() {
        eprintln!("[hotkey] evdev matched {} input device(s):", devices.len());
        for (path, dev) in &devices {
            eprintln!("[hotkey]   {} — {:?}", path.display(), dev.name());
        }
    }
    if devices.is_empty() {
        return Err(SpawnError::ListenerStartup(
            "no readable keyboard found under /dev/input (add your user to the `input` group \
             and re-log, or run `sudo usermod -aG input $USER`)"
                .to_owned(),
        ));
    }

    let (handle, cmd_rx) = manager_channel();
    let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
    let on_output = Arc::new(on_output);

    for (path, device) in devices {
        let reader_tracker = Arc::clone(&tracker);
        let reader_sink = Arc::clone(&on_output);
        thread::Builder::new()
            .name("vp-hotkey-evdev".to_owned())
            .spawn(move || reader_loop(path, device, reader_tracker, reader_sink))
            .map_err(|e| {
                SpawnError::ListenerStartup(format!("evdev reader thread spawn failed: {e}"))
            })?;
    }

    let manager_thread = spawn_manager_thread(cmd_rx, Arc::clone(&tracker))?;
    Ok((handle, manager_thread))
}

/// True when `VOICEPI_HOTKEY_DEBUG` is set to a non-empty, non-`0` value —
/// gates the opt-in device-list and raw-event traces used to diagnose a
/// silent-PTT report without a rebuild.
fn debug_enabled() -> bool {
    std::env::var("VOICEPI_HOTKEY_DEBUG")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false)
}

/// Enumerate `/dev/input/event*` and keep the nodes we can read that expose a
/// key some PTT binding could use. `evdev::enumerate` silently skips nodes it
/// cannot `open` (permission denied), so an empty result means "no readable
/// input device" — which `spawn` turns into an actionable error.
fn open_keyboards() -> Vec<(PathBuf, Device)> {
    evdev::enumerate()
        .filter(|(_, dev)| is_ptt_capable(dev))
        .collect()
}

/// A device is relevant if it supports `EV_KEY` and exposes at least one key
/// that [`code_to_name`] maps (any modifier, F-key, space/esc/tab/enter) OR a
/// letter (a full keyboard). Filtering on the *mapped* keys — rather than only
/// probing for Ctrl/letters — means a function-key-only macro pad or foot pedal
/// used for an `f9`/`f12` binding is still picked up (Codex #462 P2), while
/// mice, power buttons, and lid switches (no mapped keys) are still excluded.
fn is_ptt_capable(dev: &Device) -> bool {
    if !dev.supported_events().contains(EventType::KEY) {
        return false;
    }
    let Some(keys) = dev.supported_keys() else {
        return false;
    };
    keys.contains(Key::KEY_A) || keys.iter().any(|k| code_to_name(k.code()).is_some())
}

/// Blocking read loop for one device. Translates each key event into a
/// [`RawKeyEvent`], pushes it through the shared tracker, and forwards any
/// resulting [`TrackerOutput`] to the sink. Exits (letting the thread die) if
/// the device disappears — other reader threads keep running.
fn reader_loop(
    path: PathBuf,
    mut device: Device,
    tracker: Arc<Mutex<KeyTracker>>,
    sink: Arc<dyn Fn(TrackerOutput) + Send + Sync>,
) {
    // Opt-in raw-event trace: `VOICEPI_HOTKEY_DEBUG=1` logs every key event this
    // reader sees, so a silent-PTT report can be diagnosed to either "no events
    // arrive" (device/permission) or "events arrive but chord never completes"
    // (binding/tracker) without a rebuild.
    let debug = debug_enabled();
    loop {
        let events = match device.fetch_events() {
            Ok(events) => events,
            Err(err) => {
                eprintln!(
                    "[hotkey] evdev reader for {} stopped: {err}",
                    path.display()
                );
                return;
            }
        };
        for ev in events {
            let Some(raw) = raw_from_evdev(&ev) else {
                continue;
            };
            if debug {
                eprintln!(
                    "[hotkey] evdev raw {}: {} {:?}",
                    path.display(),
                    raw.name,
                    raw.kind
                );
            }
            let mut t = tracker.lock().expect("tracker poisoned");
            if let Some(out) = t.handle(&raw) {
                sink(out);
            }
        }
    }
}

/// Convert an `evdev::InputEvent` into a [`RawKeyEvent`]. Returns `None` for
/// non-key events (SYN/MSC/LED/…). A key we don't PTT-map still becomes a
/// synthetic `__evdev_<code>` name so the tracker's bare-modifier rule 1/2
/// (foreign-key held / foreign-key joins) works exactly as under rdev.
fn raw_from_evdev(ev: &evdev::InputEvent) -> Option<RawKeyEvent> {
    if ev.event_type() != EventType::KEY {
        return None;
    }
    let kind = match ev.value() {
        // 1 = press, 2 = key-repeat. rdev delivers repeats as KeyPress too, and
        // the tracker uses the repeat to suppress double-fires and refresh the
        // foreign-key self-heal timer — so both map to Press.
        1 | 2 => RawKeyKind::Press,
        0 => RawKeyKind::Release,
        _ => return None,
    };
    let name = code_to_name(ev.code())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("__evdev_{}", ev.code()));
    Some(RawKeyEvent {
        name,
        kind,
        at: Instant::now(),
    })
}

/// Map a Linux `input-event-codes.h` keycode to the lowercase name convention
/// the PTT settings and the tracker use (`ctrl_l`, `shift_r`, `alt_gr`, `f9`,
/// …). Mirrors [`super::rdev_driver::key_to_name`] one-for-one so a binding
/// behaves identically whichever backend is active. Unmapped codes return
/// `None` (handled as foreign keys by the caller).
fn code_to_name(code: u16) -> Option<&'static str> {
    // Numeric constants (not the `Key::` enum) so this table reads like the
    // kernel header and never depends on the evdev crate's naming.
    let name = match code {
        29 => "ctrl_l",  // KEY_LEFTCTRL
        97 => "ctrl_r",  // KEY_RIGHTCTRL
        42 => "shift_l", // KEY_LEFTSHIFT
        54 => "shift_r", // KEY_RIGHTSHIFT
        56 => "alt_l",   // KEY_LEFTALT
        100 => "alt_gr", // KEY_RIGHTALT
        125 => "cmd_l",  // KEY_LEFTMETA
        126 => "cmd_r",  // KEY_RIGHTMETA
        59 => "f1",      // KEY_F1
        60 => "f2",
        61 => "f3",
        62 => "f4",
        63 => "f5",
        64 => "f6",
        65 => "f7",
        66 => "f8",
        67 => "f9",
        68 => "f10",
        87 => "f11",
        88 => "f12",
        57 => "space", // KEY_SPACE
        1 => "esc",    // KEY_ESC
        15 => "tab",   // KEY_TAB
        28 => "enter", // KEY_ENTER
        _ => return None,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_to_name_covers_default_modifiers() {
        // The shipping default binding is shift_r+ctrl_r; the config that
        // triggered this fix is shift_l+ctrl_l. Both sides must resolve.
        assert_eq!(code_to_name(29), Some("ctrl_l"));
        assert_eq!(code_to_name(97), Some("ctrl_r"));
        assert_eq!(code_to_name(42), Some("shift_l"));
        assert_eq!(code_to_name(54), Some("shift_r"));
    }

    #[test]
    fn code_to_name_matches_rdev_name_set() {
        // Every name evdev can emit must also be an rdev-supported name, so
        // the install-time validator (which is shared) never rejects a key
        // this backend produces.
        for code in [
            29, 97, 42, 54, 56, 100, 125, 126, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 87, 88, 57,
            1, 15, 28,
        ] {
            let name = code_to_name(code).expect("mapped code");
            assert!(
                super::super::rdev_driver::is_rdev_supported_name(name),
                "{name} (code {code}) must be in the shared supported-name set"
            );
        }
    }

    #[test]
    fn unmapped_code_is_none() {
        // KEY_A (30) is a foreign key for PTT purposes — the caller turns a
        // None into a synthetic `__evdev_30` name for rule 1/2.
        assert_eq!(code_to_name(30), None);
    }

    #[test]
    fn key_repeat_value_maps_to_press() {
        // value == 2 is an OS key-repeat; it must arrive as Press so the
        // tracker refreshes the self-heal timer (matching rdev).
        let ev = evdev::InputEvent::new(EventType::KEY, 29, 2);
        let raw = raw_from_evdev(&ev).expect("key event");
        assert_eq!(raw.kind, RawKeyKind::Press);
        assert_eq!(raw.name, "ctrl_l");
    }

    #[test]
    fn non_key_event_is_ignored() {
        let ev = evdev::InputEvent::new(EventType::SYNCHRONIZATION, 0, 0);
        assert!(raw_from_evdev(&ev).is_none());
    }
}
