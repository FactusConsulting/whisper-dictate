//! Pure-logic Rust port of the user-facing `--record-corpus-item` helpers
//! (Wave 6 of the Python-removal roadmap, #348).
//!
//! Audio capture itself is shell-out: [`handle_corpus_record`] dispatches to the
//! Python worker via the existing [`runtime::record_corpus_item_command`] (the
//! same worker mode the System tab's "Record" button already drives). The mic
//! open path reuses the negotiated `vp_capture` machinery — porting that here
//! would mean re-implementing the WASAPI→DirectSound→MME fallback matrix and the
//! Blue-Yeti-friendly native-rate resampler, which is far out of scope for a
//! user tool that runs once per corpus item.
//!
//! What lives here, ported with full Rust unit-test coverage:
//!
//!   * [`is_safe_corpus_id`] — the strict `[A-Za-z0-9._-]` allowlist that gates
//!     the `<appdata>/benchmark/audio/<id>.wav` filename stem. Must stay in
//!     lockstep with `whisper_dictate.vp_corpus_record.is_safe_corpus_id` AND
//!     `ui::corpus::is_safe_corpus_id` (the picker side). [`handle_corpus_record`]
//!     rejects an unsafe id BEFORE shelling out so a crafted CLI invocation
//!     never reaches the worker — defence in depth on top of the worker's own
//!     guard.
//!   * [`compute_record_seconds`] — the chars/12 ⤍ [8, 90] + 2s lead-in
//!     duration heuristic. Pure so it is unit-testable in isolation; identical
//!     output to the Python implementation so the start-event "seconds" field is
//!     unchanged whether the recorder was launched from the UI or the CLI.

use anyhow::{anyhow, Result};

use crate::runtime;

/// Speaking-pace heuristic: ~12 reference characters per spoken second (a
/// relaxed, read-aloud cadence). Mirrors `_CHARS_PER_SECOND` in
/// `vp_corpus_record`.
const CHARS_PER_SECOND: f64 = 12.0;
/// Minimum recording body length in seconds (excluding the lead-in). A
/// one-liner still gets a usable window.
const MIN_RECORD_S: f64 = 8.0;
/// Maximum recording body length in seconds (excluding the lead-in). A long
/// paragraph never runs away.
const MAX_RECORD_S: f64 = 90.0;
/// Fixed lead-in added on top so the user has time to start speaking after the
/// countdown begins. Identical on every recording regardless of body length.
const LEAD_IN_S: f64 = 2.0;

/// Whether `id` is safe to use as the `<appdata>/benchmark/audio/<id>.wav`
/// filename stem.
///
/// Allows `[A-Za-z0-9._-]` only; rejects empty strings, `.`, `..`, and any
/// value containing a path separator (`/` or `\`). Mirrors the Python
/// `vp_corpus_record.is_safe_corpus_id` AND the picker-side
/// `ui::corpus::is_safe_corpus_id` so a crafted manifest or a crafted CLI
/// argument cannot escape the benchmark audio directory.
pub fn is_safe_corpus_id(id: &str) -> bool {
    if id.is_empty() || id == "." || id == ".." {
        return false;
    }
    if id.contains('/') || id.contains('\\') {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Recording length (s) for `text`: chars/12 clamped to [8, 90] + 2s lead-in.
///
/// The body length is clamped BEFORE the fixed lead-in is added, so the clamp
/// bounds the *speaking* window and every recording gets the same head start
/// regardless of length — same shape as Python's `compute_record_seconds`.
pub fn compute_record_seconds(text: &str) -> f64 {
    let chars = text.trim().chars().count() as f64;
    let body = (chars / CHARS_PER_SECOND).clamp(MIN_RECORD_S, MAX_RECORD_S);
    body + LEAD_IN_S
}

/// CLI entry point for `whisper-dictate corpus-record <id>`.
///
/// Validates `id` against [`is_safe_corpus_id`] first (so a bogus id never
/// reaches the worker, even though the worker re-checks), then shells out to
/// the existing [`runtime::record_corpus_item_command`] — the same worker
/// command the UI's "Record" button uses, so the resulting JSON event stream
/// and the WAV destination are bit-identical regardless of the launcher.
pub fn handle_corpus_record(id: &str) -> Result<()> {
    let id = id.trim();
    if !is_safe_corpus_id(id) {
        return Err(anyhow!(
            "unsafe corpus id '{id}': allowed chars are [A-Za-z0-9._-]"
        ));
    }
    runtime::run_foreground(&runtime::record_corpus_item_command(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typical_ids_are_safe() {
        for id in ["da-001", "en_002", "item.3", "ABC", "x"] {
            assert!(is_safe_corpus_id(id), "expected safe: {id}");
        }
    }

    #[test]
    fn empty_is_unsafe() {
        assert!(!is_safe_corpus_id(""));
    }

    #[test]
    fn dot_and_dotdot_are_unsafe() {
        assert!(!is_safe_corpus_id("."));
        assert!(!is_safe_corpus_id(".."));
    }

    #[test]
    fn path_separators_are_unsafe() {
        for id in ["a/b", "../evil", "a\\b", "..\\evil"] {
            assert!(!is_safe_corpus_id(id), "expected unsafe: {id}");
        }
    }

    #[test]
    fn disallowed_chars_are_unsafe() {
        for id in ["a b", "id;x", "id!x", "id@x", "id*x"] {
            assert!(!is_safe_corpus_id(id), "expected unsafe: {id}");
        }
    }

    #[test]
    fn short_text_clamps_to_minimum_plus_lead_in() {
        assert_eq!(compute_record_seconds("Hi"), 10.0);
    }

    #[test]
    fn long_text_clamps_to_maximum_plus_lead_in() {
        let long = "x".repeat(5000);
        assert_eq!(compute_record_seconds(&long), 92.0);
    }

    #[test]
    fn mid_length_text_uses_chars_over_twelve() {
        // 240 chars / 12 = 20s (within [8, 90]) + 2s lead-in.
        let text = "x".repeat(240);
        assert_eq!(compute_record_seconds(&text), 22.0);
    }

    #[test]
    fn empty_text_still_gets_minimum_window() {
        assert_eq!(compute_record_seconds(""), 10.0);
    }

    #[test]
    fn trimmed_text_does_not_inflate_length() {
        // Surrounding whitespace is ignored (matches Python's `.strip()`).
        assert_eq!(
            compute_record_seconds("   hi   "),
            compute_record_seconds("hi"),
        );
    }

    #[test]
    fn unsafe_id_short_circuits_before_shell_out() {
        let err = handle_corpus_record("../evil").unwrap_err();
        assert!(err.to_string().contains("unsafe corpus id"));
    }
}
