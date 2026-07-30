//! Cross-platform host integration seams the dictate engine needs but that
//! don't cleanly belong to any of the existing feature-gated modules.
//!
//! This houses the [`foreground_window`] probe used for per-utterance target
//! profile matching and the [`window_enumeration`] service used by the
//! Profiles settings picker.

pub mod foreground_window;
pub mod window_enumeration;
