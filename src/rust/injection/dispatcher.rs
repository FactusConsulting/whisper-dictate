//! Stable facade for platform text injection.
//!
//! [`Injector`] owns its lazy keyboard backend. [`InjectMethod`] preserves
//! explicit paste preferences, while [`InjectOutcome`] reports partial
//! progress so callers do not retry text that may already have been typed.
//! The Linux helper chain has a separate owner.

#[cfg(target_os = "linux")]
mod chain;
mod engine;
mod outcome;

pub use engine::{InjectMethod, Injector};
pub use outcome::InjectOutcome;

#[cfg(test)]
#[path = "dispatcher_tests.rs"]
mod tests;
