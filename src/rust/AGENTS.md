# Rust product guidance

## Code Review Rules

- Production runtime behavior belongs in Rust. Do not add a second worker,
  compatibility launcher, or silent fallback to a removed runtime.
- Credential routing spans `credentials.rs`, `runtime/cloud_api_keys.rs`,
  `ui/api_keys.rs`, and `dictate/backends/cloud_transcribe.rs`; preserve
  provider/endpoint provenance across all four.
- Public CLI, configuration, or JSON changes must update the schema,
  documentation, and the narrowest relevant tests.
- The Rust UI entry point (`ui.rs`) and its submodules own one managed native
  runtime on Windows. Keep lifecycle changes explicit and diagnosable, preserve
  settings and credential-store behavior, and test success and error paths.
- UI hotkey validation must preserve left/right modifier identity and accepted
  chord semantics across the supported Windows, X11, and Wayland drivers.
- The supported desktop build enables `rust-injection`, `rust-hotkeys`,
  `audio-in-rust`, and `whisper-rs-local` (with Vulkan added by Vulkan builds);
  keep other experimental Cargo features opt-in.
- Precision comes from the selected model file. Do not add a second
  `compute_type` or numeric precision setting to configuration or runtime code.
- Runtime logs at debug or trace level should make lifecycle, backend,
  device, fallback, and error decisions diagnosable without exposing secrets.
- Windows behavior needs Windows-specific verification or an explicit reason
  why a platform test is not practical.

## Validation

- Run the Rust test suite from `src/rust/Cargo.toml` for runtime changes.
- Prefer focused unit or integration tests during iteration, then run the
  repository validation command before pushing.
