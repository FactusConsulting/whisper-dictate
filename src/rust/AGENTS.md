# Rust product guidance

## Code Review Rules

- Production runtime behavior belongs in Rust. Do not add a second worker,
  compatibility launcher, or silent fallback to a removed runtime.
- Public CLI, configuration, or JSON changes must update the schema,
  documentation, and the narrowest relevant tests.
- Keep experimental Cargo features opt-in; the default build must remain the
  supported install path.
- Runtime logs at debug or trace level should make lifecycle, backend,
  device, fallback, and error decisions diagnosable without exposing secrets.
- Windows behavior needs Windows-specific verification or an explicit reason
  why a platform test is not practical.

## Validation

- Run the Rust test suite from `src/rust/Cargo.toml` for runtime changes.
- Prefer focused unit or integration tests during iteration, then run the
  repository validation command before pushing.
