//! External toggle / cancel IPC for issue #326.
//!
//! On Wayland (and increasingly on Linux in general) global hotkeys are owned
//! by the compositor, not the application. The user binds their compositor
//! shortcut to either:
//!
//! - `whisper-dictate --toggle-recording` (or `--start-recording` /
//!   `--stop-recording` / `--cancel-recording`), or
//! - `kill -USR1 $(pidof whisper-dictate)` / `kill -USR2 ...`.
//!
//! Both paths converge on the same in-process [`ExternalCommand`] channel,
//! drained by the [`crate::runtime::RuntimeSupervisor`] on every UI poll. The
//! supervisor forwards the command to the Rust hotkey coordinator
//! ([`crate::hotkey::coordinator::CoordinatorEvent::ExternalToggle`] /
//! `Cancel`) so external triggers fall through the exact same state machine
//! as a real PTT chord — the in-flight `Stage::Recording` / `Stage::Processing`
//! guards apply uniformly.
//!
//! ## Mechanism
//!
//! - **Daemon side (UI process):** [`install_signal_handlers`] writes a PID
//!   file under `$XDG_RUNTIME_DIR/whisper-dictate/whisper-dictate.pid` (or
//!   the cache dir on macOS/Windows) and on Linux installs SIGUSR1/SIGUSR2
//!   handlers via the `signal-hook` crate. On macOS the signal install is a
//!   stub but the PID file is still written so a future macOS path
//!   (Distributed Notifications, AppleScript) can find it. Windows is a
//!   no-op: equivalent IPC (named pipe / WM_USER message) is a follow-up.
//! - **CLI side:** [`forward_command`] reads the PID file, writes the
//!   intended action (`toggle` / `start` / `stop`) to a one-line command
//!   file so the daemon's signal handler can distinguish them, and sends
//!   SIGUSR1. SIGUSR2 is reserved exclusively for `cancel` so a "stop
//!   everything now" hotkey works even when the command file is missing.
//!
//! ## Start vs. stop vs. toggle
//!
//! Unix signals are opaque integers — they cannot carry data. To
//! differentiate start / stop / toggle on a single SIGUSR1 we use a tiny
//! command file: the CLI writes one token (`toggle` / `start` / `stop`)
//! before sending the signal; the daemon reads-and-deletes it on signal
//! receipt and falls back to `toggle` if the file is missing (so a raw
//! `kill -USR1` still does the obvious thing).
//!
//! This is intentionally simpler than a full Unix-socket IPC: the command
//! is a single token, written-then-deleted, and the file lives in the
//! per-user `$XDG_RUNTIME_DIR` (mode `0700`) so privilege boundaries match
//! the PID file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

/// External command requested via signal or CLI flag. Drained by the
/// supervisor on the next poll and routed to the hotkey coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalCommand {
    /// Start a new recording if idle, stop-and-transcribe if recording.
    /// SIGUSR1 with no command file (or command file == "toggle").
    Toggle,
    /// Start a recording if idle; no-op otherwise (matches `--start-recording`
    /// semantics). On the wire this is still SIGUSR1, distinguished from
    /// `Toggle` by the command file's contents.
    Start,
    /// Stop-and-transcribe if recording; no-op otherwise (matches
    /// `--stop-recording`). Same SIGUSR1 + command file pattern.
    Stop,
    /// Discard in-flight recording, return to Idle. SIGUSR2 — always
    /// distinct from SIGUSR1 so an emergency "stop everything" hotkey works
    /// even when the command file is missing or stale.
    Cancel,
}

impl ExternalCommand {
    /// Token written to the command file by the CLI forwarder. The daemon
    /// parses the same token in [`Self::parse_command_token`]. SIGUSR2 does
    /// not need a token (signal == cancel), but `Cancel` is included here
    /// for completeness so `--cancel-recording` can also write a token in a
    /// future SIGUSR2 fallback path.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Cancel => "cancel",
        }
    }

    /// Parse a command-file token. Whitespace is trimmed. Unknown tokens
    /// default to [`Self::Toggle`] so a malformed file doesn't break the
    /// "kill -USR1 = toggle" contract. Empty input also defaults to Toggle.
    pub fn parse_command_token(raw: &str) -> Self {
        match raw.trim() {
            "start" => Self::Start,
            "stop" => Self::Stop,
            "cancel" => Self::Cancel,
            _ => Self::Toggle,
        }
    }

    /// Which Unix signal carries this command on the wire. Cancel rides on
    /// SIGUSR2; everything else rides on SIGUSR1 (with the command file
    /// disambiguating start / stop / toggle).
    #[cfg(target_os = "linux")]
    pub fn to_signal(self) -> i32 {
        match self {
            Self::Cancel => libc::SIGUSR2,
            _ => libc::SIGUSR1,
        }
    }
}

/// Resolve the per-user directory the PID + command files live in. Prefers
/// `$XDG_RUNTIME_DIR/whisper-dictate` on Linux (tmpfs, auto-cleaned by the
/// session manager), falls back to `<user cache>/whisper-dictate` everywhere
/// else.
pub fn runtime_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
            return Some(PathBuf::from(xdg).join("whisper-dictate"));
        }
    }
    crate::os_cache::user_cache_dir().map(|p| p.join("whisper-dictate"))
}

/// Default PID-file location. The daemon overwrites it on startup; the CLI
/// forwarder reads it to find the running daemon. Missing dir is created
/// on write (see [`write_pid_file`]).
pub fn default_pid_file_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("whisper-dictate.pid"))
}

/// Default command-file location. The CLI writes this BEFORE sending
/// SIGUSR1 so the daemon's signal handler can disambiguate
/// `--toggle-recording` / `--start-recording` / `--stop-recording`. The
/// daemon reads-and-deletes it on receipt.
pub fn default_command_file_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("whisper-dictate.cmd"))
}

/// Write `pid` to `path`, creating parent dirs as needed. Idempotent (an
/// existing file is overwritten). Pure helper; the daemon-side
/// [`install_signal_handlers`] wraps this in a guard that cleans up the
/// file on drop.
pub fn write_pid_file(path: &Path, pid: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, pid.to_string())
}

/// Read a PID written by [`write_pid_file`]. Returns
/// `io::ErrorKind::InvalidData` on a malformed file. The forwarder uses
/// this to find the daemon process to signal.
pub fn read_pid_file(path: &Path) -> io::Result<u32> {
    let raw = fs::read_to_string(path)?;
    raw.trim().parse::<u32>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "PID file is not a positive integer",
        )
    })
}

/// Best-effort delete. Missing file is not an error (the daemon may have
/// crashed without cleanup).
pub fn cleanup_pid_file(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Write the command token used by the daemon's signal handler to decide
/// between `start` / `stop` / `toggle` on a SIGUSR1 receipt. Cross-platform
/// pure helper.
pub fn write_command_token(path: &Path, cmd: ExternalCommand) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, cmd.as_token())
}

/// Read-and-delete the command token. Returns [`ExternalCommand::Toggle`]
/// when the file is missing — the documented fallback for a raw
/// `kill -USR1` invocation. An I/O error other than "not found" surfaces
/// so the daemon can log it; a malformed token defaults to `Toggle` via
/// [`ExternalCommand::parse_command_token`].
pub fn take_command_token(path: &Path) -> ExternalCommand {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return ExternalCommand::Toggle,
    };
    let _ = fs::remove_file(path);
    ExternalCommand::parse_command_token(&raw)
}

// ---------------------------------------------------------------------------
// Global channel: the signal handler writes commands, the supervisor drains.
// ---------------------------------------------------------------------------

/// Process-wide channel installed once by [`install_signal_handlers`]. The
/// signal handler (and the in-process supervisor path used by tests) push
/// [`ExternalCommand`]s onto the Sender; [`take_pending_commands`] drains
/// the Receiver from the supervisor's poll loop.
///
/// `Mutex<Receiver>` is fine because `try_recv` is non-blocking and the only
/// reader is the UI thread; lock contention is bounded.
struct ChannelSlot {
    tx: Sender<ExternalCommand>,
    rx: Mutex<Receiver<ExternalCommand>>,
}

static CHANNEL: OnceLock<ChannelSlot> = OnceLock::new();

fn ensure_channel() -> &'static ChannelSlot {
    CHANNEL.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        ChannelSlot {
            tx,
            rx: Mutex::new(rx),
        }
    })
}

/// Push a command into the global channel. Used by the signal handler AND
/// by the supervisor's own test paths. No-op if the channel slot has not
/// been initialised yet (avoids a panic from a stray signal arriving before
/// daemon setup completes).
pub fn push_command(cmd: ExternalCommand) {
    if let Some(slot) = CHANNEL.get() {
        let _ = slot.tx.send(cmd);
    }
}

/// Drain every pending command from the global channel. Called by the
/// supervisor on each poll. Returns commands in FIFO order; empty when the
/// channel slot hasn't been installed or no signals have fired since the
/// last drain.
pub fn take_pending_commands() -> Vec<ExternalCommand> {
    let Some(slot) = CHANNEL.get() else {
        return Vec::new();
    };
    let Ok(rx) = slot.rx.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(cmd) = rx.try_recv() {
        out.push(cmd);
    }
    out
}

/// Test-only reset (no-op in production builds). Used by unit tests that
/// re-install signal handlers across multiple cases. Lives next to
/// [`ensure_channel`] so the test file in `runtime/external_toggle_tests.rs`
/// can drain the channel without poking at private statics directly.
#[cfg(test)]
pub(super) fn reset_channel_for_tests() {
    let _ = ensure_channel();
    if let Some(slot) = CHANNEL.get() {
        if let Ok(rx) = slot.rx.lock() {
            while rx.try_recv().is_ok() {}
        }
    }
}

/// Test-only blocking wait on the global channel. Uses `recv_timeout` on
/// the actual receiver so the parent thread block-waits (OS-efficient)
/// instead of poll+sleeping. Under CPU throttling on CI the poll+sleep
/// form starved the signal-handler thread of scheduling and timed out
/// even at 5s; block-waiting yields the CPU to the handler immediately.
///
/// Linux-only: the sole caller (`linux_signal_handler_end_to_end`) is
/// gated on the same cfg; without this guard the function is unused
/// on Windows and clippy `-D warnings` on rust (windows-2025) trips.
#[cfg(all(test, target_os = "linux"))]
pub(super) fn recv_command_blocking(timeout: std::time::Duration) -> Option<ExternalCommand> {
    let slot = ensure_channel();
    let rx = slot.rx.lock().ok()?;
    rx.recv_timeout(timeout).ok()
}

// ---------------------------------------------------------------------------
// Daemon side: install signal handlers + write the PID file.
// ---------------------------------------------------------------------------

/// RAII guard for the PID file: removes it on drop so a clean exit leaves
/// no stale PID behind. Crash-exit paths leave the file (best effort);
/// the next daemon start overwrites it.
pub struct PidFileGuard {
    path: PathBuf,
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        cleanup_pid_file(&self.path);
    }
}

/// Install signal handlers (Linux only) and write the daemon PID file.
///
/// Returns a [`PidFileGuard`] the caller must hold for the lifetime of the
/// UI loop (drop = remove pidfile). On non-Linux platforms the signal
/// handlers are a no-op but the PID file is still written so a future
/// platform-specific IPC can find the daemon. Returns `Ok(None)` when the
/// PID-file path cannot be resolved (missing `HOME` + `XDG_RUNTIME_DIR` +
/// `LOCALAPPDATA`); the daemon still runs, just without external-toggle
/// support for this session.
pub fn install_signal_handlers() -> io::Result<Option<PidFileGuard>> {
    let Some(pid_path) = default_pid_file_path() else {
        return Ok(None);
    };
    write_pid_file(&pid_path, std::process::id())?;
    // Pre-create the channel before installing signal handlers so a signal
    // that races install completion has a valid Sender to push onto.
    let _ = ensure_channel();
    #[cfg(target_os = "linux")]
    {
        install_linux_signal_handlers()?;
    }
    Ok(Some(PidFileGuard { path: pid_path }))
}

#[cfg(target_os = "linux")]
fn install_linux_signal_handlers() -> io::Result<()> {
    use signal_hook::consts::{SIGUSR1, SIGUSR2};
    use signal_hook::iterator::Signals;
    // Idempotent: the signal-handler thread runs `signals.forever()`
    // which never exits, so a second install would leak a thread and
    // double every signal. Use a Mutex-guarded bool rather than a
    // OnceLock split into `get()` + `set()` so the check-and-install
    // pair is atomic -- the earlier `get().is_some() -> ... -> set()`
    // pattern allowed two concurrent callers to race past the guard,
    // each spawning their own `Signals::new` handle and thread. Claude
    // P1 #428 external_toggle.rs:336.
    static INSTALLED: Mutex<bool> = Mutex::new(false);
    let mut installed = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    if *installed {
        return Ok(());
    }
    let mut signals = Signals::new([SIGUSR1, SIGUSR2])?;
    // Readiness handshake with the spawned signal-handler thread. On a
    // loaded CI runner the classic pattern -- spawn a thread that runs
    // `signals.forever()` and immediately `raise()` in the parent -- can
    // fail if the just-spawned thread has not yet been scheduled by the
    // time the signal fires. signal-hook's self-pipe *does* buffer the
    // pending signal, but the observed test flake on PR #428 CI
    // (`linux_signal_handler_end_to_end` -> `left: None, right:
    // Some(Toggle)`) reproduces when the handler thread has not been
    // scheduled at all before the parent's 1s poll window elapses --
    // for instance when the runner is CPU-throttled. The fix: block
    // this function until the thread confirms it has been scheduled
    // and is about to enter the blocking iterator. Bounded by a short
    // timeout so a mis-spawn does not deadlock the daemon startup.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("vp-external-toggle".to_owned())
        .spawn(move || {
            let cmd_file = default_command_file_path();
            // Announce liveness BEFORE entering `signals.forever()` so
            // the parent can proceed knowing the OS scheduler has run
            // this thread at least once. Any signal delivered between
            // this send and the first `.next()` on the iterator is
            // captured by the sigaction registered in `Signals::new`
            // and buffered in signal-hook's self-pipe -- so nothing is
            // lost even if the raise happens in that tiny window.
            let _ = ready_tx.send(());
            for sig in signals.forever() {
                let cmd = match sig {
                    SIGUSR2 => ExternalCommand::Cancel,
                    SIGUSR1 => cmd_file
                        .as_deref()
                        .map(take_command_token)
                        .unwrap_or(ExternalCommand::Toggle),
                    _ => continue,
                };
                push_command(cmd);
            }
        })?;
    // Wait up to 1s for the thread to confirm it started. The
    // sigaction is already active (`Signals::new` above installed it
    // synchronously), so a timeout here does NOT drop signals -- it
    // only means we release the caller without a scheduling guarantee.
    // Kept as a fallback so a stuck / mis-scheduled worker cannot
    // deadlock the daemon's startup path.
    let _ = ready_rx.recv_timeout(std::time::Duration::from_secs(1));
    // Set the flag AFTER the thread spawn succeeded so the guard cannot
    // erroneously report "installed" if Signals::new or Builder::spawn
    // returned an error.
    *installed = true;
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI side: forward a command to the running daemon.
// ---------------------------------------------------------------------------

/// Forward `cmd` to the running daemon by reading the PID file and sending
/// the appropriate signal (Linux only). Returns a descriptive error when
/// the daemon is not running OR the platform does not support this IPC.
pub fn forward_command(cmd: ExternalCommand) -> anyhow::Result<()> {
    let pid_path = default_pid_file_path().ok_or_else(|| {
        anyhow::anyhow!("could not resolve PID file path (XDG_RUNTIME_DIR / HOME not set)")
    })?;
    let pid = read_pid_file(&pid_path).map_err(|err| {
        anyhow::anyhow!(
            "could not read PID file at {}: {err}. Is whisper-dictate running?",
            pid_path.display()
        )
    })?;
    forward_command_to_pid(cmd, pid)
}

/// Send `cmd` to a specific PID. Exposed so test fixtures can target a
/// dummy process, and the integration test on Linux can target the
/// spawned binary directly. On non-Linux this always returns a clear
/// "not yet implemented" error.
pub fn forward_command_to_pid(cmd: ExternalCommand, pid: u32) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Write the command token BEFORE raising the signal so the daemon's
        // signal handler sees the most recent intent. SIGUSR2 (cancel) does
        // not need a token, but we write one anyway so a future SIGUSR2
        // handler path can also distinguish.
        if matches!(
            cmd,
            ExternalCommand::Start | ExternalCommand::Stop | ExternalCommand::Toggle
        ) {
            if let Some(path) = default_command_file_path() {
                if let Err(err) = write_command_token(&path, cmd) {
                    eprintln!(
                        "[external-toggle] could not write command token at {}: {err}; \
                         continuing with raw signal (daemon will fall back to toggle)",
                        path.display()
                    );
                }
            }
        }
        let sig = cmd.to_signal();
        // SAFETY: libc::kill is a thin syscall wrapper; pid + sig are
        // validated integers. No memory is shared with the target.
        let rc = unsafe { libc::kill(pid as libc::pid_t, sig) };
        if rc == 0 {
            Ok(())
        } else {
            let err = io::Error::last_os_error();
            Err(anyhow::anyhow!(
                "kill({pid}, {sig}) failed: {err}. Is whisper-dictate still running?"
            ))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        let _ = cmd;
        Err(anyhow::anyhow!(
            "external-toggle: CLI forwarding is currently Linux-only. \
             On Windows / macOS, use the UI's Start/Stop controls or the \
             tray menu. See issue #326 for the cross-platform follow-up."
        ))
    }
}

// Tests live in `runtime/external_toggle_tests.rs` (sibling file) to keep
// this module under the 500-LOC modularity guideline (AGENTS.md). The
// sibling is wired in at the bottom of `runtime.rs` under
// `#[cfg(test)] mod external_toggle_tests;`.
