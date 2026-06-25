//! Hotkey manager — owns the `rdev` global key-event listener in its own
//! thread and translates raw OS key events into the side-aware press / release
//! / cancel signals the coordinator consumes.
//!
//! Two layers, separated so the bulk is testable without `rdev`:
//!
//! * [`KeyTracker`] (this file, always compiled) — a pure state machine that
//!   takes a stream of [`RawKeyEvent`]s (the key name + press-or-release) and
//!   the user's PTT binding, and emits [`TrackerOutput`]s
//!   (`ChordPress`, `ChordRelease`, `ChordCancel`). Holds the side-aware
//!   target/foreign membership using
//!   [`super::modifier_match::modifier_matches`] and the rising-edge latch so
//!   key-repeat never re-fires a press. Mirrors the Python `_PynputListener`
//!   semantics so behaviour is preserved.
//!
//! * The `rdev` driver (`#[cfg(feature = "rust-hotkeys")]`, [`run_listener`]
//!   below) — translates the platform `rdev::Event` into a [`RawKeyEvent`] and
//!   feeds the tracker. `rdev::listen()` is blocking and its native handles
//!   are not `Send` / `Sync`, hence the dedicated thread + mpsc-command API
//!   for register / unregister so the rest of the runtime can talk to it
//!   without touching the raw listener.

use std::collections::{HashMap, HashSet};

use super::modifier_match::{
    all_targets_have_distinct_match, canonical_side, modifier_family, modifier_matches,
};

/// A single OS key event after name normalisation. Pure data; produced by the
/// `rdev` driver in production and by hand-written fixtures in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawKeyEvent {
    pub name: String,
    pub kind: RawKeyKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKeyKind {
    Press,
    Release,
}

/// What the tracker tells the coordinator about a stream of raw events. One
/// raw event can produce zero or one outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackerOutput {
    /// PTT chord just became complete (rising edge — never key-repeat).
    ChordPress,
    /// PTT chord just broke (falling edge).
    ChordRelease,
    /// A foreign key joined the held PTT modifier(s) — discard the in-flight
    /// recording. Mirrors the bare-modifier rule-2 path in vp_keys.py.
    ChordCancel,
}

/// Side-aware press/release tracker. Holds the SET of currently-pressed keys
/// (by normalised name) and the rising-edge latch. Clone-free,
/// allocation-light, no I/O — designed to be unit-tested with synthetic
/// [`RawKeyEvent`] streams.
pub struct KeyTracker {
    targets: Vec<String>,
    /// Pressed key name -> the canonical-side form recorded at press time.
    /// We keep the original name in the key so opposite-side releases don't
    /// collapse a still-held opposite side.
    pressed: HashMap<String, String>,
    chord_latched: bool,
    /// True iff the last ChordPress we emitted is still "in flight" — set on
    /// the press that actually emits ChordPress, cleared on the release that
    /// emits ChordRelease (or on a cancel). Distinct from `chord_latched`,
    /// which suppresses repeats whether or not we actually fired (rule 1
    /// blocks a start but still latches so the next repeat does not double-
    /// fire). Without this we'd emit a spurious ChordRelease for a press
    /// that was suppressed by rule 1.
    chord_emitted: bool,
    /// True when the binding is made up entirely of bare modifiers — the
    /// bare-modifier "press alone" rules apply (rule 1 + rule 2 in
    /// vp_keys_solo). When false, foreign keys are ignored.
    bare_modifier_binding: bool,
}

impl KeyTracker {
    /// Build a tracker for `targets` (the user's PTT setting, already split
    /// on `+`). Names use the same convention as the Python settings:
    /// `ctrl_l`, `shift_r`, `alt_gr`, `f9`, ...
    pub fn new(targets: Vec<String>) -> Self {
        let bare_modifier_binding =
            !targets.is_empty() && targets.iter().all(|n| modifier_family(n).is_some());
        Self {
            targets,
            pressed: HashMap::new(),
            chord_latched: false,
            chord_emitted: false,
            bare_modifier_binding,
        }
    }

    /// Process one OS event and return what (if anything) the coordinator
    /// should see.
    pub fn handle(&mut self, event: &RawKeyEvent) -> Option<TrackerOutput> {
        match event.kind {
            RawKeyKind::Press => self.handle_press(&event.name),
            RawKeyKind::Release => self.handle_release(&event.name),
        }
    }

    fn handle_press(&mut self, name: &str) -> Option<TrackerOutput> {
        // Key-repeat suppression: if we've already recorded this exact name as
        // pressed, it's an OS repeat. Update nothing; emit nothing.
        if self.pressed.contains_key(name) {
            return None;
        }
        self.pressed
            .insert(name.to_owned(), canonical_side(name).to_owned());

        let is_target = self.is_target(name);
        if !is_target {
            // Foreign key. Only meaningful if the binding is bare-modifier:
            // a fresh foreign press while we ACTUALLY emitted a ChordPress
            // for the held chord cancels. (If rule 1 had blocked the press
            // there is no recording to cancel — chord_emitted stays false.)
            if self.bare_modifier_binding && self.chord_emitted {
                self.chord_emitted = false;
                return Some(TrackerOutput::ChordCancel);
            }
            return None;
        }

        // Target press — check whether this completes the chord.
        if !self.chord_complete() {
            return None;
        }
        if self.chord_latched {
            return None; // already-fired chord, this is a stray repeat path
        }
        // Bare-modifier rule 1: refuse to start if any foreign key is held.
        if self.bare_modifier_binding && self.foreign_key_held() {
            self.chord_latched = true; // latch anyway so subsequent presses don't double-fire
            return None;
        }
        self.chord_latched = true;
        self.chord_emitted = true;
        Some(TrackerOutput::ChordPress)
    }

    fn handle_release(&mut self, name: &str) -> Option<TrackerOutput> {
        // Side-aware release clearing — mirrors held_keys_cleared_by_release
        // in vp_keys_solo.py so press/release pairs (ctrl_l down, generic
        // ctrl up) reconcile correctly.
        let family = modifier_family(name);
        let drop_names: Vec<String> = match family {
            None => self
                .pressed
                .keys()
                .filter(|k| k.as_str() == name)
                .cloned()
                .collect(),
            Some(_) => {
                let r_canonical = canonical_side(name);
                let generic_release = is_generic_modifier_name(name);
                self.pressed
                    .iter()
                    .filter(|(k, side)| {
                        modifier_family(k) == family
                            && (generic_release
                                || side.as_str() == r_canonical
                                || is_generic_modifier_name(k))
                    })
                    .map(|(k, _)| k.clone())
                    .collect()
            }
        };
        for k in &drop_names {
            self.pressed.remove(k);
        }
        if !self.is_target(name) {
            return None;
        }
        if !self.chord_complete() {
            self.chord_latched = false;
            if self.chord_emitted {
                self.chord_emitted = false;
                return Some(TrackerOutput::ChordRelease);
            }
        }
        None
    }

    fn is_target(&self, name: &str) -> bool {
        self.targets.iter().any(|t| modifier_matches(name, t))
    }

    fn chord_complete(&self) -> bool {
        let held: HashSet<String> = self.pressed.keys().cloned().collect();
        all_targets_have_distinct_match(&self.targets, &held)
    }

    fn foreign_key_held(&self) -> bool {
        self.pressed.keys().any(|k| !self.is_target(k))
    }
}

fn is_generic_modifier_name(name: &str) -> bool {
    matches!(name, "ctrl" | "shift" | "alt" | "cmd")
}

// ---------------------------------------------------------------------------
// rdev driver layer — only compiled when the `rust-hotkeys` feature is on.
// ---------------------------------------------------------------------------

#[cfg(feature = "rust-hotkeys")]
pub use rdev_driver::*;

#[cfg(feature = "rust-hotkeys")]
mod rdev_driver {
    use super::*;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    /// Commands the manager thread accepts on its inbound channel. Each
    /// carries a sync response sender so the caller can `recv()` confirmation
    /// from a non-listener thread; this is the "mpsc commands with sync
    /// response" pattern issue #318 calls out.
    pub enum ManagerCommand {
        Register {
            targets: Vec<String>,
            ack: Sender<Result<(), String>>,
        },
        Unregister {
            ack: Sender<Result<(), String>>,
        },
        Shutdown,
    }

    /// Public handle to the manager thread.
    #[derive(Clone)]
    pub struct ManagerHandle {
        tx: Sender<ManagerCommand>,
    }

    impl ManagerHandle {
        /// Install (or replace) the active PTT binding. Blocks until the
        /// manager thread acknowledges, but the underlying operation is
        /// cheap — just swapping a `Vec<String>` in a mutex.
        pub fn register(&self, targets: Vec<String>) -> Result<(), String> {
            let (ack_tx, ack_rx) = mpsc::channel();
            self.tx
                .send(ManagerCommand::Register {
                    targets,
                    ack: ack_tx,
                })
                .map_err(|e| format!("manager thread disconnected: {e}"))?;
            ack_rx
                .recv()
                .map_err(|e| format!("ack channel closed: {e}"))?
        }

        /// Stop emitting tracker outputs without tearing down the listener
        /// thread (the OS listener stays installed — `rdev` does not give us
        /// a clean per-binding teardown — but the tracker is replaced with
        /// an empty one so no events flow through).
        pub fn unregister(&self) -> Result<(), String> {
            let (ack_tx, ack_rx) = mpsc::channel();
            self.tx
                .send(ManagerCommand::Unregister { ack: ack_tx })
                .map_err(|e| format!("manager thread disconnected: {e}"))?;
            ack_rx
                .recv()
                .map_err(|e| format!("ack channel closed: {e}"))?
        }

        /// Ask the manager thread to exit. The OS-level `rdev::listen()`
        /// thread cannot be interrupted, so it leaks on shutdown — acceptable
        /// because the supervisor only ever installs the hotkey subsystem
        /// once and tears down on process exit.
        pub fn shutdown(&self) {
            let _ = self.tx.send(ManagerCommand::Shutdown);
        }
    }

    /// Owned join handle for the manager thread (NOT the inner rdev listener
    /// thread, which cannot be joined). Kept by the supervisor for cleanup.
    pub struct ManagerThread {
        join: Option<JoinHandle<()>>,
    }

    impl ManagerThread {
        pub fn join(mut self) {
            if let Some(h) = self.join.take() {
                let _ = h.join();
            }
        }
    }

    /// Spawn the manager thread plus the `rdev` listener thread. Every
    /// tracker output produced by a real OS key event is dispatched to
    /// `on_output`, which the coordinator hooks up to its press/release/cancel
    /// events.
    ///
    /// The `rdev` listener runs forever on its own thread (it has no API to
    /// stop). The manager thread is a thin layer on top that owns the shared
    /// `tracker` state via a `Mutex` and processes register/unregister
    /// commands.
    pub fn spawn<F>(on_output: F) -> (ManagerHandle, ManagerThread)
    where
        F: Fn(TrackerOutput) + Send + Sync + 'static,
    {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let tracker: Arc<Mutex<KeyTracker>> = Arc::new(Mutex::new(KeyTracker::new(Vec::new())));
        let on_output = Arc::new(on_output);

        // Listener thread — owns rdev. Translates raw events through the
        // shared tracker.
        let listener_tracker = Arc::clone(&tracker);
        let listener_sink = Arc::clone(&on_output);
        thread::Builder::new()
            .name("vp-hotkey-rdev".to_owned())
            .spawn(move || {
                let cb = move |event: rdev::Event| {
                    if let Some(raw) = raw_from_rdev(&event) {
                        let mut t = listener_tracker.lock().expect("tracker poisoned");
                        if let Some(out) = t.handle(&raw) {
                            (listener_sink)(out);
                        }
                    }
                };
                if let Err(err) = rdev::listen(cb) {
                    eprintln!("[hotkey] rdev listener failed: {err:?}");
                }
            })
            .expect("rdev listener thread spawn");

        let manager_tracker = Arc::clone(&tracker);
        let join = thread::Builder::new()
            .name("vp-hotkey-manager".to_owned())
            .spawn(move || manager_loop(cmd_rx, manager_tracker))
            .expect("hotkey manager thread spawn");

        (
            ManagerHandle { tx: cmd_tx },
            ManagerThread { join: Some(join) },
        )
    }

    fn manager_loop(rx: Receiver<ManagerCommand>, tracker: Arc<Mutex<KeyTracker>>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(ManagerCommand::Register { targets, ack }) => {
                    *tracker.lock().expect("tracker poisoned") = KeyTracker::new(targets);
                    let _ = ack.send(Ok(()));
                }
                Ok(ManagerCommand::Unregister { ack }) => {
                    *tracker.lock().expect("tracker poisoned") = KeyTracker::new(Vec::new());
                    let _ = ack.send(Ok(()));
                }
                Ok(ManagerCommand::Shutdown) => return,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Convert an `rdev::Event` into the platform-agnostic [`RawKeyEvent`]
    /// the tracker consumes. Returns `None` for events we don't care about
    /// (mouse, unhandled `Key::Unknown`).
    fn raw_from_rdev(event: &rdev::Event) -> Option<RawKeyEvent> {
        let (key, kind) = match event.event_type {
            rdev::EventType::KeyPress(k) => (k, RawKeyKind::Press),
            rdev::EventType::KeyRelease(k) => (k, RawKeyKind::Release),
            _ => return None,
        };
        let name = key_to_name(key)?;
        Some(RawKeyEvent { name, kind })
    }

    /// Map `rdev::Key` to the lowercase-name convention used by the Python
    /// PTT settings (`ctrl_l`, `shift_r`, `alt_gr`, `f9`, single chars, ...).
    /// Unmapped keys return `None` — they cannot be a PTT target so silently
    /// dropping them is fine.
    fn key_to_name(key: rdev::Key) -> Option<String> {
        use rdev::Key as K;
        let name = match key {
            K::ControlLeft => "ctrl_l",
            K::ControlRight => "ctrl_r",
            K::ShiftLeft => "shift_l",
            K::ShiftRight => "shift_r",
            K::Alt => "alt_l",
            K::AltGr => "alt_gr",
            K::MetaLeft => "cmd_l",
            K::MetaRight => "cmd_r",
            K::F1 => "f1",
            K::F2 => "f2",
            K::F3 => "f3",
            K::F4 => "f4",
            K::F5 => "f5",
            K::F6 => "f6",
            K::F7 => "f7",
            K::F8 => "f8",
            K::F9 => "f9",
            K::F10 => "f10",
            K::F11 => "f11",
            K::F12 => "f12",
            K::Space => "space",
            K::Escape => "esc",
            K::Tab => "tab",
            K::Return => "enter",
            _ => return None,
        };
        Some(name.to_owned())
    }

    #[cfg(test)]
    mod driver_tests {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        #[test]
        fn register_and_unregister_roundtrip() {
            // Lightweight test that register/unregister responses come back
            // through the mpsc — does NOT exercise the rdev listener thread
            // (it's still installed but no synthetic events are injected).
            let count = Arc::new(AtomicUsize::new(0));
            let count_cb = Arc::clone(&count);
            let (handle, _thread) = spawn(move |_out| {
                count_cb.fetch_add(1, Ordering::SeqCst);
            });
            handle
                .register(vec!["ctrl_l".to_owned(), "f9".to_owned()])
                .expect("register");
            handle.unregister().expect("unregister");
            handle
                .register(vec!["shift_r".to_owned()])
                .expect("re-register");
            // No events fired through the tracker — count stays zero.
            assert_eq!(count.load(Ordering::SeqCst), 0);
            handle.shutdown();
            // Do NOT join: the rdev listener thread is unjoinable, but the
            // manager thread is — drop the handle and let the test runner
            // finish. (The thread exits on its own when it sees Shutdown.)
        }
    }
}

#[cfg(test)]
mod tracker_tests {
    use super::*;

    fn press(name: &str) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Press,
        }
    }
    fn release(name: &str) -> RawKeyEvent {
        RawKeyEvent {
            name: name.to_owned(),
            kind: RawKeyKind::Release,
        }
    }

    #[test]
    fn solo_modifier_press_release_emits_chord() {
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl_l")),
            Some(TrackerOutput::ChordRelease)
        );
    }

    #[test]
    fn key_repeat_does_not_re_fire() {
        let mut t = KeyTracker::new(vec!["f9".to_owned()]);
        assert_eq!(t.handle(&press("f9")), Some(TrackerOutput::ChordPress));
        assert_eq!(t.handle(&press("f9")), None);
        assert_eq!(t.handle(&press("f9")), None);
        assert_eq!(t.handle(&release("f9")), Some(TrackerOutput::ChordRelease));
    }

    #[test]
    fn opposite_side_does_not_satisfy_side_specific_target() {
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        // ctrl_r is foreign — for a bare-modifier binding it would only
        // matter if we held ctrl_l first. Standalone right ctrl: no chord.
        assert_eq!(t.handle(&press("ctrl_r")), None);
        assert_eq!(t.handle(&release("ctrl_r")), None);
    }

    #[test]
    fn generic_press_satisfies_side_specific_target_failsafe() {
        // The OS occasionally delivers sideless ctrl — must still satisfy
        // a ctrl_l binding (fail-safe toward starting).
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl")),
            Some(TrackerOutput::ChordRelease)
        );
    }

    #[test]
    fn chord_completion_needs_two_distinct_held() {
        // ctrl_l+ctrl_r both-sides chord must NOT fire on a single generic
        // ctrl press — that's the 1:1 matching property.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "ctrl_r".to_owned()]);
        assert_eq!(t.handle(&press("ctrl")), None);
        // Adding a second key — a side-specific one — completes the chord
        // via the augmenting-path matching (one generic + one specific).
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
    }

    #[test]
    fn bare_modifier_with_foreign_key_held_blocks_start_rule1() {
        // Foreign key held FIRST, then PTT chord → rule 1: refuse to start.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("a")), None);
        assert_eq!(t.handle(&press("ctrl_l")), None);
        // Release the foreign key first, then ctrl_l — still no chord
        // since the latch was set to suppress it. (Mirrors vp_keys.py: a
        // late release re-arms only after the chord breaks.)
        assert_eq!(t.handle(&release("a")), None);
        assert_eq!(t.handle(&release("ctrl_l")), None);
    }

    #[test]
    fn bare_modifier_foreign_press_during_recording_cancels_rule2() {
        // Chord held, then foreign key down → cancel.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(t.handle(&press("c")), Some(TrackerOutput::ChordCancel));
    }

    #[test]
    fn non_bare_binding_ignores_foreign_keys() {
        // f9 alone is NOT a bare-modifier binding — rule 1/2 do not apply.
        let mut t = KeyTracker::new(vec!["f9".to_owned()]);
        assert_eq!(t.handle(&press("a")), None);
        assert_eq!(t.handle(&press("f9")), Some(TrackerOutput::ChordPress));
        // Foreign press during recording: no cancel for non-bare bindings.
        assert_eq!(t.handle(&press("b")), None);
        assert_eq!(t.handle(&release("f9")), Some(TrackerOutput::ChordRelease));
    }

    #[test]
    fn release_of_opposite_side_does_not_break_chord() {
        // Both-sides chord held; release of one side breaks it, release of
        // the other doesn't fire a second time. Mirrors the side-specific
        // release-clearing path.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned(), "ctrl_r".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), None);
        assert_eq!(t.handle(&press("ctrl_r")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl_l")),
            Some(TrackerOutput::ChordRelease)
        );
        // Right side still held — releasing it now is a no-op (the chord
        // already broke on the left release).
        assert_eq!(t.handle(&release("ctrl_r")), None);
    }

    #[test]
    fn generic_release_clears_whole_family() {
        // ctrl_l down, then sideless ctrl up: must clear the held ctrl_l so
        // the chord breaks (fail-safe). Mirrors the generic-release branch.
        let mut t = KeyTracker::new(vec!["ctrl_l".to_owned()]);
        assert_eq!(t.handle(&press("ctrl_l")), Some(TrackerOutput::ChordPress));
        assert_eq!(
            t.handle(&release("ctrl")),
            Some(TrackerOutput::ChordRelease)
        );
    }
}
