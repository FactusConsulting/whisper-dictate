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
//! # What is ported
//!
//! Python's whole-text `is_hallucination` gate checks two things, BOTH
//! ported here:
//!
//! 1. **Exact blacklist match** — the lowercased / rstripped text is in
//!    `HALLUCINATIONS` (data:
//!    `whisper_dictate/data/hallucination_patterns.json::exact_blacklist`).
//! 2. **Anchored credit regex** — the whole (trimmed, lowercased) text
//!    matches `_CREDIT_RE`, assembled by `_build_credit_re` from
//!    `credit_phrase_prefixes` + the shared `credit_phrase_year_tail` and
//!    the `bare_company_names` branches. A phrase prefix must carry a
//!    trailing year, so real dictation that merely starts with "tekster
//!    af …" survives.
//!
//! Only the **year-less prefix** regex (`_looks_like_credit_prefix`)
//! stays Python-only: it is loose (matches real dictation) and is used
//! solely in Python's SEGMENT-level `_drop_hallucinated_segments`, which
//! needs faster-whisper segment metadata (`no_speech_prob`, per-segment
//! rate) the session's whole-text gate never sees. The whole-text gate
//! this module implements is exactly what the Rust session consumes.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;

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

/// Credit-phrase prefixes, ported verbatim from
/// `hallucination_patterns.json::credit_phrase_prefixes`. Each is a regex
/// fragment; the shared [`CREDIT_PHRASE_YEAR_TAIL`] is appended to the
/// whole alternation (a prefix only counts as a credit WITH a trailing
/// year, so "tekster af høj kvalitet" survives). MUST stay in sync with
/// the JSON data file.
const CREDIT_PHRASE_PREFIXES: &[&str] = &[
    r"(?:danske |norske |svenske )?(?:under)?tekster (?:af|by|:)",
    r"tekstet af ",
    r"oversat af ",
    r"subtitles? by ",
    r"subtitled by ",
    r"captions? by ",
    r"translated by ",
];

/// Shared year tail appended once to the whole prefix alternation. Ported
/// verbatim from `hallucination_patterns.json::credit_phrase_year_tail`.
const CREDIT_PHRASE_YEAR_TAIL: &str = r".{0,60}\b(?:19|20)\d{2}";

/// Bare company-name credits, each an independent branch carrying its own
/// optional year. Ported verbatim from
/// `hallucination_patterns.json::bare_company_names`.
const BARE_COMPANY_NAMES: &[&str] = &[
    r"scandinavian text service(?: (?:19|20)\d{2})?",
    r"broadcast text international(?: (?:19|20)\d{2})?",
    r"dansk video ?tekst(?: (?:19|20)\d{2})?",
];

/// The anchored subtitle-credit regex, built once. Mirrors Python's
/// `_build_credit_re`: `^(?:(?:<prefixes>)<year_tail>|<company1>|…)[\s.!?]*$`,
/// case-insensitive.
fn credit_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let phrase_group = CREDIT_PHRASE_PREFIXES.join("|");
        let mut branches = vec![format!("(?:{phrase_group}){CREDIT_PHRASE_YEAR_TAIL}")];
        branches.extend(BARE_COMPANY_NAMES.iter().map(|c| (*c).to_owned()));
        let body = branches.join("|");
        Regex::new(&format!(r"(?i)^(?:{body})[\s.!?]*$")).expect("credit regex is valid")
    })
}

/// `true` when the WHOLE (trimmed, lowercased) text is a subtitle/caption
/// credit. Mirrors Python's `_looks_like_credit`
/// (`_CREDIT_RE.match(text.strip().lower())`).
fn looks_like_credit(text: &str) -> bool {
    credit_re().is_match(text.trim().to_lowercase().as_str())
}

/// `true` iff `text` is a hallucinated subtitle/caption credit: either on
/// the exact-match blacklist OR matching the anchored credit regex.
///
/// Full port of Python's whole-text `vp_transcribe.is_hallucination`
/// (`text.lower().rstrip() in HALLUCINATIONS or _looks_like_credit(text)`).
/// The blacklist check lowercases (`str::to_lowercase`, Unicode-aware like
/// Python) and right-trims only, preserving leading whitespace exactly like
/// Python's `rstrip`; the credit check trims both ends like Python's
/// `strip`. Callers that hold a run of segment text should normalise it
/// first (the local backend runs `normalize_whitespace`, the cloud backend
/// trims the endpoint's text).
pub fn is_hallucination(text: &str) -> bool {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    let set = SET.get_or_init(|| EXACT_BLACKLIST.iter().copied().collect());
    let lowered = text.to_lowercase();
    set.contains(lowered.trim_end()) || looks_like_credit(text)
}

/// `VOICEPI_MAX_CHARS_PER_SECOND` env key.
pub const MAX_CHARS_PER_SECOND_ENV: &str = "VOICEPI_MAX_CHARS_PER_SECOND";
/// Python default (`vp_transcribe.MAX_CHARS_PER_SECOND = 30`).
pub const DEFAULT_MAX_CHARS_PER_SECOND: f64 = 30.0;

/// Read the impossible-speech-rate ceiling from the env, mirroring Python's
/// `VOICEPI_MAX_CHARS_PER_SECOND` module constant. `0` (or negative)
/// disables the guard; unset / blank / unparseable falls back to the
/// default.
pub fn max_chars_per_second_from_env() -> f64 {
    std::env::var(MAX_CHARS_PER_SECOND_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(DEFAULT_MAX_CHARS_PER_SECOND)
}

/// `true` when `text` was produced far too fast to be real speech -- a
/// Whisper hallucination guard. Port of Python's `_speech_rate_exceeded`:
/// `len(text.strip()) / max(duration_s, 0.1) > MAX_CHARS_PER_SECOND`, with
/// `max_cps <= 0` disabling the guard. The caller blanks the transcript on a
/// `true`, so it surfaces as an `empty` no-text event.
pub fn speech_rate_exceeded(text: &str, duration_s: f64, max_cps: f64) -> bool {
    if max_cps <= 0.0 {
        return false;
    }
    let chars = text.trim().chars().count() as f64;
    chars / duration_s.max(0.1) > max_cps
}

#[cfg(test)]
#[path = "hallucination_tests.rs"]
mod tests;
