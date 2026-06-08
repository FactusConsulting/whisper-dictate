# Contributing

## Dev container (recommended — uniform build/test everywhere)

The repo ships a dev container that reproduces the CI Linux environment, so the
build/test loop is identical on Windows (Docker Desktop / WSL2), WSL2, native
Ubuntu and macOS — no per-machine toolchain setup, no drift from CI.

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

The container pins the same Rust toolchain as CI (via `rust-toolchain.toml`) and
Python 3.12, with `pytest`/`numpy` preinstalled.

### The dev loop (inside the container)

```sh
# Rust (matches CI exactly)
cargo fmt --manifest-path src/rust/Cargo.toml --all -- --check
cargo clippy --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app --all-targets --all-features -- -D warnings
cargo test --manifest-path src/rust/Cargo.toml --target-dir target -p whisper-dictate-app

# Python
python -m pytest src/python/tests src/tests/python -q

# To run the app / full STT stack (not needed for the test loop):
pip install -r requirements/cpu.txt
```

`.github/workflows/devcontainer.yml` builds the container and runs this exact
loop, so it stays in lockstep with CI and can't silently rot.

## Without a dev container

You can also work natively, but then you must match CI yourself:

- **Rust:** use rustup — it honours `rust-toolchain.toml` (the pinned version).
  Don't rely on a system `cargo` (e.g. Chocolatey) that ignores it; run
  `rustup run <pinned> cargo …` if rustup isn't first on `PATH`.
- **Python:** 3.12 with `pytest` + `numpy`.
- **Windows native build:** needs Visual Studio with the C++ workload (a working
  `vcvarsall.bat`). A broken/partial VS install is the usual cause of
  `error occurred in cc-rs: failed to find tool "lib.exe"`.
