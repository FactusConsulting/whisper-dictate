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
//! Every one of those fields is emitted by BOTH the Rust in-process
//! session (`crate::dictate::session::wire`) and the Python worker
//! (`vp_dictate.py::_transcription_event_fields`), and `stt_backend`
//! names the backend the user *selected*, not the code that ran. So the
//! record could not distinguish
//!
//! * Rust in-process whisper.cpp on a Vulkan GPU, from
//! * the Python worker's faster-whisper/CTranslate2 on CUDA, from
//! * either of those having silently fallen back to CPU.
//!
//! Three fields fix that, and this module owns their vocabulary so the
//! Rust and Python emitters cannot drift:
//!
//! * `engine` -- which runtime served the utterance
//!   ([`ENGINE_RUST_IN_PROCESS`] / [`ENGINE_PYTHON_WORKER`]).
//! * `stt_impl` -- the transcription implementation that actually ran
//!   ([`STT_IMPL_WHISPER_CPP`], [`STT_IMPL_FASTER_WHISPER`],
//!   [`STT_IMPL_CLOUD_OPENAI`], [`STT_IMPL_CLOUD_GROQ`]).
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

/// `engine` value for the Python worker subprocess
/// (`python -m whisper_dictate.runtime`). Mirrored verbatim in
/// `vp_dictate.py`; see `src/python/tests/test_dictate.py` for the
/// cross-language pin.
pub const ENGINE_PYTHON_WORKER: &str = "python-worker";

/// `stt_impl` value for whisper.cpp (via the `whisper-rs` bindings),
/// whether it runs in-process in the Rust session or behind the
/// `transcribe-server` helper the Python worker drives.
pub const STT_IMPL_WHISPER_CPP: &str = "whisper.cpp";

/// `stt_impl` value for the Python worker's in-process
/// faster-whisper / CTranslate2 bindings.
pub const STT_IMPL_FASTER_WHISPER: &str = "faster-whisper";

/// `stt_impl` value for an OpenAI `/audio/transcriptions` endpoint.
pub const STT_IMPL_CLOUD_OPENAI: &str = "cloud-openai";

/// `stt_impl` value for Groq's OpenAI-compatible endpoint. Split from
/// [`STT_IMPL_CLOUD_OPENAI`] because they are different services with
/// different models and failure modes, and the `stt_backend` setting
/// spells both `openai`.
pub const STT_IMPL_CLOUD_GROQ: &str = "cloud-groq";

/// Registrable domain identifying Groq's OpenAI-compatible endpoint.
const GROQ_DOMAIN: &str = "groq.com";

/// Which cloud provider a configured `base_url` points at.
///
/// Sniffs the HOST rather than trusting the `stt_backend` setting, which
/// is `openai` for BOTH providers (Groq is selected purely by base URL).
/// An empty / unset / unparseable base URL defaults to OpenAI, matching
/// `CloudTranscribeConfig::from_env`'s `DEFAULT_STT_BASE_URL`.
///
/// Host classification, NOT `contains`, reusing the same
/// [`crate::cloud_api::provider_host_public`] parser the API-key selector
/// uses. A substring test mislabels both directions:
/// `https://groq.com.attacker.example/v1` merely *contains* `groq.com`,
/// and `https://api.groq.com@custom.example/v1` has host
/// `custom.example` while containing `api.groq.com`. Either way the
/// record would name a service that did not handle the audio -- which is
/// the same class of untruth the provenance fields exist to remove.
/// Codex P2 #687 vp_provenance.py:92.
pub fn cloud_stt_impl_for_base_url(base_url: &str) -> &'static str {
    let Some(host) = crate::cloud_api::provider_host_public(base_url) else {
        return STT_IMPL_CLOUD_OPENAI;
    };
    if host == GROQ_DOMAIN || host.ends_with(&format!(".{GROQ_DOMAIN}")) {
        STT_IMPL_CLOUD_GROQ
    } else {
        STT_IMPL_CLOUD_OPENAI
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
