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
(currently 1.98.1).

The container matches CI. Use the canonical [native runtime verification
guide](docs/dev/testing-native-runtime.md) for test, dependency, and diagnostic
commands.

Build profiles, Linux packages, Windows MSVC/libclang requirements, and local
installer commands are documented in the [source build guide](docs/dev/BUILDING.md).

## Without a dev container

You can also work natively, but then you must match CI yourself:

- **Rust:** use rustup — it honours `rust-toolchain.toml` (the pinned version).
  Don't rely on a system `cargo` (e.g. Chocolatey) that ignores it; run
  `rustup run <pinned> cargo …` if rustup isn't first on `PATH`.
- **Repository checks:** Rust integration tests in `src/rust/tests/`.
- **Windows native build:** needs Visual Studio with the C++ workload (a working
  `vcvarsall.bat`). A broken/partial VS install is the usual cause of
  `error occurred in cc-rs: failed to find tool "lib.exe"`.

For Rust test commands and dependency checks outside the container, see the
[native runtime verification guide](docs/dev/testing-native-runtime.md).
