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

pub fn read_manual_test_readme() -> String {
    let path = repo_root().join("scripts/manual-test/README.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

pub fn read_wayland_smoke() -> String {
    let path = repo_root().join("scripts/integration/wayland-user-smoke.sh");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}
