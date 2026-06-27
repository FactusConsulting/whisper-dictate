//! Capture-clip skip-gating: mirror of `Dictate._should_skip_pcm`.
//!
//! Pure decision logic — given the captured buffer's size, the user's
//! `min_record_seconds` setting and the active backend, return whether
//! the clip should be discarded before transcription. Python's
//! `src/python/whisper_dictate/vp_dictate.py::Dictate._should_skip_pcm`
//! is the canonical reference; this module mirrors its decision tree
//! verbatim so the Wave 8 Rust supervisor can drop the Python helper.
//!
//! Wave 8 of #348 removed the NeMo/Parakeet backend, so the
//! Parakeet-specific `recording_s < parakeet_min_seconds` gate that
//! Python's `_should_skip_pcm` carried is gone — only the generic
//! `len(pcm) < SR * min_seconds` clamp remains.
//!
//! See `src/python/tests/test_dictate.py::ShouldSkipPcmTests` for the
//! characterisation cases. The unit tests in this module mirror them
//! one-to-one so a regression in either implementation is caught here.

/// Sample rate baked into the Python `vp_dictate` capture gate (16 kHz —
/// the rate the whisper model consumes). Pinned because
/// `_should_skip_pcm` compares `len(pcm) < SR * min_seconds`.
const SR: usize = 16_000;

/// Absolute misfire floor (seconds) enforced regardless of the user's
/// `min_record_seconds` setting. A user setting 0 still gets this
/// protection via `max(0.3, ...)` (mirrors the Python clamp).
pub const MIN_RECORD_FLOOR_S: f64 = 0.3;

/// Outcome of a skip-gate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipDecision {
    /// Clip is acceptable; proceed to transcription.
    Keep,
    /// Clip is too short (below `min_record_seconds` clamped to the
    /// 0.3 s misfire floor). Maps to the Python `"too_short"` reason.
    TooShort,
}

impl SkipDecision {
    /// Reason token surfaced via worker events. Mirrors the Python
    /// `_should_skip_pcm` return value (the falsy `None` for keep,
    /// `"too_short"` for the short-clip rejection).
    pub fn reason(&self) -> Option<&'static str> {
        match self {
            Self::Keep => None,
            Self::TooShort => Some("too_short"),
        }
    }

    /// Hint shown to the user when the clip is dropped. Mirrors the
    /// stdout line `_should_skip_pcm` prints — kept here so a Rust-side
    /// caller can emit the same wording without duplicating the branch.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::Keep => None,
            Self::TooShort => Some("too short — hold the key while you speak"),
        }
    }
}

/// Decide whether the captured clip should be dropped.
///
/// * `samples` — length of the captured mono buffer (post-resample to 16 kHz).
/// * `min_record_seconds` — user setting (`"min_record_seconds"` in config);
///   clamped up to [`MIN_RECORD_FLOOR_S`] so a misconfigured 0 still drops
///   sub-300 ms misfires.
///
/// The Parakeet-specific `recording_s < parakeet_min_seconds` gate that
/// Python carried alongside the generic too-short check is gone with the
/// backend (Wave 8 of #348); the corresponding parameters are no longer
/// part of the API.
pub fn should_skip(samples: usize, min_record_seconds: f64) -> SkipDecision {
    // Mirror Python's `max(0.3, getattr(self, "min_record_seconds", 0.5))`.
    let min_seconds = if min_record_seconds.is_nan() || min_record_seconds < MIN_RECORD_FLOOR_S {
        MIN_RECORD_FLOOR_S
    } else {
        min_record_seconds
    };
    // `len(pcm) < SR * min_seconds` — same comparison Python performs against
    // the channel-selected, post-resample int16 buffer. We compare as `f64`
    // (rather than truncating `SR * min_seconds` to `usize`) so a fractional
    // threshold like `0.50001` rejects a clip at the truncated sample count
    // the same way Python does — otherwise we'd accept clips Python drops.
    if (samples as f64) < (SR as f64) * min_seconds {
        return SkipDecision::TooShort;
    }
    SkipDecision::Keep
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cases here mirror src/python/tests/test_dictate.py::ShouldSkipPcmTests
    // — same buffer sizes, same `min_record_seconds` values.

    #[test]
    fn too_short_capture_is_skipped() {
        let d = should_skip(1000, 0.5);
        assert_eq!(d, SkipDecision::TooShort);
        assert_eq!(d.reason(), Some("too_short"));
    }

    #[test]
    fn long_enough_whisper_capture_is_kept() {
        assert_eq!(should_skip(16_000, 0.5), SkipDecision::Keep);
    }

    #[test]
    fn min_record_seconds_drops_clip_below_setting() {
        // 0.45 s clip (7200 samples @ 16 kHz) dropped at the default 0.5 floor.
        assert_eq!(should_skip(7_200, 0.5), SkipDecision::TooShort);
    }

    #[test]
    fn min_record_seconds_passes_clip_at_lower_setting() {
        // Same 0.45 s clip passes when min_record_seconds is lowered to 0.3.
        assert_eq!(should_skip(7_200, 0.3), SkipDecision::Keep);
    }

    #[test]
    fn min_record_seconds_floor_clamps_below_point_three() {
        // Setting 0 still enforces the 0.3 s misfire floor.
        assert_eq!(should_skip(4_000, 0.0), SkipDecision::TooShort);
        assert_eq!(should_skip(5_600, 0.0), SkipDecision::Keep);
    }

    #[test]
    fn nan_min_record_falls_back_to_floor() {
        // Defensive: a NaN slipping through from a bad config still clamps
        // to the 0.3 s floor instead of accidentally accepting everything.
        assert_eq!(should_skip(4_000, f64::NAN), SkipDecision::TooShort);
    }

    #[test]
    fn fractional_min_record_seconds_drops_clip_at_truncated_sample_count() {
        // Regression for PR #359: a non-integral threshold like 0.50001
        // produces `SR * min_seconds = 8000.16`. Python's
        // `len(pcm) < SR * min_seconds` (float comparison) drops a clip of
        // exactly 8000 samples; truncating the threshold to `usize` (8000)
        // would have kept that clip.
        assert_eq!(should_skip(8_000, 0.50001), SkipDecision::TooShort);
        // One sample over the float threshold (8001) is still below 8000.16
        // when compared as float? Actually 8001 > 8000.16, so it's kept.
        assert_eq!(should_skip(8_001, 0.50001), SkipDecision::Keep);
    }

    #[test]
    fn keep_decision_has_no_reason_or_hint() {
        let d = SkipDecision::Keep;
        assert!(d.reason().is_none());
        assert!(d.hint().is_none());
    }

    #[test]
    fn parakeet_backend_no_longer_alters_decision() {
        // Wave 8 of #348 dropped Parakeet, so the per-backend
        // `recording_s < parakeet_min_seconds` gate that lived here is
        // gone — the same length must produce the same decision for what
        // used to be every backend. Pin this so a future caller that
        // accidentally reintroduces a backend-specific branch fails here.
        assert_eq!(should_skip(16_000, 0.5), SkipDecision::Keep);
        assert_eq!(should_skip(32_000, 0.5), SkipDecision::Keep);
    }
}
