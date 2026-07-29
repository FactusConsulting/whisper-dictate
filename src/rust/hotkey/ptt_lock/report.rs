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
    ///
    /// Stored verbatim; [`PttConflict::message`] renders it through
    /// [`ascii_path`] because this message reaches CLI stderr and the
    /// Windows subprocess logs.
    pub lock_path: String,
}

/// Render a filesystem path for a console line without dragging non-ASCII
/// bytes onto it.
///
/// Codex P2 #688: a localized Windows profile (`C:\Users\Jørgen\...`) or a
/// `VOICEPI_PTT_LOCK_DIR` with non-ASCII in it would otherwise make this
/// refusal line non-ASCII, and it is written to PowerShell / cmd.exe
/// stderr and to the Rust UI's subprocess logs — the exact surfaces
/// AGENTS.md's console-output rule covers, where a legacy code page turns
/// those bytes into mojibake.
///
/// Each non-ASCII character becomes `?`. A degraded path is still enough
/// for a support thread to identify WHICH lock was contended (the
/// directory structure and file name survive), and the pid — the part the
/// user actually acts on — is unaffected.
pub fn ascii_path(raw: &str) -> String {
    raw.chars()
        .map(|ch| if ch.is_ascii() { ch } else { '?' })
        .collect()
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
        // Both the chord and the path are user-influenced, and this line
        // goes to a console: sanitise both rather than trusting either.
        let lock_path = ascii_path(&self.lock_path);
        let chord = ascii_path(&self.chord);
        let holder = match &self.holder {
            Some(holder) => format!("{} already holds it", holder.describe()),
            None => format!(
                "another whisper-dictate process already holds it (its holder record at {lock_path} \
                 was missing or unreadable, so the pid cannot be named - use Task Manager \
                 or `ps` to find the other whisper-dictate process)"
            ),
        };
        format!(
            "[hotkey] REFUSED to register the push-to-talk chord {chord:?}: {holder}. Only one \
             whisper-dictate process may own push-to-talk at a time. If both held it, \
             one key press would start BOTH processes recording, both would transcribe, \
             and both would type into the focused window - interleaving the injected \
             text character by character and corrupting it. Quit the other \
             whisper-dictate process, then start this one again. (lock: {lock_path})"
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
/// assertions. Crate-visible because the tests that touch it are spread
/// across several files and all of them must take the SAME lock for it to
/// mean anything.
///
/// **Usage rule** (mirrors the `diag_test_lock::DIAG_WRITER_LOCK` and
/// `test_env_lock::ENV_LOCK` disciplines): every `#[test]` that
///
/// * calls [`record`] / [`current`] / [`clear`] directly, **or**
/// * calls `hotkey::install_hotkey` on a path that reaches the ownership
///   guard — which `clear()`s the slot on entry and may `record()` a
///   conflict,
///
/// MUST hold this lock across the whole read/write window. The second
/// case is the one that is easy to miss: an install-path test looks like
/// it has nothing to do with the report slot (Codex P2 #688).
///
/// When a test needs this AND `GLOBAL_GUARD_LOCK`, take
/// `GLOBAL_GUARD_LOCK` first — every current call site does, so the pair
/// cannot deadlock.
#[cfg(test)]
pub(crate) static TEST_SLOT_LOCK: Mutex<()> = Mutex::new(());
