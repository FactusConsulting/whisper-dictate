//! Process-bound ownership lock for push-to-talk.
//!
//! ## The bug this closes
//!
//! On 2026-07-29 a `whisper-dictate.exe dictate-run` CLI process and the
//! `whisper-dictate-gui.exe` tray app ran side by side. Both registered
//! F9 — the GUI through the `win_registerhotkey` driver, the CLI through
//! `rdev`. One press made both record, both transcribe, and both inject
//! into the focused window at the same time. The 232-character utterance
//! came out written over itself:
//!
//! ```text
//! skaDle rae nedrr eesn.  sVeit thianrg  omgesraae ,e nd elra nsgksaolm
//! ```
//!
//! Neither process logged anything wrong. Both ran a clean cycle; the
//! corruption happened in a third place, the focused window.
//!
//! ## Why it needs its own guard
//!
//! Nothing downstream can see the problem. Each driver only knows about
//! the hotkey IT registered — `RegisterHotKey` returns success because
//! nobody in THIS process took F9, and the `rdev` low-level hook is
//! passive by construction and never conflicts with anything. So a guard
//! inside either driver is blind to the other, which is exactly the pair
//! that broke. The guard has to sit above both, at the one point every
//! backend passes through: [`crate::hotkey::install_hotkey_with_raw_tap`].
//!
//! Keying it there also satisfies "any future entry point" — a new verb
//! that installs a hotkey is covered because it installs a hotkey, not
//! because someone remembered to add it to a list.
//!
//! ## The primitive, and why
//!
//! `std::fs::File::try_lock` (stable since 1.89), on a zero-byte file in
//! a per-user directory. It is `flock(LOCK_EX|LOCK_NB)` on Linux and
//! `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY)` on
//! Windows. Both bind the lock's lifetime to the OPEN FILE HANDLE, which
//! gives us the release semantics the brief demands, for free and without
//! any reaping code:
//!
//! | Holder ends by | What releases the lock |
//! |---|---|
//! | normal exit / [`PttLock::release`] | `Drop` closes the `File` |
//! | panic (unwind) | `Drop` closes the `File` |
//! | panic (abort), `kill -9`, TerminateProcess, power loss | the kernel closes every handle at process teardown |
//!
//! A PID file would have needed liveness probing and could strand a
//! permanent block after a crash — "a stale lock that blocks every future
//! launch is worse than the bug". This primitive cannot go stale: the file
//! may survive on disk, but the LOCK cannot outlive the process.
//!
//! It also gives us a hermetic test seam. Both platforms scope the lock to
//! the file handle, not the process, so two `File::open` calls in ONE
//! process contend exactly like two processes do — the refusal path is
//! unit-testable without spawning anything.
//!
//! ## Two files
//!
//! Windows byte-range locks are mandatory: a locked-out process cannot
//! read the locked file. So the holder's identity lives in an unlocked
//! sibling ([`record`]) and the lock file itself stays empty. The sibling
//! is advisory — it feeds the message, never the decision.
//!
//! ## Failure to open the lock at all
//!
//! Deliberately FAIL-OPEN, with a warning. If the lock directory is
//! read-only or the file cannot be created, we install the hotkey anyway.
//! Refusing push-to-talk outright because a temp file could not be
//! created would break dictation for a user who has no second process at
//! all, converting a rare corruption into a common outage. The `reason`
//! goes to the diagnostic log so the degraded state is inspectable.

pub mod paths;
pub mod record;
pub mod report;

// Companion tests. Split out of inline `#[cfg(test)] mod tests` blocks so
// the regression-test discipline scanner sees a matching test file next to
// each production module (`src/tests/python/test_regression_test_discipline.py`).
#[cfg(test)]
#[path = "mod_tests.rs"]
mod mod_tests;
#[cfg(test)]
#[path = "paths_tests.rs"]
mod paths_tests;
#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;
#[cfg(test)]
#[path = "report_tests.rs"]
mod report_tests;

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub use record::HolderRecord;
pub use report::PttConflict;

/// Outcome of trying to take PTT ownership.
#[derive(Debug)]
pub enum Acquisition {
    /// We own push-to-talk. Hold the [`PttLock`] for as long as the
    /// hotkey is installed; dropping it releases ownership.
    Acquired(PttLock),
    /// Another process owns it. `holder` is `None` when the advisory
    /// record could not be read — the refusal still stands, we just
    /// cannot name the PID.
    Held {
        holder: Option<HolderRecord>,
        lock_path: PathBuf,
    },
    /// The lock file itself could not be opened. Callers fail OPEN (see
    /// the module docs) and log `reason`.
    Unavailable { reason: String },
}

/// Live PTT ownership. Releases on drop, on panic, and on process death.
///
/// No `Clone`, no `Copy`: exactly one value per acquisition, so ownership
/// cannot be duplicated inside a process either.
#[derive(Debug)]
pub struct PttLock {
    /// The locked handle. `Option` only so [`Drop`] can close it in a
    /// controlled order relative to the owner-record cleanup.
    lock_file: Option<File>,
    lock_path: PathBuf,
    owner_path: PathBuf,
    holder: HolderRecord,
}

impl PttLock {
    /// The record this process published while acquiring.
    pub fn holder(&self) -> &HolderRecord {
        &self.holder
    }

    /// Path of the locked file, for diagnostics.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// Release ownership now instead of at drop. Consuming, so a released
    /// lock cannot be used again.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for PttLock {
    /// Order matters: remove the advisory record BEFORE releasing the OS
    /// lock.
    ///
    /// The other order has a real race — release first, and a waiting
    /// process can acquire and write ITS record in the gap, which this
    /// `drop` would then delete. The next contender would read nothing
    /// and report "pid unknown" against a live, nameable holder. Removing
    /// first cannot misfire: while we still hold the lock, nobody else can
    /// have written a record.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.owner_path);
        // Explicit unlock before the handle closes. Dropping the `File`
        // alone would release it, but naming the step keeps the ordering
        // above meaningful to a reader and independent of `File`'s drop
        // glue.
        if let Some(file) = self.lock_file.take() {
            let _ = file.unlock();
            drop(file);
        }
    }
}

/// Take PTT ownership for this process, in the default per-user location.
///
/// `chord` is the `+`-joined binding and `driver` the backend label
/// (`rdev` / `evdev` / `win_registerhotkey`); both are recorded so the NEXT
/// process's refusal message can name them.
pub fn acquire(chord: &str, driver: &str) -> Acquisition {
    let dir = paths::lock_dir();
    let user = paths::user_tag();
    acquire_at(
        &paths::lock_path_in(&dir, &user),
        &paths::owner_path_in(&dir, &user),
        HolderRecord::for_current_process(chord, driver),
    )
}

/// Explicit-path form behind [`acquire`].
///
/// This is the hermetic test seam: a test points both paths into a
/// `tempfile::TempDir` and supplies its own [`HolderRecord`], so it can
/// assert the refusal names a PID that is demonstrably not its own —
/// without process env mutation and without spawning anything.
pub fn acquire_at(lock_path: &Path, owner_path: &Path, holder: HolderRecord) -> Acquisition {
    if let Some(parent) = lock_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return Acquisition::Unavailable {
                reason: format!("cannot create {}: {err}", parent.display()),
            };
        }
    }
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(err) => {
            return Acquisition::Unavailable {
                reason: format!("cannot open {}: {err}", lock_path.display()),
            }
        }
    };
    match file.try_lock() {
        Ok(()) => {
            // We own it. Publish the advisory record; a write failure
            // costs the NEXT process a named PID but must not cost us the
            // lock we just legitimately took.
            write_owner_record(owner_path, &holder);
            Acquisition::Acquired(PttLock {
                lock_file: Some(file),
                lock_path: lock_path.to_path_buf(),
                owner_path: owner_path.to_path_buf(),
                holder,
            })
        }
        Err(TryLockError::WouldBlock) => Acquisition::Held {
            holder: read_owner_record(owner_path),
            lock_path: lock_path.to_path_buf(),
        },
        Err(TryLockError::Error(err)) => Acquisition::Unavailable {
            reason: format!("cannot lock {}: {err}", lock_path.display()),
        },
    }
}

/// Overwrite `owner_path` with `holder`'s single-line record.
///
/// Truncate-and-rewrite rather than create-new: the file is expected to
/// exist from a previous holder (we never delete it on a crash path), and
/// a leftover from a longer record must not tail the new one.
fn write_owner_record(owner_path: &Path, holder: &HolderRecord) {
    let outcome = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(owner_path)
        .and_then(|mut file| {
            file.seek(SeekFrom::Start(0))?;
            file.write_all(holder.encode().as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()
        });
    if let Err(err) = outcome {
        crate::diag::log!(
            "[hotkey] could not write the PTT holder record to {} ({err}); a second \
             whisper-dictate process will still be refused, but its message will not be \
             able to name this process's pid.",
            owner_path.display()
        );
    }
}

/// What the install-time decision concluded.
///
/// A three-way enum rather than `Result<Option<PttLock>, PttConflict>`:
/// the outcomes are genuinely three, and `Ok(None)` reads as "nothing
/// happened" when it actually means "the guard is off for this session".
/// It also keeps `clippy::result_large_err` happy, which it would not be
/// with a 150-byte error variant on a hot install path.
#[derive(Debug)]
pub enum PttOwnership {
    /// We own push-to-talk. Hold the lock for the lifetime of the hotkey.
    Owned(PttLock),
    /// Another process owns it. The conflict has already been written to
    /// the diagnostic log and published to [`report::current`] for the GUI
    /// banner by the time this is returned.
    Refused(PttConflict),
    /// The lock could not be opened at all. FAIL-OPEN: install anyway,
    /// with the reason already in the diagnostic log (see the module
    /// docs for why this is not a refusal).
    Unguarded,
}

/// The whole install-time decision in one place: take PTT ownership, or
/// refuse loudly.
///
/// Sits here rather than in `crate::hotkey` so it compiles — and its
/// tests run — on every CI leg, including the ones without the
/// `rust-hotkeys` feature.
pub fn acquire_or_refuse(chord: &str, driver: &str) -> PttOwnership {
    let dir = paths::lock_dir();
    let user = paths::user_tag();
    acquire_or_refuse_at(
        &paths::lock_path_in(&dir, &user),
        &paths::owner_path_in(&dir, &user),
        chord,
        driver,
    )
}

/// Explicit-path form behind [`acquire_or_refuse`], and the seam the
/// refusal tests drive: they point both paths into a temp dir and call it
/// twice, which contends exactly like two processes because the OS scopes
/// these locks to the file handle rather than the process.
pub fn acquire_or_refuse_at(
    lock_path: &Path,
    owner_path: &Path,
    chord: &str,
    driver: &str,
) -> PttOwnership {
    let holder = HolderRecord::for_current_process(chord, driver);
    match acquire_at(lock_path, owner_path, holder) {
        Acquisition::Acquired(lock) => {
            // A previous attempt in this process may have parked a
            // refusal; we own the hotkey now, so retract it.
            report::clear();
            PttOwnership::Owned(lock)
        }
        Acquisition::Held { holder, lock_path } => {
            let conflict = PttConflict {
                chord: chord.to_owned(),
                holder,
                lock_path: lock_path.display().to_string(),
            };
            // `diag::log!` tees to the GUI diagnostic file AND stderr, so
            // this single call covers both the console-subsystem CLI
            // operator and the after-the-fact Windows log. The GUI banner
            // is fed by the slot below, because a windows-subsystem
            // binary's stderr goes nowhere.
            crate::diag::log!("{}", conflict.message());
            report::record(conflict.clone());
            PttOwnership::Refused(conflict)
        }
        Acquisition::Unavailable { reason } => {
            crate::diag::log!(
                "[hotkey] could not open the push-to-talk ownership lock ({reason}); \
                 installing the hotkey anyway. The guard against a second \
                 whisper-dictate process taking the same chord is INACTIVE for this \
                 session - if you start a second one, injected text may interleave."
            );
            report::clear();
            PttOwnership::Unguarded
        }
    }
}

/// Read the advisory holder record. Any failure — missing file, unreadable
/// file, truncated or foreign contents — yields `None`, and the caller
/// degrades the message to "pid unknown".
///
/// Capped read: a corrupted or hostile file must not be pulled into memory
/// wholesale just to format one log line.
fn read_owner_record(owner_path: &Path) -> Option<HolderRecord> {
    const MAX_RECORD_BYTES: u64 = 4096;
    let file = File::open(owner_path).ok()?;
    let mut raw = String::new();
    file.take(MAX_RECORD_BYTES).read_to_string(&mut raw).ok()?;
    HolderRecord::parse(&raw)
}
