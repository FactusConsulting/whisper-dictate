# Native dictation session wiring

The dictation session is wired entirely in Rust. The controller owns runtime
lifecycle and installs one native session per active run; the GUI does not
spawn a Python worker or maintain a compatibility dispatch chain.

The public entry points are:

- `wd ui` for the desktop controller;
- `wd run` for terminal-driven dictation; and
- `whisper-dictate dictate-run` for the foreground native runtime smoke path.

Startup failures are reported with actionable diagnostics and do not silently
switch engines. Use `VOICEPI_LOG=debug` or `VOICEPI_LOG=trace` when diagnosing
hotkey, capture, transcription, or injection setup.

See [ARCHITECTURE.md](../../ARCHITECTURE.md) for the current design and
[RELEASING.md](../RELEASING.md) for release verification.
