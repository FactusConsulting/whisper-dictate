# Current architecture audit

The application and shipped tooling are native Rust. The controller owns the
desktop UI, runtime supervision, hotkey/capture pipeline, transcription,
post-processing, history, dictionary, and text injection.

Repository-policy tests enforce the native-only boundary: no `src/python` or
`requirements` payload is tracked, and packaging/workflows must not invoke a
retired Python runtime. Compatibility strings in policy tests are deliberate
regression guards, not executable code.

For implementation details and platform-specific behavior, see
[TECHNICAL.md](TECHNICAL.md). For the validation commands, see
[CONTRIBUTING.md](../CONTRIBUTING.md).
