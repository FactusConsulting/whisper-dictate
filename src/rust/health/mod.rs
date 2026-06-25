//! Per-utterance `[health]` line + 4-level quality grade.
//!
//! Rust port of `src/python/whisper_dictate/vp_health.py`. The whole module is
//! pure functions over a metrics map (the same field names the `[utterance]`
//! event carries), so it is trivially unit-testable without any audio/model
//! state. The Python side may keep its own implementation for now and call this
//! one via the JSON-over-stdin `health` sub-command — Phase 2 keeps both
//! implementations available during the validation period.
//!
//! Format (single line, ASCII-safe), preserved verbatim from Python so the UI
//! parser in `ui/log_render.rs` keeps working:
//!
//! ```text
//! [health] mic -38dBFS SNR 56dB good | confidence high (-0.13) | post clean/groq | grade=good
//! ```
//!
//! Submodules:
//! * [`util`] — small JSON-coercion helpers + the segment-mean / band /
//!   ties-to-even rounding that mirror Python's stdlib semantics.
//! * [`grade`] — [`health_grade`], the 4-level verdict.
//! * [`format`] — [`format_health_line`], the user-facing one-line summary.
//!
//! Tests live next to the code they exercise (one `#[cfg(test)] mod tests` per
//! submodule) so each file stays well under the 500-LOC repo limit and remains
//! independently unit-testable.

use std::io::{self, Read};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod format;
mod grade;
mod util;

pub use format::format_health_line;
pub use grade::{health_grade, GRADE_FAIR, GRADE_GOOD, GRADE_PERFECT, GRADE_POOR};

// Confidence bands over the aggregate avg_logprob (MEAN of the segments'
// avg_logprob, not the min — matches Python).
pub const CONFIDENCE_HIGH: f64 = -0.35;
pub const CONFIDENCE_OK: f64 = -0.60;

// Graduated health grade thresholds (signal-to-noise ratio, in dB).
pub const HEALTH_SNR_POOR: f64 = 6.0;
pub const HEALTH_SNR_GOOD: f64 = 20.0;
pub const HEALTH_SNR_PERFECT: f64 = 30.0;

/// JSON request envelope for the `health` sub-command.
#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum HealthRequest {
    /// Render the one-line `[health]` summary.
    FormatLine { metrics: Value },
    /// Fold every per-utterance signal into one 4-level quality grade.
    Grade { metrics: Value },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthLineResponse {
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthGradeResponse {
    pub grade: String,
}

/// Handler for the hidden `health` sub-command. Reads a JSON request on stdin
/// and writes a JSON response on stdout — the same pattern as the other
/// helpers (`redact-text`, `apply-profile`, `privacy`).
pub fn handle_health() -> Result<()> {
    let request = read_request()?;
    match request {
        HealthRequest::FormatLine { metrics } => {
            let response = HealthLineResponse {
                line: format_health_line(&metrics),
            };
            println!("{}", serde_json::to_string(&response)?);
        }
        HealthRequest::Grade { metrics } => {
            let response = HealthGradeResponse {
                grade: health_grade(&metrics).to_owned(),
            };
            println!("{}", serde_json::to_string(&response)?);
        }
    }
    Ok(())
}

fn read_request() -> Result<HealthRequest> {
    let mut raw = String::new();
    io::stdin().read_to_string(&mut raw)?;
    Ok(serde_json::from_str(&raw)?)
}
