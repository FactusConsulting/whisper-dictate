//! Stable facade for platform text injection and the JSON CLI envelope.
//!
//! [`Injector`] owns its lazy keyboard backend. [`InjectMethod`] preserves
//! explicit paste preferences, while [`InjectOutcome`] reports partial
//! progress so callers do not retry text that may already have been typed.
//! The Linux helper chain and request/response reporting have separate owners.

#[cfg(target_os = "linux")]
mod chain;
mod engine;
mod outcome;
mod protocol;
mod stdio;

pub use engine::{InjectMethod, Injector};
pub use outcome::InjectOutcome;
#[allow(unused_imports)]
pub(crate) use protocol::resolve_method;
pub use protocol::{InjectMethodSpec, InjectMode, InjectRequest, InjectResponse, ProbeResponse};
pub use stdio::handle_inject;

#[cfg(test)]
#[path = "dispatcher_tests.rs"]
mod tests;
