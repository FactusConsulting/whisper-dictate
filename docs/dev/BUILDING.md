# Building from source

Use a published installer or archive for normal installation. This page is for
developers building the Rust application from a checkout.

## Build profiles

The crate keeps reduced development features separate from supported shipping
profiles:

| Feature/profile | Purpose |
|---|---|
| default (`ui-egui-glow`) | Fast UI/CLI development build without the complete dictation backend. |
| `audio-capture` | Native microphone capture, device enumeration, and resampling. |
| `rust-hotkeys` | Native global hotkey listeners and coordinator. |
| `rust-injection` | Native typing and clipboard injection. |
| `whisper-rs-local` | Local whisper.cpp transcription. |
| `nemotron-local` | In-process Nemotron 3.5 decoding through the dynamically loaded NeMo-Speech.cpp C ABI. Official runtime/model assets are cached and verified automatically on first use. |
| `shipping` | Canonical CPU profile used by Linux, Nix, packaging, and exact-profile CI. |
| `shipping-vulkan` | Canonical shipping profile plus whisper.cpp Vulkan acceleration for Windows artifacts. |

Build the CPU shipping profile with:

```bash
cargo build --locked --release --manifest-path src/rust/Cargo.toml \
  --no-default-features --features shipping
```

A default `cargo run` is intentionally reduced. Use a named shipping profile
when validating an installable application.

## Dev container

The repository dev container supplies the Linux compiler and native libraries
used by CI. From VS Code, use **Dev Containers: Reopen in Container**. From a
terminal:

```bash
npm install -g @devcontainers/cli
devcontainer up --workspace-folder .
devcontainer exec --workspace-folder . cargo test --manifest-path src/rust/Cargo.toml
```

On the repository's Windows development machines, the CI-matched gate is:

```powershell
pwsh -NoProfile -File scripts/dev/dev-check.ps1
```

It uses Rancher Desktop's Docker `default` context and validates formatting,
Clippy, tests, feature profiles, the shipping build, and CLI smoke commands.

## Linux native prerequisites

Ubuntu/Debian builds need the Rust toolchain plus the native UI, audio,
whisper.cpp, and input libraries:

```bash
sudo apt install \
  build-essential pkg-config cmake clang libclang-dev \
  libdbus-1-dev libwayland-dev libx11-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
  libxkbcommon-dev libxi-dev libxtst-dev libasound2-dev \
  libportaudio2 alsa-utils xclip xdotool
```

The source installer builds and installs the canonical shipping profile:

```bash
scripts/linux/install-rust-ui.sh
wd ui
```

## Windows native prerequisites

Native Windows builds use Rust's MSVC target and require:

1. Visual Studio Build Tools with **Desktop development with C++** and a
   Windows SDK;
2. CMake on `PATH`;
3. LLVM/libclang for bindgen. Set `LIBCLANG_PATH` to the LLVM `bin` directory
   if it is not detected automatically.

The Vulkan shipping profile additionally requires the Vulkan SDK and Ninja on
`PATH` (or a Developer PowerShell environment that provides it). Normal Windows
development can use the default/reduced profile. The installer built locally or
on Windows CI is unsigned, so Windows may show a SmartScreen warning.

For local installer testing:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/windows/build-installer.ps1
```

The generated installer and portable archive are written under `Output\`.

## Verification

Use [testing-native-runtime.md](testing-native-runtime.md) for the complete
test matrix, dependency checks, diagnostics, and platform smoke procedures.
Release packaging and tag rules are documented in [RELEASING.md](RELEASING.md).
