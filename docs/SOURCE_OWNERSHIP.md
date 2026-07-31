# Source ownership

The shipped application is native Rust. Python is limited to repository
automation and policy tests; it is not packaged, installed, imported, or
launched by the product.

## Rust

Rust source lives in `src/rust`.

| Area | Primary modules |
|---|---|
| CLI and entrypoints | `cli.rs`, `main.rs`, `whisper-dictate-gui.rs` |
| Runtime lifecycle | `runtime/`, `hotkey/`, `dictate/session/` |
| Audio and VAD | `audio/`, `audio_dsp/`, `dictate/audio_route/` |
| Local and cloud STT | `whisper/`, `cloud_api/`, `dictate/backends/` |
| Formatting and dictionaries | `formatting.rs`, `postprocess/`, `dictionary.rs` |
| Text injection | `injection/`, `dictate/backends/inject.rs` |
| Desktop UI | `ui.rs`, `ui/` |
| History, metrics, diagnostics | `history.rs`, `telemetry.rs`, `diag.rs` |

The UI supervisor runs the session in-process. Reduced builds that omit a
required Cargo feature return an explicit error and do not fall back to another
engine.

## Repository-policy tooling

The native-only regression guard in
`src/rust/tests/native_only_runtime.rs` rejects:

- tracked product files under the retired `src/python` or `requirements` trees;
- Python process launches and retired runtime environment markers in production
  Rust;
- installer, portable-archive, release, or Nix references that package a Python
  runtime;
- the retired CLI/UI Install/Repair flow.

The remaining repository-policy checks are being migrated from the legacy
`src/tests/python` harness. The temporary Python test tooling must not become a
release payload or a product call target.

When moving or deleting production behavior, add the narrowest useful Rust
regression test and extend the repository guard when the removed boundary could
otherwise be reintroduced by packaging or workflow changes.
