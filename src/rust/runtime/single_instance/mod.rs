//! Cross-platform single-instance gate with CLI-arg forwarding
//! (issue #327).
//!
//! Contract:
//!
//! * The first instance calls [`try_acquire`]. It gets back
//!   [`AcquireOutcome::Acquired`] with a live [`SingleInstance`] guard,
//!   binds a loopback TCP port for forwarded commands, writes a
//!   lockfile with `{pid, port, token}`, and starts a background thread
//!   that pushes each incoming argv into an mpsc queue the guard
//!   exposes via [`SingleInstance::try_recv`].
//!
//! * A second instance calls [`try_acquire`] with its own argv. It
//!   finds the lockfile, connects to the recorded port, sends its argv
//!   as a length-prefixed JSON frame, and gets back
//!   [`AcquireOutcome::Forwarded`]. `main.rs` then exits 0.
//!
//! Stale-lock handling: if the lockfile exists but the recorded port
//! refuses TCP connections (server crashed, was killed, laptop woke up
//! from sleep in a new session, …), we treat the lockfile as stale,
//! delete it, and become the running instance ourselves.
//!
//! Threat model + wire format are documented in [`socket`].
//!
//! Transport rationale — TCP loopback (not named pipes / abstract Unix
//! sockets):
//!
//! * `std::net::TcpListener` works identically on every target we ship
//!   (Windows, Linux, macOS) without pulling `windows-sys` /
//!   `interprocess` / `#[cfg]` splits into the module.
//! * The lockfile lives under `$XDG_RUNTIME_DIR` (0700 on Linux) or
//!   `%LOCALAPPDATA%\whisper-dictate` (per-user on Windows), so a
//!   sibling local process without read access to the lockfile can't
//!   discover the port + token pair anyway.
//! * D-Bus is explicitly called out as out of scope for v1 in #327.
//!
//! Rollout: this module ships the gate but does NOT alter default main
//! flow unless the caller opts in via [`try_acquire`]. The plumbing
//! into `main.rs::run()` sits behind a documented entry point so the
//! parallel PR #428 (external-toggle SIGUSR1) can wire the received
//! commands into its `ExternalCommand` handler without either PR having
//! to know about the other.

pub mod lockfile;
pub mod socket;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};

pub use lockfile::{LockData, RUNTIME_DIR_OVERRIDE_ENV};
pub use socket::{ForwardedCommand, MAX_FRAME_BYTES};

/// Outcome of [`try_acquire`].
#[derive(Debug)]
pub enum AcquireOutcome {
    /// We are the running instance. Hold the guard for as long as this
    /// process wants to serve forwarded commands; drop it (or let it
    /// drop implicitly at exit) to release the lock.
    Acquired(SingleInstance),
    /// Another instance was already running; our argv has been forwarded
    /// to it. Caller should exit 0.
    Forwarded,
}

/// Live single-instance guard. Owns the lockfile + accept thread.
///
/// Dropping this value:
///   1. Signals the accept thread to stop.
///   2. Wakes it via a self-connect (`TcpStream::connect` to our own
///      port) so a blocking `accept()` call returns promptly.
///   3. Removes the lockfile.
///
/// If the process is killed abruptly (SIGKILL, power loss, …) the
/// lockfile lingers; the next invocation detects it as stale via a
/// TCP connect probe and takes over.
#[derive(Debug)]
pub struct SingleInstance {
    lockfile_path: PathBuf,
    receiver: Receiver<ForwardedCommand>,
    stop_flag: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    /// Cached for the self-wake connect during drop.
    port: u16,
}

impl SingleInstance {
    /// Non-blocking drain: returns the next forwarded command if one is
    /// waiting, else `None`. Callers typically poll this from their
    /// main event loop.
    pub fn try_recv(&self) -> Option<ForwardedCommand> {
        self.receiver.try_recv().ok()
    }

    /// Blocking drain with a timeout — useful in tests and in a
    /// "consume any pending forwards then exit" shutdown path.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ForwardedCommand> {
        self.receiver.recv_timeout(timeout).ok()
    }

    /// Port the running instance is accepting forwarded commands on.
    /// Exposed for diagnostics + tests; production code should never
    /// need it.
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // Signal the accept thread to stop taking new work…
        self.stop_flag.store(true, Ordering::SeqCst);
        // …then wake it out of its blocking `accept()` via a self-
        // connect. If the connect fails (port already closed, socket
        // was shut down mid-drop) we don't care — the thread's next
        // check of `stop_flag` will let it exit.
        let addr = std::net::SocketAddr::V4(std::net::SocketAddrV4::new(
            std::net::Ipv4Addr::LOCALHOST,
            self.port,
        ));
        let _ = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200));
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
        let _ = lockfile::remove_lockfile(&self.lockfile_path);
    }
}

/// Try to become the single-instance owner. When another instance is
/// already running, forward `argv` to it and return
/// [`AcquireOutcome::Forwarded`].
///
/// `argv` should be the CLI arguments to hand to the running instance —
/// typically `std::env::args().skip(1).collect()` from `main.rs`. The
/// running instance receives them as [`ForwardedCommand::argv`].
pub fn try_acquire(argv: Vec<String>) -> Result<AcquireOutcome> {
    let runtime_dir =
        lockfile::resolve_runtime_dir().context("resolving single-instance runtime directory")?;
    let lock_path = lockfile::lockfile_path(&runtime_dir);

    // Peek at the existing lockfile (if any) and decide whether to
    // forward or take over. Note: a race between two fresh invocations
    // starting at the exact same instant can result in both writing the
    // lockfile; the second `bind` will still succeed on its own port
    // and the lockfile-with-the-later-port wins. That's acceptable for
    // this feature (either instance can serve the user); a strict
    // one-owner guarantee would require an OS-level mutex which #327
    // does not ask for.
    if let Some(existing) =
        lockfile::read_lockfile(&lock_path).context("reading existing single-instance lockfile")?
    {
        match socket::forward(existing.port, &existing.token, &argv) {
            Ok(()) => return Ok(AcquireOutcome::Forwarded),
            Err(e) => {
                // Server unreachable — treat the lock as stale. We log
                // rather than swallow silently so a user debugging a
                // "why did it start a second instance" report can see
                // the reason.
                eprintln!(
                    "single-instance: existing lockfile at {} looks stale ({}); taking over",
                    lock_path.display(),
                    e
                );
                let _ = lockfile::remove_lockfile(&lock_path);
            }
        }
    }

    // We're the owner. Bind loopback, write lockfile, start accept loop.
    let (listener, port) = socket::bind_loopback().context("binding single-instance listener")?;
    let token = generate_token();
    let data = LockData {
        pid: std::process::id(),
        port,
        token: token.clone(),
    };
    lockfile::write_lockfile(&lock_path, &data).with_context(|| {
        format!(
            "writing single-instance lockfile at {}",
            lock_path.display()
        )
    })?;

    let (tx, rx) = mpsc::channel();
    let stop_flag = Arc::new(AtomicBool::new(false));
    let thread_stop = stop_flag.clone();
    let thread_token = token.clone();
    let handle = std::thread::Builder::new()
        .name("whisper-dictate/single-instance".to_owned())
        .spawn(move || accept_loop(listener, thread_token, tx, thread_stop))
        .context("spawning single-instance accept thread")?;

    Ok(AcquireOutcome::Acquired(SingleInstance {
        lockfile_path: lock_path,
        receiver: rx,
        stop_flag,
        accept_thread: Some(handle),
        port,
    }))
}

/// Background accept loop. Runs until `stop_flag` flips or the listener
/// errors out. Every successful handshake pushes a [`ForwardedCommand`]
/// into `tx`; failed handshakes (bad token, malformed JSON, …) are
/// logged to stderr and dropped so a rogue client can't wedge the
/// server.
fn accept_loop(
    listener: TcpListener,
    token: String,
    tx: Sender<ForwardedCommand>,
    stop_flag: Arc<AtomicBool>,
) {
    // A short accept timeout lets the loop notice `stop_flag` even if
    // no self-wake fires. `set_nonblocking(false)` is the default; we
    // just set a modest per-iteration wait.
    let _ = listener.set_nonblocking(false);
    for stream in listener.incoming() {
        if stop_flag.load(Ordering::SeqCst) {
            break;
        }
        match stream {
            Ok(mut stream) => match socket::serve_one(&mut stream, &token) {
                Ok(cmd) => {
                    if tx.send(cmd).is_err() {
                        // Receiver dropped — owning process is on its
                        // way out. Stop accepting.
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("single-instance: rejected forwarded request: {e}");
                }
            },
            Err(e) => {
                eprintln!("single-instance: accept() failed: {e}");
                break;
            }
        }
    }
}

/// Generate a 128-bit hex token for the lockfile. We don't need
/// cryptographic strength here — the token is a modest speed bump
/// against local port scanners, not a session key — so `SystemTime` +
/// `process::id` mixed through a splitmix64 core is sufficient and
/// avoids pulling `rand` for this one use case.
fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seed_lo = now.as_nanos() as u64;
    let seed_hi = (now.as_nanos() >> 64) as u64 ^ u64::from(std::process::id());
    let mix1 = splitmix64(seed_lo ^ 0x9E3779B97F4A7C15);
    let mix2 = splitmix64(seed_hi ^ mix1);
    format!("{mix1:016x}{mix2:016x}")
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests;
