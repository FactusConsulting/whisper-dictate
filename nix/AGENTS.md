# Nix packaging guidance

## Code Review Rules

- Keep Nix package definitions aligned with the Rust CLI/UI launch paths and
  the native feature set used by the supported installer builds.
- Do not add dead runtime payloads, stale model backends, credentials, or
  account-bound URLs to Nix expressions.
