//! Injection result policy, including the partial-progress retry barrier.

use anyhow::Result;

/// Result of an injection attempt with the extra `partial` signal.
///
/// `partial: true` means at least one keystroke reached the compositor
/// before the failure. Any outer fallback or retry loop must not re-inject
/// the same text -- doing so would silently double-type the successful
/// prefix into the user's document.
#[derive(Debug)]
pub struct InjectOutcome {
    pub result: Result<()>,
    pub partial: bool,
}

impl InjectOutcome {
    pub fn ok() -> Self {
        Self {
            result: Ok(()),
            partial: false,
        }
    }

    pub fn failed(err: anyhow::Error) -> Self {
        Self {
            result: Err(err),
            partial: false,
        }
    }

    /// Failure that landed a partial prefix. Outer fallbacks must NOT
    /// re-run the injection or the prefix will be double-typed.
    pub fn partial(err: anyhow::Error) -> Self {
        Self {
            result: Err(err),
            partial: true,
        }
    }

    /// Bridge an existing `Result<()>` into an [`InjectOutcome`] with
    /// `partial=false`. Used by paths that cannot observe partial
    /// progress (enigo on Windows/macOS, injected trait-object backends).
    pub fn from_result(result: Result<()>) -> Self {
        match result {
            Ok(()) => Self::ok(),
            Err(err) => Self::failed(err),
        }
    }
}

#[cfg(test)]
#[path = "outcome_tests.rs"]
mod tests;
