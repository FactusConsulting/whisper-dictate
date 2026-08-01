# Rust UI guidance

## Code Review Rules

- The Rust UI owns one managed native runtime on Windows. Do not introduce a
  duplicate worker, hidden restart, or implicit second UI instance.
- Start, stop, restart, and restart-required setting changes must be explicit
  in the UI and visible in diagnostics.
- Configuration edits must preserve dirty-state, save/reset, migration, and
  OS credential-store behavior. API keys never belong in `config.json` or UI
  logs.
- UI state, provider selection, and settings persistence changes need focused
  tests for both success and validation/error paths.
- User-visible UI behavior changes must update the relevant configuration or
  architecture documentation when they alter settings, providers, lifecycle,
  or validation behavior.
