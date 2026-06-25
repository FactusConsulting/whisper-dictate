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
mod http;
mod transcribe;

pub use chat::{
    handle_external_api, openai_chat_completion, ChatCompletionResult, DEFAULT_OPENAI_BASE_URL,
    GROQ_BASE_URL,
};
pub use check::{
    check_cloud_api, check_post_api, CloudApiCheck, CloudApiCheckResult, PostApiCheck,
    PostApiCheckResult,
};
pub use transcribe::{
    cloud_transcribe, handle_cloud_transcribe, CloudTranscriptionResult,
    GROQ_TRANSCRIPTION_PROMPT_LIMIT,
};
