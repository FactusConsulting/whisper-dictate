# Native runtime verification

All supported builds use the Rust runtime. Validation requires the Rust
toolchain and the platform tools listed below.

## Automated checks

Run the repository's Rust test suite from the checkout:

```powershell
cargo test --manifest-path src/rust/Cargo.toml
cargo fmt --manifest-path src/rust/Cargo.toml --all -- --check
```

For the CI-matched Linux matrix, use:

```powershell
pwsh scripts/dev/dev-check.ps1
```

## Native CLI smoke checks

Build the CLI with the feature set needed by the command under test, then
exercise the native verbs directly:

```powershell
cargo run --manifest-path src/rust/Cargo.toml --features whisper-rs-local -- transcribe-file .\recording.wav
cargo run --manifest-path src/rust/Cargo.toml -- doctor
cargo run --manifest-path src/rust/Cargo.toml -- setup --help
```

The Windows hardware-dependent checks are documented in
[`scripts/manual-test/README.md`](../../scripts/manual-test/README.md). The
Wayland end-to-end procedure is in
[`wayland-user-smoke.md`](wayland-user-smoke.md).

## Diagnostics

Set `VOICEPI_LOG=debug` for lifecycle and configuration decisions. Set
`VOICEPI_LOG=trace` for hotkey, audio, session, and injection breadcrumbs.
On Windows, the GUI writes the diagnostic stream to
`%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`.
