# Contributing

## Dev container (recommended — uniform build/test everywhere)

The repo ships a dev container that reproduces the CI Linux environment. On
Windows, use Rancher Desktop's Docker-compatible runtime. On Ubuntu or WSL2,
use Docker directly. This keeps the build/test loop aligned with CI without
per-machine toolchain drift.

It builds and tests the **Linux target** (`x86_64-unknown-linux-gnu`), which
avoids the Windows MSVC toolchain entirely. The Windows installer is produced by
CI on a Windows runner; only running the *native* Windows GUI build locally needs
Visual Studio.

### Use it

- **VS Code:** install the *Dev Containers* extension, then run *Dev Containers:
  Reopen in Container*.
- **CLI:** `npm i -g @devcontainers/cli`, then
  `devcontainer up --workspace-folder .` and
  `devcontainer exec --workspace-folder . <cmd>`.

The container and CI both use the Rust version pinned in `rust-toolchain.toml`
(currently 1.97.1).

### The dev loop (inside the container)

```sh
# Rust toolchain and baseline checks
cargo fmt --manifest-path src/rust/Cargo.toml --all -- --check
cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --all-features -- -D warnings
cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app

# Repository and product tests
cargo test --manifest-path src/rust/Cargo.toml -p whisper-dictate-app
```

`.github/workflows/devcontainer.yml` builds the container and runs this exact
loop, so it stays in lockstep with CI and can't silently rot.

## Without a dev container

You can also work natively, but then you must match CI yourself:

- **Rust:** use rustup — it honours `rust-toolchain.toml` (the pinned version).
  Don't rely on a system `cargo` (e.g. Chocolatey) that ignores it; run
  `rustup run <pinned> cargo …` if rustup isn't first on `PATH`.
- **Repository checks:** Rust integration tests in `src/rust/tests/`.
- **Windows native build:** needs Visual Studio with the C++ workload (a working
  `vcvarsall.bat`). A broken/partial VS install is the usual cause of
  `error occurred in cc-rs: failed to find tool "lib.exe"`.

## Faster Rust test runs

The CI test runner is `cargo-nextest`; doctests remain on `cargo test` because
nextest does not run them. Install the same runner locally when you want a
CI-equivalent test pass:

```sh
cargo install cargo-nextest --locked
cargo nextest run --manifest-path src/rust/Cargo.toml --locked -p whisper-dictate-app --profile ci
cargo test --manifest-path src/rust/Cargo.toml --locked -p whisper-dictate-app --doc
```
