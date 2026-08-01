//! Shared lock for tests that mutate the diagnostic writer or log level.
//! Both values are read by each log call, so tests must change them under
//! one guard to remain deterministic when the suite runs in parallel.
//!
//! **Usage rule:** every `#[test]` in the library that either
//! - installs a diagnostic writer
//!   ([`crate::diag::install_gui_diagnostic_log`]), **or**
//! - mutates the resolved log level
//!   ([`crate::diag::init_from_env`],
//!   [`crate::diag::reset_level_for_tests`], or setting
//!   `VOICEPI_LOG` and expecting the sink to observe it),
//!
//! MUST take [`DIAG_WRITER_LOCK`] across the install / mutate /
//! log / read window. Mirrors the [`crate::test_env_lock::ENV_LOCK`]
//! discipline for env-var mutation.
//!
//! Integration tests in `tests/` live in their own binaries and so
//! do not need to share this lock with the library suite.

use std::sync::Mutex;

/// Guard used by diagnostic tests while changing writer or level state.
pub(crate) static DIAG_WRITER_LOCK: Mutex<()> = Mutex::new(());
