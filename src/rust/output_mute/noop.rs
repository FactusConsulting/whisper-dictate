//! No-op output-mute backend: never touches the OS.
//!
//! Used on platforms where issue #322 deferred a real implementation
//! (macOS) and as the default fixture in unit tests that do not care
//! whether OS commands were emitted. Also handy in local test doubles:
//! wrap it in a small counter if a test wants to prove the controller
//! did NOT try to mute.

use crate::output_mute::{MuteError, OutputMuteBackend};

/// Backend that pretends the output is never muted and swallows
/// every `set_mute` call.
///
/// Constructing it is free and has no external side-effects, so it is
/// safe to use as the default in tests and as the macOS fallback.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopBackend;

impl OutputMuteBackend for NoopBackend {
    fn get_mute(&self) -> Result<bool, MuteError> {
        Ok(false)
    }

    fn set_mute(&self, _muted: bool) -> Result<(), MuteError> {
        Ok(())
    }
}
