//! Cross-platform host integration seams the dictate engine needs but that
//! don't cleanly belong to any of the existing feature-gated modules.
//!
//! Today this houses the [`foreground_window`] probe, which supplies the
//! title/process pair the per-utterance target-profile matcher (Python parity
//! for `vp_events._apply_profile_settings` -> `_capture_target_window`) uses
//! to pick per-app dictation settings. See the submodule for the per-OS
//! backends.

pub mod foreground_window;
