//! Shared helpers for the text-artifact regression tests.
//!
//! `manual_test_docs.rs` and `wayland_smoke_guard.rs` are separate integration
//! test binaries that both read repo-root text artifacts (the manual-test
//! README and the canonical Wayland smoke script). Cargo compiles
//! `tests/common/mod.rs` into each of them rather than as a test target of its
//! own, so every helper is "unused" from the perspective of at least one
//! binary -- hence the crate-wide allow below.

#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

/// Repo root, resolved from the crate manifest dir.
///
/// Cargo runs integration tests with CWD = the manifest dir; these tests live
/// at `src/rust/tests/`, so the manifest is `src/rust/` and the docs / scripts
/// under test live two levels up.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("resolve repo root from CARGO_MANIFEST_DIR")
}

/// Run `op` up to `attempts` times with a short linear backoff, returning the
/// first success or the last error's message. The devcontainer bind mount
/// (Rancher Desktop) intermittently fails transient filesystem reads while
/// the full suite's concurrent processes churn, so policy tests that shell
/// out to git retry instead of failing their whole run on a first-try
/// guarantee they never had.
pub fn with_git_retries<T>(
    attempts: u32,
    mut op: impl FnMut() -> std::io::Result<T>,
) -> Result<T, String> {
    let mut last_err = None;
    for attempt in 1..=attempts {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_err = Some(err);
                if attempt < attempts {
                    std::thread::sleep(std::time::Duration::from_millis(100 * u64::from(attempt)));
                }
            }
        }
    }
    Err(last_err
        .map(|err| err.to_string())
        .unwrap_or_else(|| "no attempts configured".to_owned()))
}

pub fn read_manual_test_readme() -> String {
    let path = repo_root().join("scripts/manual-test/README.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub fn read_wayland_smoke() -> String {
    let path = repo_root().join("scripts/integration/wayland-user-smoke.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}
