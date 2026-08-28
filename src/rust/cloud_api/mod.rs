//! OpenAI-compatible cloud API surface (transcription, post-processing checks,
//! chat completion).
//!
//! Split into submodules to keep each file under the repo's 500-LOC ceiling
//! and to give the new `external-api` chat completion path its own home as
//! Wave 4-B of the Python-removal roadmap (#348). Public re-exports below
//! keep the legacy `cloud_api::*` import sites in `main.rs`, `ui/tasks.rs`
//! and the postprocess module working without changes.

mod chat;
mod check;
mod check_nemotron;
mod grpc;
mod grpc_transcribe;
pub(crate) mod http;
mod prompts;
mod transcribe;

#[cfg(test)]
pub(crate) use grpc::NEMOTRON_NVCF_FUNCTION_ID;
pub(crate) use grpc::{
    canonical_nemotron_endpoint, has_custom_function_id, has_explicit_grpc_transport,
    is_hosted_nemotron_endpoint, is_nemotron_grpc_endpoint, is_nemotron_provider,
    migrate_nemotron_endpoint, NVCF_HOST,
};
pub(crate) use transcribe::{cloud_transcribe_for_provider, CloudTranscriptionRequest};

pub use chat::{
    handle_external_api, openai_chat_completion, ChatCompletionResult, DEFAULT_OPENAI_BASE_URL,
    GROQ_BASE_URL,
};
pub use check::{
    check_cloud_api, check_post_api, CloudApiCheck, CloudApiCheckResult, PostApiCheck,
    PostApiCheckResult,
};
pub(crate) use transcribe::provider_host as provider_host_public;
pub use transcribe::{
    cloud_transcribe, handle_cloud_transcribe, resolve_api_key, CloudTranscriptionResult,
    GROQ_TRANSCRIPTION_PROMPT_LIMIT,
};
