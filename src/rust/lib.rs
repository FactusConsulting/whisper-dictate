pub mod cli;
pub mod cloud_api;
pub mod command_hook;
pub mod config;
pub mod dictionary;
pub mod formatting;
pub mod injection;
pub mod model_capacity;
pub mod privacy;
pub mod profiles;
pub mod redaction;
pub mod runtime;
pub mod telemetry;
pub mod ui;
// Local Whisper inference (CPU-only spike, roadmap issue #317 sub-task 1).
// Gated behind the `whisper-rs-local` cargo feature so the default build
// never pulls in whisper.cpp / CMake.
#[cfg(feature = "whisper-rs-local")]
pub mod whisper;
