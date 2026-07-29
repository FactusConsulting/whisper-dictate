//! The advisory *holder record* — who currently owns the PTT hotkey.
//!
//! The lock itself is an OS file lock (see [`super`]); this module owns the
//! side-channel that makes a refusal ACTIONABLE. Without it a blocked
//! process could only say "somebody else has the hotkey", which is exactly
//! the un-actionable message the 2026-07-29 interleaved-injection report
//! asked us not to ship.
//!
//! ## Why a separate file rather than the lock file's own bytes
//!
//! On Windows `File::try_lock` maps to `LockFileEx`, whose byte-range locks
//! are MANDATORY: a second process cannot even `read` the locked range. So
//! the record cannot live inside the locked file — the one process that
//! needs to read it is precisely the one that is locked out. The holder
//! therefore writes an unlocked sibling file next to the lock.
//!
//! That sibling is ADVISORY ONLY. It never participates in the
//! lock/refuse decision (the OS lock alone does that), so a stale record
//! left behind by `kill -9` can at worst degrade the message to
//! "pid unknown" — it can never block a launch. See [`super`] for the
//! release ordering that keeps staleness to a race-window rather than a
//! permanent state.
//!
//! ## Encoding
//!
//! One line of `key=value` pairs separated by single spaces. Values are
//! sanitised to a conservative ASCII token set at construction
//! ([`sanitize_token`]) so the format is unambiguous by construction,
//! round-trips without escaping, and cannot smuggle a non-ASCII byte into
//! a console line (AGENTS.md console-output rule). Parsing is deliberately
//! total: any malformed input yields `None` and the caller degrades to
//! "holder unknown".

/// Format version stamped into every record. Bumped only if the field set
/// changes incompatibly; a reader that sees an unknown version treats the
/// record as unreadable rather than guessing.
pub const RECORD_VERSION: u32 = 1;

/// Longest token we keep for any single field. Long enough for a real
/// executable stem or a chord, short enough that a hostile / corrupted
/// file cannot blow up a log line.
const MAX_TOKEN_LEN: usize = 64;

/// Placeholder written when a field cannot be determined (no executable
/// path, no subcommand, empty chord).
pub const UNKNOWN_TOKEN: &str = "unknown";

/// Placeholder for "this entry point takes no subcommand" (the GUI).
pub const NO_VERB_TOKEN: &str = "none";

/// Who holds the PTT hotkey, as recorded by the holding process.
///
/// Deliberately NOT a snapshot of anything security-sensitive: it exists
/// only so a refused process can name its blocker in a message a user can
/// act on ("quit pid 12345").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderRecord {
    /// OS process id of the holder.
    pub pid: u32,
    /// Executable file stem, e.g. `whisper-dictate-gui` or
    /// `whisper-dictate`. Sanitised.
    pub exe: String,
    /// First CLI argument (the subcommand) when there is one, e.g.
    /// `dictate-run`; [`NO_VERB_TOKEN`] otherwise. Sanitised.
    pub verb: String,
    /// Hotkey driver the holder installed, e.g. `rdev`,
    /// `win_registerhotkey`, `evdev`. Sanitised.
    pub driver: String,
    /// The `+`-joined chord the holder registered, e.g. `f9` or
    /// `ctrl_l+shift_l`. Sanitised.
    pub chord: String,
}

impl HolderRecord {
    /// Build a record describing THIS process, sanitising every field.
    ///
    /// `exe` / `verb` are derived from the running process rather than
    /// passed in, so a future entry point that installs a hotkey is
    /// described correctly without anyone remembering to register a label
    /// — the brief's "key it on the act of registering, not on the binary
    /// name" requirement applied to the report as well as the lock.
    pub fn for_current_process(chord: &str, driver: &str) -> Self {
        Self::new(
            std::process::id(),
            &current_exe_stem(),
            &current_verb(),
            driver,
            chord,
        )
    }

    /// Sanitising constructor. Exposed so tests can build a record with a
    /// PID that is not their own — the "refusal names the holding process"
    /// assertion needs a holder that is demonstrably somebody else.
    pub fn new(pid: u32, exe: &str, verb: &str, driver: &str, chord: &str) -> Self {
        Self {
            pid,
            exe: sanitize_token(exe),
            verb: sanitize_token(verb),
            driver: sanitize_token(driver),
            chord: sanitize_token(chord),
        }
    }

    /// Serialise to the single-line wire form. Always ASCII, never
    /// contains a newline, so a reader can parse the first line and stop.
    pub fn encode(&self) -> String {
        format!(
            "v={} pid={} exe={} verb={} driver={} chord={}",
            RECORD_VERSION, self.pid, self.exe, self.verb, self.driver, self.chord
        )
    }

    /// Parse the wire form. Returns `None` for anything unexpected —
    /// truncated write, foreign format, unknown version, missing field —
    /// because every caller's fallback ("holder unknown") is strictly
    /// better than a guess.
    ///
    /// Total by design: a contender reads this file WITHOUT holding any
    /// lock, so it can legitimately observe a half-written record in the
    /// microseconds between the holder's `set_len(0)` and its `flush`.
    pub fn parse(raw: &str) -> Option<Self> {
        let line = raw.lines().next()?.trim();
        let mut version: Option<u32> = None;
        let mut pid: Option<u32> = None;
        let mut exe: Option<String> = None;
        let mut verb: Option<String> = None;
        let mut driver: Option<String> = None;
        let mut chord: Option<String> = None;
        for field in line.split(' ').filter(|s| !s.is_empty()) {
            let (key, value) = field.split_once('=')?;
            match key {
                "v" => version = value.parse().ok(),
                "pid" => pid = value.parse().ok(),
                "exe" => exe = Some(value.to_owned()),
                "verb" => verb = Some(value.to_owned()),
                "driver" => driver = Some(value.to_owned()),
                "chord" => chord = Some(value.to_owned()),
                // Unknown keys are ignored rather than fatal so a future
                // additive field does not make old readers blind.
                _ => {}
            }
        }
        if version? != RECORD_VERSION {
            return None;
        }
        Some(Self {
            pid: pid?,
            exe: exe?,
            verb: verb?,
            driver: driver?,
            chord: chord?,
        })
    }

    /// Human-readable one-liner naming the holder, for the refusal
    /// message. Always leads with the PID because that is the single
    /// thing the user needs in order to act.
    pub fn describe(&self) -> String {
        let program = if self.verb == NO_VERB_TOKEN {
            self.exe.clone()
        } else {
            format!("{} {}", self.exe, self.verb)
        };
        format!(
            "pid {} ({}, driver {}, chord {})",
            self.pid, program, self.driver, self.chord
        )
    }
}

/// Reduce `raw` to a conservative ASCII token: alphanumerics plus
/// `-`, `_`, `.` and `+` (the chord separator) survive; every other byte
/// (including any non-ASCII one) becomes `_`. Truncated to
/// [`MAX_TOKEN_LEN`]; an empty result becomes [`UNKNOWN_TOKEN`].
///
/// Two jobs at once: it keeps the `key=value` line unambiguous (no spaces,
/// no newlines, no `=`), and it guarantees the record can be pasted into a
/// console line without tripping the repo's ASCII-output rule regardless of
/// what the user named their executable or their profile directory.
pub fn sanitize_token(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_TOKEN_LEN));
    for ch in raw.chars() {
        if out.len() >= MAX_TOKEN_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '+') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        UNKNOWN_TOKEN.to_owned()
    } else {
        trimmed
    }
}

/// File stem of the running executable, or [`UNKNOWN_TOKEN`] when the OS
/// will not tell us (which happens on some hardened Linux configurations).
fn current_exe_stem() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| UNKNOWN_TOKEN.to_owned())
}

/// The subcommand this process was invoked with, or [`NO_VERB_TOKEN`].
///
/// Skips anything starting with `-` so `whisper-dictate --json dictate-run`
/// still reports `dictate-run` rather than a flag.
fn current_verb() -> String {
    std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_else(|| NO_VERB_TOKEN.to_owned())
}
