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
excluding the `repository_policy` test binary. The required `unit` job owns
repository-policy tests and doctests, and the cross-platform `smoke` matrix
owns `--help`, `--version`, and `config path`. The Ubuntu 26.04 compatibility
job runs library, binary, and every non-policy integration-test target. Keep
those owners separate so required contexts remain meaningful and the same
validation is not rebuilt in multiple jobs.

To check the locked Rust dependency graph against RustSec advisories, install
`cargo-audit` and run:

```powershell
cargo audit --file src/rust/Cargo.lock
```

The reusable `cargo-audit` workflow runs once for dependency-relevant pull
requests and `main` pushes, release validation, and every Monday. The tests
workflow routes PR, push, and release calls through the required `unit` context;
the scheduled run invokes the same implementation directly. Address findings
by updating the dependency; do not suppress an
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

## Physical hotkey focus matrix

Stop normal dictation. On Windows and X11, open **Speech > Test shortcut in
both windows** and record the actual installed driver plus the two visible
results. On Wayland, confirm that the control is disabled and both focus
results are documented as unavailable. The diagnostic must not open the
microphone, load Whisper, or inject text.

| Platform/session | Chord | Expected driver | Another window focused | WhisperDictate focused |
|---|---|---|---|---|
| Windows | `pause` | `win_registerhotkey` | press + release verified | press + release verified |
| Windows | `ctrl+f9` | `win_registerhotkey` | record observed result | record observed result |
| Windows fallback | `ctrl_l+f9` | `rdev` with focus-risk warning | record observed result | record observed result; use `pause` if it fails |
| Linux X11 | `pause` | `rdev` | press + release verified | press + release verified |
| Linux Wayland | `pause` | `evdev` | unavailable - no portable focus-owner API | unavailable - no portable focus-owner API |

For each row, confirm Settings distinguishes preflight from the installed
driver and keeps the two focus results separate. Editing the chord must make a
prior result stale, and restarting the app must clear the diagnostic result.
If either supported focus context fails, confirm the UI recommends `pause` or
another tested chord rather than changing configuration automatically. On
Wayland, record the installed `evdev` driver from normal runtime status but
treat the disabled focus-classification workflow as unavailable by design, not
as an `evdev` regression. Listener install or chord-event failures on Wayland
still belong to the `evdev`
access/device-permission path; X11 and Windows fallback failures belong to the
`rdev` listener path.

## Diagnostics

Set `VOICEPI_LOG=debug` for lifecycle and configuration decisions. Set
`VOICEPI_LOG=trace` for hotkey, audio, session, and injection breadcrumbs.
On Windows, the GUI writes the diagnostic stream to
`%LOCALAPPDATA%\WhisperDictate\gui-diagnostic.log`.
The guided shortcut test logs only its selected driver, registration outcome,
configured-chord press/release/cancel signals, and focused/unfocused result. It
does not log unrelated keys, even when the parent has
`VOICEPI_HOTKEY_DEBUG=1`. Each chord signal carries the foreground-process
classification captured by the child action sink at the event source; pipe or
GUI-frame latency never reclassifies it later. Pure Wayland reports the focus
classification as unavailable because no portable owner API exists. Stopping
the test must terminate and reap the child process so repeated rdev/evdev checks
do not leave process-lifetime listener threads in the GUI.
