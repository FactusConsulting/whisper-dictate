<p align="center">
  <img src="assets/whisper-dictate-logo.svg" width="96" height="96" alt="whisper-dictate logo">
</p>

Moves the Windows and Linux control surface to the Rust UI/controller while
keeping the Python audio/ML worker behind that controller.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-cpu-setup-<version>.exe** | Windows CPU / AMD |
| **whisper-dictate-windows-nvidia-setup-<version>.exe** | Windows with NVIDIA CUDA |
| **whisper-dictate-windows-cpu-<version>.zip** | Portable Windows CPU / AMD bundle |
| **whisper-dictate-windows-nvidia-<version>.zip** | Portable Windows NVIDIA CUDA bundle |
| **whisper-dictate-windows-amd-<version>.zip** | Portable Windows AMD/CPU bundle |
| **whisper-dictate-linux-rust-ui-<version>** | Linux Rust UI/controller binary |
| **whisper-dictate-linux-cpu-<version>.zip** | Linux portable bundle with Rust controller |

## Highlights

- Windows installer shortcuts now launch the Rust UI as the primary control surface.
- The Rust UI can start, stop, restart, doctor, install/repair, edit settings, preview dictionaries, and stream worker logs.
- The Rust terminal controller supports `run`, `doctor`, `install`, `settings`, and `config`.
- Linux release bundles include the Rust controller, and the standalone Linux Rust UI binary is published as a release asset.
- The bundled Rust controller is now the only installed Windows launcher.
- Deterministic dictionary parsing, prompt construction, replacements, and spoken formatting commands are covered in Rust tests.

## Notes

- Python remains the worker boundary for audio capture, Whisper/Parakeet/OpenAI STT, hotkeys, text injection, history, and metrics.
- Legacy PySide/PowerShell UI files are no longer shipped; Rust UI owns the installed Windows experience.
