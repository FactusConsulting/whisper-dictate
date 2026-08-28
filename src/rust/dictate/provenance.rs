//! Engine / STT-implementation provenance vocabulary shared by the
//! utterance record, the metrics + history sinks, and the startup
//! diagnostic line.
//!
//! # The ambiguity this closes
//!
//! An utterance record used to carry only the *configured* stack:
//!
//! ```json
//! {"compute_type":"int8_float16","real_time_factor":0.23,"compute_ms":351,
//!  "model":"large-v3-turbo","stt_backend":"whisper","device":"auto"}
//! ```
//!
//! `stt_backend` names the backend the user selected, not the native
//! implementation or accelerator that actually ran.
//!
//! Three fields close that ambiguity, and this module owns their vocabulary:
//!
//! * `engine` -- which runtime served the utterance
//!   ([`ENGINE_RUST_IN_PROCESS`]).
//! * `stt_impl` -- the transcription implementation that actually ran
//!   ([`STT_IMPL_WHISPER_CPP`], [`STT_IMPL_CLOUD_OPENAI`], [`STT_IMPL_CLOUD_GROQ`],
//!   [`STT_IMPL_CLOUD_CUSTOM`]).
//! * `stt_accel` -- the compute path it actually used, from
//!   [`crate::whisper::accel`] (whisper.cpp's own model-load verdict),
//!   NOT from the `device` setting.
//!
//! All labels are lowercase ASCII with no spaces so they survive the
//! console-ASCII guard and stay greppable in JSONL rows.

/// `engine` value for the in-process Rust dictation session
/// (`VOICEPI_DICTATE_BACKEND=rust-session`): capture, transcription and
/// injection all inside `whisper-dictate.exe`.
pub const ENGINE_RUST_IN_PROCESS: &str = "rust-in-process";

/// `stt_impl` value for whisper.cpp (via the `whisper-rs` bindings),
/// running in-process in the Rust session.
pub const STT_IMPL_WHISPER_CPP: &str = "whisper.cpp";

/// `stt_impl` value for an OpenAI `/audio/transcriptions` endpoint.
pub const STT_IMPL_CLOUD_OPENAI: &str = "cloud-openai";

/// `stt_impl` value for Groq's OpenAI-compatible endpoint. Split from
/// [`STT_IMPL_CLOUD_OPENAI`] because they are different services with
/// different models and failure modes, and the `stt_backend` setting
/// spells both `openai`.
pub const STT_IMPL_CLOUD_GROQ: &str = "cloud-groq";

/// `stt_impl` value for NVIDIA Nemotron ASR served by an NIM endpoint.
pub const STT_IMPL_CLOUD_NEMOTRON: &str = "cloud-nemotron";

/// `stt_impl` value for NVIDIA Nemotron 3.5 decoded in-process through the
/// official NeMo-Speech.cpp C ABI.
pub const STT_IMPL_NEMOTRON_LOCAL: &str = "nemotron.cpp";

/// `stt_impl` value for any OTHER OpenAI-compatible endpoint: a
/// self-hosted server on localhost, Azure OpenAI, a proxy, or whatever
/// else the operator put in `stt_base_url` (`vp_setup.py` exposes
/// `custom` as a first-class provider). Distinct from
/// [`STT_IMPL_CLOUD_OPENAI`] because OpenAI did not serve that audio and
/// saying it did is the same class of untruth these fields remove.
/// Codex P2 #687 round 3.
pub const STT_IMPL_CLOUD_CUSTOM: &str = "cloud-custom";

/// Registrable domain identifying Groq's OpenAI-compatible endpoint.
const GROQ_DOMAIN: &str = "groq.com";

/// Registrable domain identifying OpenAI's own endpoint.
const OPENAI_DOMAIN: &str = "openai.com";

/// Which cloud STT service a configured `base_url` points at.
///
/// Sniffs the HOST rather than trusting the `stt_backend` setting, which
/// is `openai` for EVERY OpenAI-compatible endpoint (Groq, Azure, a
/// self-hosted server -- all of them). An empty / unset base URL is the
/// `CloudTranscribeConfig::from_env` default, which IS OpenAI.
///
/// Three outcomes, fail-open to [`STT_IMPL_CLOUD_CUSTOM`]:
///
/// * `groq.com` / `*.groq.com` -> [`STT_IMPL_CLOUD_GROQ`]
/// * `openai.com` / `*.openai.com` (or an unset URL) -> [`STT_IMPL_CLOUD_OPENAI`]
/// * anything else -> [`STT_IMPL_CLOUD_CUSTOM`]. Claiming OpenAI served
///   audio that went to localhost or Azure is the same class of untruth
///   this module exists to remove. Codex P2 #687 round 3.
///
/// Host classification, NOT `contains`, reusing the same
/// [`crate::cloud_api::provider_host_public`] parser the API-key selector
/// uses. A substring test mislabels both directions:
/// `https://groq.com.attacker.example/v1` merely *contains* `groq.com`,
/// and `https://api.groq.com@custom.example/v1` has host
/// `custom.example` while containing `api.groq.com`.
pub fn cloud_stt_impl_for_base_url(base_url: &str) -> &'static str {
    if base_url.trim().is_empty() {
        // Unset means `DEFAULT_STT_BASE_URL`, which is OpenAI.
        return STT_IMPL_CLOUD_OPENAI;
    }
    let Some(host) = crate::cloud_api::provider_host_public(base_url) else {
        return STT_IMPL_CLOUD_CUSTOM;
    };
    let matches = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if matches(GROQ_DOMAIN) {
        STT_IMPL_CLOUD_GROQ
    } else if matches(OPENAI_DOMAIN) {
        STT_IMPL_CLOUD_OPENAI
    } else {
        STT_IMPL_CLOUD_CUSTOM
    }
}

/// Render the one-line startup summary that answers "what am I actually
/// running" at a glance:
///
/// ```text
/// [runtime] transcribe backend resolved: engine=rust-in-process impl=whisper.cpp accel=vulkan model=large-v3-turbo
/// ```
///
/// `model` is omitted entirely when empty (cloud sessions that never
/// resolved a local model tag) rather than emitting `model=` -- a blank
/// value reads as "no model" when it means "not applicable".
pub fn startup_line(engine: &str, stt_impl: &str, accel: &str, model: &str) -> String {
    let mut line = format!(
        "[runtime] transcribe backend resolved: engine={engine} impl={stt_impl} accel={accel}"
    );
    let model = model.trim();
    if !model.is_empty() {
        line.push_str(&format!(" model={model}"));
    }
    line
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;
