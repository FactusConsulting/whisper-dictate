//! The refusal message, and the process-wide slot that carries it to the
//! GUI.
//!
//! A refusal that only shows up as a non-zero exit code is its own bug: a
//! tray app that quietly stops responding to F9 is indistinguishable from
//! a broken hotkey. So the refusal is published three ways:
//!
//! * [`PttConflict::message`] goes through `crate::diag::log!` at the
//!   refusal site, which tees to the GUI diagnostic file AND to stderr —
//!   the console-subsystem CLI operator sees it immediately.
//! * The same text is carried by
//!   [`crate::hotkey::InstallError::AlreadyHeld`], so every caller that
//!   already prints an install error gets it for free.
//! * [`record`] parks it in a process-wide slot that the egui app polls
//!   each frame and renders as a banner. The GUI's stderr is discarded on
//!   Windows (it is a windows-subsystem binary), so without this slot the
//!   user would never see the reason.
//!
//! The slot is process-wide rather than threaded through the install
//! signature because the install call sits five layers below the UI
//! (`ui::app` -> `RuntimeSupervisor::start` -> `install_rust_hotkey_from_command`
//! -> `maybe_install_rust_hotkey` -> `install_hotkey`), and three of those
//! layers deliberately collapse the error to `Option<HotkeyHandle>`. Same
//! shape, and same reasoning, as `hotkey::inject_guard::set_global`.

use std::sync::Mutex;

use super::record::HolderRecord;

/// A refused PTT registration: what we wanted, and who already had it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PttConflict {
    /// The `+`-joined chord this process tried to register.
    pub chord: String,
    /// The holder, when its advisory record was readable. `None` means
    /// the lock is genuinely held but the record file was missing or
    /// half-written — we still refuse, we just cannot name the PID.
    pub holder: Option<HolderRecord>,
    /// Display form of the lock file, so a support thread can confirm
    /// which lock was contended.
    pub lock_path: String,
}

impl PttConflict {
    /// PID of the blocking process when known.
    pub fn holder_pid(&self) -> Option<u32> {
        self.holder.as_ref().map(|h| h.pid)
    }

    /// Short holder phrase used inside [`crate::hotkey::InstallError`].
    pub fn holder_description(&self) -> String {
        match &self.holder {
            Some(holder) => holder.describe(),
            None => "another whisper-dictate process (pid unknown)".to_owned(),
        }
    }

    /// The full, actionable refusal text.
    ///
    /// Three things it must contain, all of them learned from the
    /// 2026-07-29 report where the user got NONE of them: the chord, the
    /// PID to go and quit, and what the refusal prevented. The last one
    /// is what turns "whisper-dictate refused to start its hotkey" into
    /// "ah, that is why my dictation came out scrambled last time".
    pub fn message(&self) -> String {
        let holder = match &self.holder {
            Some(holder) => format!("{} already holds it", holder.describe()),
            None => format!(
                "another whisper-dictate process already holds it (its holder record at {} \
                 was missing or unreadable, so the pid cannot be named - use Task Manager \
                 or `ps` to find the other whisper-dictate process)",
                self.lock_path
            ),
        };
        format!(
            "[hotkey] REFUSED to register the push-to-talk chord {:?}: {}. Only one \
             whisper-dictate process may own push-to-talk at a time. If both held it, \
             one key press would start BOTH processes recording, both would transcribe, \
             and both would type into the focused window - interleaving the injected \
             text character by character and corrupting it. Quit the other \
             whisper-dictate process, then start this one again. (lock: {})",
            self.chord, holder, self.lock_path
        )
    }
}

/// Process-wide slot holding the most recent refusal. `Mutex<Option<_>>`
/// rather than `OnceLock` because it must be CLEARABLE: a successful
/// install after the blocking process quits has to retract the banner.
static LAST_CONFLICT: Mutex<Option<PttConflict>> = Mutex::new(None);

/// Publish `conflict` for the UI to pick up. Last writer wins — a second
/// refusal is newer information than the first.
///
/// A poisoned mutex is ignored rather than propagated: losing the banner
/// is bad, panicking the install path is worse.
pub fn record(conflict: PttConflict) {
    if let Ok(mut slot) = LAST_CONFLICT.lock() {
        *slot = Some(conflict);
    }
}

/// The most recent refusal, if PTT is currently refused in this process.
pub fn current() -> Option<PttConflict> {
    LAST_CONFLICT.lock().ok().and_then(|slot| slot.clone())
}

/// Retract the refusal. Called at the top of every install attempt so the
/// slot always describes the LATEST attempt rather than a historical one,
/// and so a retry that succeeds clears the GUI banner.
pub fn clear() {
    if let Ok(mut slot) = LAST_CONFLICT.lock() {
        *slot = None;
    }
}

/// Serialises the tests that read or write [`LAST_CONFLICT`].
///
/// The slot is process-wide by design, so two `cargo test` threads
/// exercising the publish path would otherwise clobber each other's
/// assertions. Crate-visible because the tests live in two companion
/// files (`mod_tests.rs` drives the publish path, `report_tests.rs` the
/// slot itself) and both must take the SAME lock for it to mean anything.
#[cfg(test)]
pub(crate) static TEST_SLOT_LOCK: Mutex<()> = Mutex::new(());
