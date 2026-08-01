# Native in-process dictation runtime

The in-process Rust runtime described by this page is now the shipped path.
The Rust controller installs the hotkey coordinator, capture/session sink,
transcription backend, post-processing, history, and injection without
starting a Python worker.

The standalone `whisper-dictate dictate-run` command remains a native Rust
diagnostic/foreground entry point. It shares the same runtime building blocks
as the GUI and `run` command.

No Python fallback, dual-process dispatch, or Python-specific environment
setup is supported. See [TECHNICAL.md](../TECHNICAL.md) for the current
component boundaries and [CONFIGURATION.md](../CONFIGURATION.md) for user
settings.
