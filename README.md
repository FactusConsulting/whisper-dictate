<p align="center">
  <img src="assets/whisper-dictate-logo.svg" width="112" height="112" alt="whisper-dictate logo">
</p>

<h1 align="center">whisper-dictate</h1>

<p align="center"><strong>Speak prompts instead of typing them.</strong></p>

<p align="center">
  <img src="assets/live-dictation.png" width="820" alt="Live dictation in the whisper-dictate GUI: each utterance shows the transcript and a per-utterance microphone/model health grade">
</p>

whisper-dictate is app-agnostic push-to-talk dictation. Hold a key, speak,
release, and the text is inserted into the focused window: Codex, Claude Code,
a terminal, a browser, an editor, anything.

The default speech engine is local Whisper, so normal dictation does not send
audio to a cloud service. Optional cloud and self-hosted backends are available
when you explicitly choose them.

## Start Dictating

1. **Install**
   - Windows: download the installer from the
     [latest release](https://github.com/FactusConsulting/whisper-dictate/releases/latest),
     or use Chocolatey:

     ```powershell
     choco source add -n=whisper-dictate -s="https://factusconsulting.github.io/whisper-dictate/chocolatey/index.json"
     choco install whisper-dictate --source=whisper-dictate -y
     ```

   - Ubuntu Wayland:

     ```bash
     brew tap factusconsulting/tap
     brew install whisper-dictate
     whisper-dictate setup-ubuntu
     ```

   - Nix:

     ```bash
     nix run github:FactusConsulting/whisper-dictate -- run --key f9 --lang en
     ```

2. **Open the app**
   - Windows: Start menu -> **whisper-dictate**
   - Linux: run `whisper-dictate ui`

3. **Pick only the basics**
   - microphone
   - push-to-talk key
   - spoken language

4. **Use it**
   Click **Start**, focus the app you want to dictate into, then hold the key,
   speak, and release.

The three first-run settings are enough for most people. Everything else has a
default.

## Need More?

| Task | Go here |
|---|---|
| Platform-specific installs, Chocolatey, winget, Nix, Linux X11 | [docs/INSTALLATION.md](docs/INSTALLATION.md) |
| Every setting, CLI flag, recipes, dictionary, profiles, cloud/STT backends | [docs/CONFIGURATION.md](docs/CONFIGURATION.md) |
| Microphone quality, SNR, quiet/noisy input | [docs/MICROPHONE.md](docs/MICROPHONE.md) |
| Architecture and platform internals | [docs/TECHNICAL.md](docs/TECHNICAL.md) |
| Development and tests | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Releases and local installer builds | [docs/RELEASING.md](docs/RELEASING.md) |

## CLI

The UI is the easiest path. For terminal use:

```bash
whisper-dictate run --key f9 --lang en
```

Common examples:

```powershell
whisper-dictate run --key ctrl_r --lang da
whisper-dictate.exe run --key ctrl_r --lang da --device cuda
whisper-dictate doctor
```

On Windows, the normal **whisper-dictate** shortcut runs the Rust UI and starts
the Python worker hidden underneath it, with logs streamed into the Dictation
tab.

## Supported Platforms

| Platform | Best start |
|---|---|
| Windows 10 / 11 | Installer or Chocolatey |
| Ubuntu 24.04 / 26.04 Wayland | Homebrew + `whisper-dictate setup-ubuntu` |
| Linux X11 | Release zip or source install |
| NixOS / Nix | Flake package or NixOS module |

See [docs/INSTALLATION.md](docs/INSTALLATION.md) for details, including
Chocolatey source management, local winget manifests, portable zips, and Linux
desktop entries.

## Tests

```bash
python -m pytest src/python/tests src/tests/python -q
```

For Rust, clippy/fmt, and a CI-matched environment, use the dev container in
[CONTRIBUTING.md](CONTRIBUTING.md).

## Build features

The Rust crate exposes a small set of opt-in cargo features beyond the
default UI build:

| Feature              | Default | What it does |
|----------------------|---------|--------------|
| `ui-egui-glow`       | yes     | egui via the glow (OpenGL) backend — shipping renderer. |
| `ui-egui-wgpu`       | no      | egui via the wgpu backend — continuously-validated exit route. |
| `whisper-rs-local`   | no      | Compiles in [whisper-rs] (whisper.cpp bindings) for local CPU inference. See below. |

[whisper-rs]: https://crates.io/crates/whisper-rs

### Local Whisper (experimental)

Behind the **`whisper-rs-local`** cargo feature, the crate ships a
small `whisper` module that loads a GGML/GGUF Whisper model and
transcribes a 16 kHz mono WAV. This is the CPU-only spike from roadmap
issue [#317] sub-task 1 — it is **not** wired into the runtime yet; the
Python transcription path is still the only thing the app uses at
runtime.

Enabling the feature pulls in whisper.cpp and compiles it from source,
so the build host must have:

- **CMake** (3.x)
- A **C/C++ compiler** (clang or gcc on Linux/macOS; MSVC on Windows)

Grab a model from the [whisper.cpp release page on Hugging Face][whisper-models]
— `ggml-tiny.en.bin` (~75 MB) is enough to validate the integration.

Run the example against a 16 kHz mono WAV:

```bash
cargo run --release \
    --manifest-path src/rust/Cargo.toml \
    --features whisper-rs-local \
    --example whisper_local -- \
    --model /path/to/ggml-tiny.en.bin \
    --wav   /path/to/audio_16khz_mono.wav
```

The unit test `transcribes_hello_world_when_model_available` skips
unless both `WHISPER_TEST_MODEL_PATH` and `WHISPER_TEST_WAV_PATH` are
set, so CI is unaffected.

[#317]: https://github.com/FactusConsulting/whisper-dictate/issues/317
[whisper-models]: https://huggingface.co/ggerganov/whisper.cpp

## License

MIT - see [LICENSE](LICENSE).
