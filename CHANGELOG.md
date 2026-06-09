# Changelog

All notable changes to whisper-dictate are documented here. This project follows
[Semantic Versioning](https://semver.org/).

## [1.7.1] — 2026-06-09

### Fixed
- **Release bundles no longer ship source code.** The Windows ZIP, Linux ZIP and
  the local installer builder copied the entire `src/` tree — including the Rust
  source (already compiled into the binary) and the test suites — into the
  downloadable bundle. They now ship only the Python worker package
  (`src/python/whisper_dictate`) that is actually run at runtime, matching the
  Windows installer. Smaller downloads, no stray source. A packaging test guards
  against the whole-tree copy returning.

### Changed
- Internal maintainability only (no behaviour change): reduced the cognitive
  complexity of 16 functions across the Rust UI/controller and the Python worker
  by extracting small, single-purpose helpers, and cleared the remaining
  SonarCloud code smells (duplicate literals, redundant exception handlers,
  unused parameters, nested ternaries, and similar). New unit tests lock the
  extracted push-to-talk state machines.

## [1.7.0] — 2026-06-08

### Added
- **Cross-platform `--doctor`**: expanded from a Linux/Wayland-only injection
  check into a comprehensive readiness report — app version, config validity, the
  Rust helper, the configured STT backend and its prerequisites (faster-whisper /
  CUDA + model cache, or the cloud API key and `/models` reachability), the audio
  stack, GPU details and free disk. Now genuinely useful on Windows and macOS,
  not just Linux.
- **Dev container** (`.devcontainer/`) that reproduces the CI environment for
  uniform build/test on any OS (Windows/WSL2/Ubuntu/macOS), validated by its own
  CI job — see [CONTRIBUTING.md](CONTRIBUTING.md).

### Fixed
- Worker version reporting — the app no longer reports `whisper-dictate unknown`.
- UTF-8 decoding of subprocess output (Danish characters on Windows / cp1252),
  with an AST structure test guarding against regressions.
- Stale dBFS/peak readings on the audio meter when capture goes inactive, and
  the `secret_store` documentation accuracy (0600 is Unix-only).

### Changed
- Pinned the Rust toolchain (`rust-toolchain.toml`, 1.96.0) so local builds match
  CI; decomposed oversized source and test files (no file over 500 lines) and
  flattened the Rust crate under `src/rust`.
- CI hardening: Rust build caching, PR-only concurrency cancellation, and
  Dependabot coverage for GitHub Actions and Cargo.

## Earlier releases

See the [GitHub Releases](https://github.com/FactusConsulting/whisper-dictate/releases)
page for 1.6.x and earlier.
