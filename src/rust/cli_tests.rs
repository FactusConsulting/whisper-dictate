//! CLI compatibility tests, grouped by command family.
#[path = "cli/tests/commands.rs"]
mod commands;
#[path = "cli/tests/corpus.rs"]
mod corpus;
#[path = "cli/tests/dictionary.rs"]
mod dictionary;
#[path = "cli/tests/history.rs"]
mod history;
#[path = "cli/tests/self_test.rs"]
mod self_test;
#[path = "cli/tests/tools.rs"]
mod tools;
#[path = "cli/tests/transcription.rs"]
mod transcription;

#[path = "cli/tests/config.rs"]
mod config;

#[path = "cli/tests/hotkey.rs"]
mod hotkey;

#[path = "cli/tests/devices.rs"]
mod devices;
