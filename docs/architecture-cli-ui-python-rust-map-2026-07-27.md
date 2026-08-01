# Current CLI and UI architecture

The shipped application is native Rust. The Rust controller owns the UI,
runtime supervision, hotkeys, capture, transcription, post-processing,
history, dictionary, and text injection.

## Runtime boundaries

| Surface | Implementation | Notes |
|---|---|---|
| `whisper-dictate ui` | Rust/egui | Owns settings, lifecycle, diagnostics, and restart actions. |
| `whisper-dictate run` | Rust runtime | Runs the native in-process dictation session. |
| `whisper-dictate transcribe-file` | Rust | One-shot WAV transcription using the configured native backend. |
| `whisper-dictate setup`, `export-config`, `doctor` | Rust | No interpreter or virtual environment is required. |
| Windows installer and Linux/Nix packages | Rust binaries | No Python payload or `requirements` tree is packaged. |

The `VOICEPI_DICTATE_ENGINE` variable is retained only to reject old
configuration values with migration guidance. `rust` (or an unset value) is
the only supported engine; `python` is not a fallback.

## Repository policy

The repository policy tests assert that `src/python` and `requirements` are
absent and that packaging, release, and workflow files do not launch Python.
Python-shaped strings that appear in policy tests or compatibility diagnostics
are guards against accidentally reintroducing the retired runtime; they are
not application entry points.

For the component map and platform details, see [TECHNICAL.md](TECHNICAL.md).
