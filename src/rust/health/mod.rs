//! Per-utterance `[health]` line + 4-level quality grade.
//!
//! The whole module is
//! pure functions over a metrics map (the same field names the `[utterance]`
//! event carries), so it is trivially unit-testable without any audio/model
//! state.
//!
//! Format (single line, ASCII-safe) — the UI parser in
//! `ui/log_render.rs` keys off it:
//!
//! ```text
//! [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq | grade=good
//! ```
//!
//! Submodules:
//! * [`util`] — small JSON-coercion helpers + the segment-mean / band
//!   ties-to-even rounding that mirror the stdlib semantics.
//! * [`grade`] — [`health_grade`], the 4-level verdict.
//! * [`format`] — [`format_health_line`], the user-facing one-line summary.
//!
//! Tests live next to the code they exercise (one `#[cfg(test)] mod tests` per
//! submodule) so each file stays well under the 500-LOC repo limit and remains
//! independently unit-testable.

mod format;
mod grade;
mod util;

pub use format::format_health_line;
pub use grade::{health_grade, GRADE_FAIR, GRADE_GOOD, GRADE_PERFECT, GRADE_POOR};

// Confidence bands over the aggregate avg_logprob (MEAN of the segments'
// avg_logprob, not the min).
pub const CONFIDENCE_HIGH: f64 = -0.35;
pub const CONFIDENCE_OK: f64 = -0.60;

// Graduated health grade thresholds (signal-to-noise ratio, in dB).
pub const HEALTH_SNR_POOR: f64 = 6.0;
pub const HEALTH_SNR_GOOD: f64 = 20.0;
pub const HEALTH_SNR_PERFECT: f64 = 30.0;
