//! `evdev` driver layer — the Linux/Wayland global key listener.
//!
//! ## Why this exists
//!
//! [`super::rdev_driver`] speaks X11 (XRecord). Under Wayland the compositor
//! delivers key events straight to the focused Wayland client and never routes
//! them through XWayland's record extension, so the rdev listener is deaf —
//! PTT chords silently never fire. The deleted Python backend used `evdev` for
//! exactly this reason; this driver restores that path in Rust (audit item 5
//! prereq 2 — Wayland PTT missing evdev listener, regressed in v1.20.1 #462).
//!
//! We read the kernel input devices under `/dev/input/event*` directly (no
//! X server, no compositor cooperation), which behaves identically on X11 and
//! Wayland. The trade-off is that the user must be able to read those nodes —
//! on a stock desktop that means membership of the `input` group. When no node
//! is readable, [`spawn`] returns [`SpawnError::ListenerStartup`] with a hint,
//! and the worker logs it just like an rdev startup failure.
//!
//! ## ydotool / injection self-feedback exclusion (audit item 5 prereq 3)
//!
//! whisper-dictate's Wayland text injector types transcribed text through the
//! `ydotool` client, whose `ydotoold` daemon exposes a virtual keyboard node
//! under `/dev/input` ("ydotoold virtual device"). If the PTT listener reads
//! that node too, every injected keystroke feeds back into the tracker: for a
//! bare-modifier chord the typed characters look like foreign keys and trip
//! the "foreign key held" guard (rule 1), silently blocking the NEXT push-to-
//! talk for ~10 s. That's the exact v1.20.2 wedge #467 fixed at device-
//! enumeration level; the fix is baked in here from the start (see
//! [`INJECTION_DEVICE_MARKERS`] and [`is_injection_device`]) so this listener
//! cannot re-open the loop.
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
//!
//! ## Raw tap
//!
//! Mirrors [`super::rdev_driver::RawTap`] one-for-one so the diagnostic
//! `whisper-dictate hotkey capture` CLI can observe every keydown/keyup this
//! driver emits, not just chord-level output. The tap runs on the reader
//! thread for the device that emitted the event; keep it cheap and non-
//! blocking (any latency here starves the coordinator).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

// evdev 0.13 renamed the `Key` type to `KeyCode`. `EventType` is unchanged and
// its `KEY` associated constant is a tuple-struct with a `.0: u16` field —
// used below when constructing synthetic `InputEvent`s for the unit tests
// (`InputEvent::new` now takes a `u16` type/code, not the `EventType` newtype).
use evdev::{Device, EventType, KeyCode};

use super::driver_common::{manager_channel, spawn_manager_thread};
use super::tracker::{KeyTracker, RawKeyEvent, RawKeyKind, TrackerOutput};
use crate::hotkey::inject_guard::{dispatch_raw_event, InjectionGuard};

pub use super::driver_common::{ManagerHandle, ManagerThread, NoopRawTap, RawTap, SpawnError};

/// Spawn the manager thread plus one blocking `evdev` reader thread per
/// keyboard device. Every tracker output produced by a real key event is
/// dispatched to `on_output`, which the coordinator hooks up to its
/// press/release/cancel events.
///
/// Returns `Err(SpawnError::ListenerStartup)` when no readable keyboard node
/// is found under `/dev/input` — typically a permissions problem (the user is
/// not in the `input` group). The message is surfaced to the worker so the
/// user gets an actionable hint instead of a silently-dead hotkey.
///
/// `injection_guard` is a defense-in-depth second layer for self-injection
/// feedback: the PRIMARY defense on evdev is the device-enumeration
/// exclusion (`INJECTION_DEVICE_MARKERS`) — an injected keystroke never
/// even reaches this reader. The guard added by #507 to close the Windows
/// wedge is threaded through here too so a future injection path that
/// bypasses `/dev/input` (e.g. libei / portal-based emitters) is still
/// filtered by the same mechanism as rdev. Fast path is a single atomic
/// load per event.
#[cfg(test)]
pub fn spawn<F>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
{
    spawn_with_raw_tap(injection_guard, on_output, NoopRawTap)
}

/// Same as [`spawn`] but also invokes `raw_tap` for every raw key event
/// BEFORE the tracker sees it (and before the injection guard's check —
/// the diagnostic `hotkey capture` CLI still sees suppressed events, only
/// the tracker is shielded). The tap runs on the per-device reader thread —
/// keep it cheap and non-blocking (long work will delay the tracker and
/// starve the coordinator).
pub fn spawn_with_raw_tap<F, R>(
    injection_guard: Arc<InjectionGuard>,
    on_output: F,
    raw_tap: R,
) -> Result<(ManagerHandle, ManagerThread), SpawnError>
where
    F: Fn(TrackerOutput) + Send + Sync + 'static,
    R: RawTap,
{
    let EnumeratedDevices {
        kept: devices,
        excluded,
    } = enumerate_devices();
    if debug_enabled() {
        eprintln!("[hotkey] evdev matched {} input device(s):", devices.len());
        for (path, dev) in &devices {
            eprintln!("[hotkey]   {} — {:?}", path.display(), dev.name());
        }
        if !excluded.is_empty() {
            eprintln!(
                "[hotkey] evdev excluded {} injection device(s):",
                excluded.len()
            );
            for (path, name) in &excluded {
                eprintln!(
                    "[hotkey]   {} — {name:?} (injection self-feedback)",
                    path.display()
                );
            }
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
    let raw_tap = Arc::new(raw_tap);

    for (path, device) in devices {
        let reader_tracker = Arc::clone(&tracker);
        let reader_sink = Arc::clone(&on_output);
        let reader_tap = Arc::clone(&raw_tap);
        let reader_guard = Arc::clone(&injection_guard);
        thread::Builder::new()
            .name("vp-hotkey-evdev".to_owned())
            .spawn(move || {
                reader_loop(
                    path,
                    device,
                    reader_tracker,
                    reader_sink,
                    reader_tap,
                    reader_guard,
                )
            })
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

/// Result of the one-shot `/dev/input` device enumeration `spawn` performs.
/// `kept` is the set of readable keyboard nodes we will attach a reader
/// thread to; `excluded` is the (path, name) list of injection uinput
/// devices we deliberately filtered out (see [`is_injection_device`]).
/// Split into a named struct to keep clippy's type-complexity lint happy
/// AND to make the callsite read as intent, not as a raw tuple projection.
struct EnumeratedDevices {
    kept: Vec<(PathBuf, Device)>,
    excluded: Vec<(PathBuf, String)>,
}

/// Enumerate `/dev/input/event*` and partition into kept / excluded.
/// `evdev::enumerate` silently skips nodes it cannot `open` (permission
/// denied), so an empty `kept` result means "no readable input device" —
/// which `spawn` turns into an actionable error. The `excluded` list carries
/// the (path, name) of any injection uinput devices we explicitly filtered
/// out so `VOICEPI_HOTKEY_DEBUG=1` can surface WHY the ydotoold node was
/// dropped — invaluable when diagnosing an "injection fed back into PTT"
/// report.
fn enumerate_devices() -> EnumeratedDevices {
    let mut kept = Vec::new();
    let mut excluded = Vec::new();
    for (path, dev) in evdev::enumerate() {
        if !is_ptt_capable(&dev) {
            continue;
        }
        if is_injection_device(&dev) {
            let name = dev.name().unwrap_or("<unnamed>").to_owned();
            excluded.push((path, name));
            continue;
        }
        kept.push((path, dev));
    }
    EnumeratedDevices { kept, excluded }
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
    keys.contains(KeyCode::KEY_A) || keys.iter().any(|k| code_to_name(k.code()).is_some())
}

/// Names (case-insensitive substrings) of the virtual uinput devices that
/// whisper-dictate's OWN text injectors create. On Wayland the app types the
/// transcribed text through `ydotool` (`injection::wayland`), whose `ydotoold`
/// daemon exposes a "ydotoold virtual device" under `/dev/input`. If the hotkey
/// listener reads that node, every injected keystroke feeds back into the PTT
/// [`crate::hotkey::manager::tracker`]: for a bare-modifier chord the typed
/// characters look like foreign keys and trip the "foreign key held" guard
/// (rule 1), so the SECOND push-to-talk after a transcription is silently
/// blocked until the stale key expires (~10 s). Excluding these self-injection
/// nodes breaks the feedback loop. `enigo`'s X11/Wayland uinput node is listed
/// too for the `rust-injection` path.
///
/// Baked into the driver at introduction (audit item 5 prereq 3) so the fix
/// v1.20.2 shipped and the v1.21.0 reset discarded (#467) cannot regress.
pub(crate) const INJECTION_DEVICE_MARKERS: &[&str] =
    &["ydotool", "wtype", "dotool", "kwtype", "enigo"];

/// True if `dev` is one of whisper-dictate's own injection uinput devices — see
/// [`INJECTION_DEVICE_MARKERS`]. Matched by device name so a genuine keyboard
/// is never excluded (no real keyboard carries these tool names).
fn is_injection_device(dev: &Device) -> bool {
    dev.name().is_some_and(name_is_injection_device)
}

/// Pure name test behind [`is_injection_device`], split out so it is unit
/// testable without constructing an `evdev::Device`.
pub(crate) fn name_is_injection_device(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    INJECTION_DEVICE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Blocking read loop for one device. Translates each key event into a
/// [`RawKeyEvent`], pushes it through the shared tracker (after the tap has
/// observed the raw event AND the injection guard has decided the event is
/// not the app's own SendInput / equivalent burst), and forwards any
/// resulting [`TrackerOutput`] to the sink. Exits (letting the thread die)
/// if the device disappears — other reader threads keep running.
fn reader_loop(
    path: PathBuf,
    mut device: Device,
    tracker: Arc<Mutex<KeyTracker>>,
    sink: Arc<dyn Fn(TrackerOutput) + Send + Sync>,
    tap: Arc<dyn RawTap>,
    injection_guard: Arc<InjectionGuard>,
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
            tap.tap(&raw);
            // Belt-and-braces: the primary defense against injection self-
            // feedback on evdev is the device-enumeration exclusion
            // (`INJECTION_DEVICE_MARKERS`) — an injected keystroke never
            // reaches this reader. The injection guard is a second layer
            // that also filters events on the rdev path (#507) and covers
            // future emitters that bypass `/dev/input` (libei / portals).
            // Fast path is one atomic load when the guard is inactive.
            let mut t = tracker.lock().expect("tracker poisoned");
            if let Some(out) = dispatch_raw_event(&injection_guard, &mut t, &raw) {
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
/// …). Mirrors [`super::rdev_driver::is_rdev_supported_name`]'s implicit map
/// one-for-one so a binding behaves identically whichever backend is active.
/// Unmapped codes return `None` (handled as foreign keys by the caller).
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

    // -----------------------------------------------------------------------
    // code_to_name — keycode → shared PTT name mapping
    // -----------------------------------------------------------------------

    #[test]
    fn code_to_name_covers_default_modifiers() {
        // The shipping default binding is shift_r+ctrl_r; the config that
        // triggered #467 (the ydotoold self-feedback wedge) is shift_l+ctrl_l.
        // Both sides must resolve.
        assert_eq!(code_to_name(29), Some("ctrl_l"));
        assert_eq!(code_to_name(97), Some("ctrl_r"));
        assert_eq!(code_to_name(42), Some("shift_l"));
        assert_eq!(code_to_name(54), Some("shift_r"));
    }

    #[test]
    fn code_to_name_matches_rdev_name_set() {
        // Every name evdev can emit must also be an rdev-supported name, so
        // the install-time validator (which is shared, gated by
        // `is_rdev_supported_name`) never rejects a key this backend produces.
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
        let ev = evdev::InputEvent::new(EventType::KEY.0, 29, 2);
        let raw = raw_from_evdev(&ev).expect("key event");
        assert_eq!(raw.kind, RawKeyKind::Press);
        assert_eq!(raw.name, "ctrl_l");
    }

    #[test]
    fn non_key_event_is_ignored() {
        let ev = evdev::InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0);
        assert!(raw_from_evdev(&ev).is_none());
    }

    #[test]
    fn unmapped_key_becomes_synthetic_evdev_name() {
        // KEY_A (30) has no PTT mapping. The raw event must still be
        // produced with a synthetic `__evdev_30` name so foreign-key rules
        // 1 and 2 in the tracker (bare-modifier cancel) still fire — this
        // mirrors the rdev `__rdev_KeyA` behaviour.
        let ev = evdev::InputEvent::new(EventType::KEY.0, 30, 1);
        let raw = raw_from_evdev(&ev).expect("key event");
        assert_eq!(raw.name, "__evdev_30");
        assert_eq!(raw.kind, RawKeyKind::Press);
    }

    #[test]
    fn release_value_zero_maps_to_release() {
        // Explicit press/release symmetry check — a keyup carries value 0
        // and must always be delivered so the tracker can drop the held key.
        let ev = evdev::InputEvent::new(EventType::KEY.0, 29, 0);
        let raw = raw_from_evdev(&ev).expect("key event");
        assert_eq!(raw.kind, RawKeyKind::Release);
        assert_eq!(raw.name, "ctrl_l");
    }

    // -----------------------------------------------------------------------
    // Injection-device exclusion — audit item 5 prereq 3
    // (ports the v1.20.2 #467 fix, baked in from introduction)
    // -----------------------------------------------------------------------

    #[test]
    fn injection_devices_are_excluded_by_name() {
        // The app types transcribed text through ydotool on Wayland; its
        // uinput node must NOT be read by the PTT listener, or every injected
        // keystroke feeds back into the tracker and wedges the next chord
        // (#467, silently blocked PTT after first transcription).
        assert!(name_is_injection_device("ydotoold virtual device"));
        assert!(name_is_injection_device("wtype"));
        assert!(name_is_injection_device("dotool keyboard"));
        assert!(name_is_injection_device("Enigo virtual device"));
        // kwtype is the KDE-native injector — same self-feedback risk.
        assert!(name_is_injection_device("kwtype virtual keyboard"));
    }

    #[test]
    fn injection_device_match_is_case_insensitive() {
        // Uppercase / mixed-case device names must still be excluded.
        // Some kernel drivers name the node "ydotoold" verbatim (lowercase),
        // but downstream tools sometimes wrap with title-cased suffixes.
        assert!(name_is_injection_device("YDOTOOLD"));
        assert!(name_is_injection_device("Ydotoold Virtual Keyboard"));
        assert!(name_is_injection_device("KWtype"));
    }

    #[test]
    fn injection_device_match_is_substring_not_exact() {
        // ydotoold's device name in practice is "ydotoold virtual device";
        // the marker "ydotool" (no trailing d) is a substring of that. The
        // matcher must accept substrings so a version bump that renames the
        // node ("ydotoold-v2 virtual device") does not silently unmask the
        // feedback loop.
        assert!(name_is_injection_device("ydotoold-v2 virtual device"));
        assert!(name_is_injection_device("prefix-wtype-suffix"));
    }

    #[test]
    fn real_keyboards_are_not_excluded() {
        // Genuine keyboards must survive the injection-device filter — no
        // production kernel driver ships with any of the marker substrings
        // in its device name.
        assert!(!name_is_injection_device("AT Translated Set 2 keyboard"));
        assert!(!name_is_injection_device("Raptor Lake-P/U/H cAVS HID"));
        assert!(!name_is_injection_device("Logitech USB Keyboard"));
        assert!(!name_is_injection_device(
            "Apple Internal Keyboard / Trackpad"
        ));
        assert!(!name_is_injection_device("Kinesis Advantage2"));
        // Neither a foot pedal nor a macro pad should trip the filter.
        assert!(!name_is_injection_device("VEC Footpedal USB"));
        assert!(!name_is_injection_device("Elgato Stream Deck"));
    }

    #[test]
    fn marker_list_has_no_empty_or_whitespace_entries() {
        // A stray `""` in the marker list would match every device name and
        // silently disable the entire evdev listener. Guard against that
        // regression by validating the constant.
        for marker in INJECTION_DEVICE_MARKERS {
            assert!(
                !marker.trim().is_empty(),
                "INJECTION_DEVICE_MARKERS entry must be a non-empty non-whitespace string"
            );
            // Markers are matched after `to_ascii_lowercase()` so they must
            // be pre-lowercased or the substring check would miss.
            assert_eq!(
                *marker,
                marker.to_ascii_lowercase(),
                "INJECTION_DEVICE_MARKERS entry {marker:?} must be lowercase"
            );
        }
    }

    #[test]
    fn injection_markers_cover_every_wayland_helper() {
        // The Wayland injection fallback chain in `injection::wayland` is
        // (in order): kwtype, wtype, dotool, ydotool. Each MUST have a
        // corresponding marker so its uinput node is filtered — otherwise
        // the same self-feedback bug re-appears when the fallback chain
        // resolves to a different tool (e.g. Sway where wtype wins). Also
        // covers `enigo` which the `rust-injection` X11/Wayland uinput
        // path uses.
        let markers: std::collections::HashSet<&str> =
            INJECTION_DEVICE_MARKERS.iter().copied().collect();
        for expected in ["ydotool", "wtype", "dotool", "kwtype", "enigo"] {
            assert!(
                markers.contains(expected),
                "INJECTION_DEVICE_MARKERS is missing {expected:?} — the corresponding \
                 injection tool's uinput node would feed back into PTT (regression of #467)"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Environment-driven debug flag
    // -----------------------------------------------------------------------

    #[test]
    fn debug_flag_reads_env_var() {
        // `debug_enabled` is process-wide (reads the env var directly) so
        // this test guards the parse rules ("1"/"true"/anything-non-empty
        // enables it; unset / empty / "0" disables). Uses the crate-wide
        // env lock so we don't race other env-mutating tests.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prev = std::env::var("VOICEPI_HOTKEY_DEBUG").ok();

        std::env::remove_var("VOICEPI_HOTKEY_DEBUG");
        assert!(!debug_enabled(), "unset must be disabled");

        std::env::set_var("VOICEPI_HOTKEY_DEBUG", "");
        assert!(!debug_enabled(), "empty must be disabled");

        std::env::set_var("VOICEPI_HOTKEY_DEBUG", "0");
        assert!(!debug_enabled(), "'0' must be disabled");

        std::env::set_var("VOICEPI_HOTKEY_DEBUG", "1");
        assert!(debug_enabled(), "'1' must be enabled");

        std::env::set_var("VOICEPI_HOTKEY_DEBUG", "yes");
        assert!(debug_enabled(), "any non-empty non-'0' must be enabled");

        match prev {
            Some(v) => std::env::set_var("VOICEPI_HOTKEY_DEBUG", v),
            None => std::env::remove_var("VOICEPI_HOTKEY_DEBUG"),
        }
    }
}
