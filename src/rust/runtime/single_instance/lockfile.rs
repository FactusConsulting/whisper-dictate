//! Lockfile format + runtime directory resolution for the single-instance
//! gate.
//!
//! Layout on disk:
//!
//! ```text
//! <runtime_dir>/whisper-dictate.lock
//! ```
//!
//! Contents (single JSON line):
//!
//! ```json
//! {"pid": 12345, "port": 51823, "token": "hex-nonce"}
//! ```
//!
//! `port` is the TCP loopback port the running instance is accepting
//! forwarded commands on; `token` is a per-process nonce that clients
//! must present as the first byte-string of the framed handshake so a
//! rogue local process can't spoof commands (mild defence-in-depth —
//! the socket is bound to 127.0.0.1 anyway).
//!
//! Runtime dir selection mirrors the platform conventions the issue calls
//! out (`$XDG_RUNTIME_DIR` on Linux, `%LOCALAPPDATA%` on Windows, `$TMPDIR`
//! on macOS), with a tempdir fallback so the module still functions on a
//! stripped-down container or CI runner where none of those are set.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Env var callers (and tests) can set to override the resolved runtime
/// directory. When present, wins over every platform default. The tests
/// point this at a per-test tempdir so parallel test runs don't stomp on
/// each other or on a real user daemon.
pub const RUNTIME_DIR_OVERRIDE_ENV: &str = "VOICEPI_SINGLE_INSTANCE_DIR";

/// Filename of the lockfile within the runtime directory. Kept short and
/// stable so it's easy to spot in `ls $XDG_RUNTIME_DIR` when debugging.
pub const LOCKFILE_NAME: &str = "whisper-dictate.lock";

/// On-disk lockfile contents. Serialised as a single JSON object so the
/// file is human-inspectable and future fields can be added without a
/// migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockData {
    /// Process ID that owns the lock.
    pub pid: u32,
    /// TCP loopback port the owning process is accepting forwarded
    /// commands on.
    pub port: u16,
    /// Per-process nonce presented by clients as an authentication token
    /// on the wire. Hex-encoded 128-bit value.
    pub token: String,
}

impl LockData {
    /// Serialise to a single-line JSON string suitable for writing to the
    /// lockfile. Guarantees no trailing newline; callers decide whether
    /// to append one for readability.
    pub fn encode(&self) -> String {
        // The struct only holds primitives + a hex string, so
        // serialisation is infallible in practice; unwrap keeps the
        // caller ergonomics simple.
        serde_json::to_string(self).expect("LockData is always JSON-serialisable")
    }

    /// Parse the on-disk representation. Whitespace / trailing newlines
    /// are tolerated so a lockfile that got edited by hand still loads.
    pub fn decode(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw.trim())
    }
}

/// Resolve the directory the lockfile should live in.
///
/// Order of precedence:
///   1. `$VOICEPI_SINGLE_INSTANCE_DIR` (tests + power-user override).
///   2. `$XDG_RUNTIME_DIR` (Linux desktop convention).
///   3. `%LOCALAPPDATA%\whisper-dictate` (Windows).
///   4. `$TMPDIR` (macOS + fallback for anything else).
///   5. `std::env::temp_dir()` (last resort — always defined).
///
/// The chosen directory is created if it does not exist; if creation
/// fails the error is propagated so the caller can surface it.
pub fn resolve_runtime_dir() -> io::Result<PathBuf> {
    let dir = if let Some(override_dir) =
        std::env::var_os(RUNTIME_DIR_OVERRIDE_ENV).filter(|v| !v.is_empty())
    {
        PathBuf::from(override_dir)
    } else if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        PathBuf::from(xdg)
    } else if cfg!(windows) {
        // %LOCALAPPDATA% is defined on every supported Windows install; the
        // subdirectory keeps our lockfile from colliding with unrelated tools
        // that also drop state at the LocalAppData root.
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            PathBuf::from(local).join("whisper-dictate")
        } else {
            std::env::temp_dir()
        }
    } else if let Some(tmp) = std::env::var_os("TMPDIR").filter(|v| !v.is_empty()) {
        PathBuf::from(tmp)
    } else {
        std::env::temp_dir()
    };

    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Full path to the lockfile inside the resolved runtime dir.
pub fn lockfile_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(LOCKFILE_NAME)
}

/// Read the current lockfile, if any. Returns `Ok(None)` when the file
/// doesn't exist — a fresh install / first launch is a common case and
/// shouldn't surface as an error to the caller.
pub fn read_lockfile(path: &Path) -> io::Result<Option<LockData>> {
    match fs::read_to_string(path) {
        Ok(raw) => match LockData::decode(&raw) {
            Ok(data) => Ok(Some(data)),
            Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Write the lockfile atomically: write to a sibling `.tmp` first, then
/// rename over the target. Rename on the same filesystem is atomic on
/// Unix and Windows (post-NTFS), so a concurrent reader either sees the
/// old content or the new content but never a half-written file.
pub fn write_lockfile(path: &Path, data: &LockData) -> io::Result<()> {
    let tmp = path.with_extension("lock.tmp");
    let payload = format!("{}\n", data.encode());
    fs::write(&tmp, payload)?;
    // On Windows `fs::rename` refuses to overwrite an existing target on
    // older platform SDKs; the ordering here (write-then-rename) means
    // the target only exists if a previous acquire raced with us, in
    // which case a `remove_file` + `rename` gets us across the line.
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
        }
        Err(e) => Err(e),
    }
}

/// Best-effort removal — used on graceful shutdown. Missing file is not
/// an error; other I/O errors are logged by the caller and swallowed
/// because they're not actionable at process exit.
pub fn remove_lockfile(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip_lockdata_encode_decode() {
        let data = LockData {
            pid: 4242,
            port: 51823,
            token: "abcdef0123456789".to_owned(),
        };
        let encoded = data.encode();
        let decoded = LockData::decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn decode_tolerates_trailing_newline_and_whitespace() {
        let raw = "  {\"pid\":1,\"port\":2,\"token\":\"x\"}  \n";
        let decoded = LockData::decode(raw).unwrap();
        assert_eq!(decoded.pid, 1);
        assert_eq!(decoded.port, 2);
        assert_eq!(decoded.token, "x");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(LockData::decode("not-json").is_err());
    }

    #[test]
    fn read_lockfile_returns_none_when_absent() {
        let dir = TempDir::new().unwrap();
        let path = lockfile_path(dir.path());
        assert!(read_lockfile(&path).unwrap().is_none());
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = lockfile_path(dir.path());
        let data = LockData {
            pid: 99,
            port: 12345,
            token: "deadbeef".to_owned(),
        };
        write_lockfile(&path, &data).unwrap();
        let read = read_lockfile(&path).unwrap().unwrap();
        assert_eq!(read, data);
    }

    #[test]
    fn write_overwrites_existing_lockfile() {
        let dir = TempDir::new().unwrap();
        let path = lockfile_path(dir.path());
        let first = LockData {
            pid: 1,
            port: 1,
            token: "a".to_owned(),
        };
        let second = LockData {
            pid: 2,
            port: 2,
            token: "b".to_owned(),
        };
        write_lockfile(&path, &first).unwrap();
        write_lockfile(&path, &second).unwrap();
        assert_eq!(read_lockfile(&path).unwrap().unwrap(), second);
    }

    #[test]
    fn remove_lockfile_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = lockfile_path(dir.path());
        // Missing file is not an error.
        remove_lockfile(&path).unwrap();
        write_lockfile(
            &path,
            &LockData {
                pid: 1,
                port: 1,
                token: "t".to_owned(),
            },
        )
        .unwrap();
        remove_lockfile(&path).unwrap();
        // Second remove is a no-op.
        remove_lockfile(&path).unwrap();
    }

    #[test]
    fn resolve_runtime_dir_honours_override_env() {
        // The crate-wide env-lock (`crate::test_env_lock::ENV_LOCK`)
        // serialises any test that mutates process env vars. Acquire
        // via `unwrap_or_else(|e| e.into_inner())` so a panic in an
        // unrelated env-mutating test poisons the mutex without
        // cascading a `PoisonError` failure here — the pattern
        // documented in `test_env_lock`. Recovering the inner value is
        // safe because `EnvVarGuard`'s Drop restores env state
        // unconditionally.
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        // `EnvVarGuard` restores the pre-existing value (or absence) on
        // Drop; a bare `set_var` + `remove_var` pair leaks the override
        // into every subsequent test on panic (Codex P2 #415).
        let _env = crate::test_env_lock::EnvVarGuard::set(RUNTIME_DIR_OVERRIDE_ENV, dir.path());
        let resolved = resolve_runtime_dir().unwrap();
        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn resolve_runtime_dir_creates_missing_directory() {
        let _guard = crate::test_env_lock::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("nested").join("runtime");
        assert!(!nested.exists());
        let _env = crate::test_env_lock::EnvVarGuard::set(RUNTIME_DIR_OVERRIDE_ENV, &nested);
        let resolved = resolve_runtime_dir().unwrap();
        assert_eq!(resolved, nested);
        assert!(nested.is_dir());
    }
}
