# Native runtime verification

All supported builds use the Rust runtime. Validation requires the Rust
toolchain and the platform tools listed below.

## Automated checks

Run the repository's Rust test suite from the checkout:

```powershell
cargo install cargo-nextest --locked
cargo nextest run --manifest-path src/rust/Cargo.toml --locked -p whisper-dictate-app --profile ci
cargo test --manifest-path src/rust/Cargo.toml --locked -p whisper-dictate-app --doc
cargo fmt --manifest-path src/rust/Cargo.toml --all -- --check
```

`cargo-nextest` is the CI runner. It does not run doctests, so retain the
separate `cargo test --doc` command.

In CI, the Linux `base` feature-matrix cell owns the default nextest suite,
the required `unit` job owns repository-policy tests and doctests, and the
cross-platform `smoke` matrix owns `--help`, `--version`, and `config path`.
The Ubuntu 26.04 compatibility job runs library and binary tests without the
repository-policy integration target. Keep those owners separate so required
contexts remain meaningful and the same validation is not rebuilt in multiple
jobs.

To check the locked Rust dependency graph against RustSec advisories, install
`cargo-audit` and run:

```powershell
cargo audit --file src/rust/Cargo.lock
```

The reusable `cargo-audit` workflow runs once for dependency-relevant pull
requests, dependency changes on `main`, release validation, and every Monday.
For pull requests and releases its result is aggregated into the required
`unit` context. Address findings by updating the dependency; do not suppress an
advisory without documenting the reason and expiry in `.cargo/audit.toml`.
Current exceptions are tracked in the same file and must be removed by their
review date.

The `cargo-outdated` workflow publishes a scheduled Monday report of outdated
root dependencies. Review its output before updating FFI or system crates; it
does not change dependencies automatically.

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
