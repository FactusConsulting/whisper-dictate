//! Stock hallucination blacklist filter shared by every transcribe backend.
//!
//! Ported from Python's `vp_transcribe.is_hallucination`. In the Python
//! worker the whole-text hallucination gate runs in the backend-agnostic
//! `vp_dictate.py::_transcribe_pcm` (`if is_hallucination(result.text): ...
//! return None, "no_speech"`), so it applies to BOTH the local Whisper
//! backend AND the cloud (`stt_backend=openai`) backend. This module keeps
//! the filter here — with no cargo-feature gate — so the local
//! ([`super::whisper_local`]) and cloud ([`super::cloud_transcribe`])
//! backends share one implementation and it is unit-tested on every build.
//!
//! # Partial port
//!
//! Python's `is_hallucination` checks two things:
//!
//! 1. **Exact blacklist match** — the lowercased / rstripped text is in
//!    `HALLUCINATIONS` (data:
//!    `whisper_dictate/data/hallucination_patterns.json::exact_blacklist`).
//! 2. **Credit regex match** — the whole text matches one of the
//!    subtitle-credit patterns assembled by `_build_credit_re`.
//!
//! Only (1) is ported here (as in the original local-only port). The
//! credit-regex port is deferred to its own follow-up because porting the
//! regex assembler + the JSON loader is a separate multi-file change. The
//! blacklist is the most common path in practice (every observed false
//! positive on quiet Danish input has been an exact `"tak"`-family match).

use std::collections::HashSet;
use std::sync::OnceLock;

/// Exact-match hallucination blacklist, ported verbatim from
/// `src/python/whisper_dictate/data/hallucination_patterns.json::exact_blacklist`.
///
/// Whisper emits one of these strings on quiet / empty audio — they are
/// subtitle / caption credits the multilingual model picked up from its
/// training set. Matched against `text.to_lowercase().trim_end()` to
/// mirror Python's `text.lower().rstrip()`.
///
/// MUST stay byte-identical to the JSON data file. When the JSON file
/// gains a new entry the same entry must be added here (and a regression
/// test should catch the drift — see [`is_hallucination`]'s tests).
pub(crate) const EXACT_BLACKLIST: &[&str] = &[
    "tak",
    "tak.",
    "tak for din opmærksomhed",
    "tak for din opmærksomhed.",
    "tak fordi du så med",
    "tak fordi du så med.",
    "tak fordi du lyttede med",
    "tak fordi du lyttede med.",
    "tak for at du så med",
    "tak for at du så med.",
    "tak for at i så med",
    "tak for at i så med.",
    "tak fordi i så med",
    "tak fordi i så med.",
    "thank you",
    "thank you.",
    "thank you for watching",
    "thank you for watching.",
    "thank you for listening",
    "thank you for listening.",
    "thanks for watching",
    "thanks for watching.",
    "undertekster af",
    "undertekstet af",
];

/// `true` iff `text` is on the exact-match hallucination blacklist.
///
/// Partial port of Python's `vp_transcribe.is_hallucination` — only the
/// `text.lower().rstrip() in HALLUCINATIONS` branch is implemented (see the
/// module docs for the credit-regex deferral note). Lowercases with
/// `str::to_lowercase` (Unicode-aware, matching Python) and right-trims
/// only, so leading whitespace is preserved exactly like Python's `rstrip`;
/// callers that need a run of segment text normalised first should do so
/// before calling (the local backend runs `normalize_whitespace`, the cloud
/// backend trims the endpoint's text).
pub fn is_hallucination(text: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let set = SET.get_or_init(|| EXACT_BLACKLIST.iter().copied().collect());
    let lowered = text.to_lowercase();
    set.contains(lowered.trim_end())
}

#[cfg(test)]
#[path = "hallucination_tests.rs"]
mod tests;
