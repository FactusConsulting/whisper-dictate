# Third-party assets and licenses

This file tracks third-party assets bundled in the repository (i.e. binary
or data files that are checked in alongside our source). Rust / Python
package dependencies are not listed here — they are tracked via
`Cargo.toml` / `requirements/*.txt` and inherit their own licenses from
the respective package registries.

## Silero VAD model (`assets/silero_vad.onnx`)

- **Source:** https://github.com/snakers4/silero-vad
- **Version:** v4 (ONNX, 8 kHz / 16 kHz, ~1.8 MB).
- **License:** MIT
  (https://github.com/snakers4/silero-vad/blob/master/LICENSE)
- **Why this version:** v5 of the ONNX model changed its input tensor
  signature (`state` instead of `h`/`c`), which the `vad-rs` 0.1.5
  Rust binding we use does not yet support. v4 is the latest version
  compatible with the binding; we'll bump when `vad-rs` gains v5
  support upstream.
- **Used by:** the `audio-in-rust` cargo feature in
  `src/rust/audio/vad.rs`. The bytes are embedded into the binary via
  `include_bytes!` at compile time and written to a temp file at
  runtime (`vad-rs::Vad::new` requires a file path).
- **License compatible with our MIT-licensed code:** yes.

## Logo and bundled icons (`assets/whisper-dictate-logo.svg`, `assets/whisper-dictate.ico`)

- Our own work. MIT-licensed alongside the rest of the repository.

## Screenshot (`assets/live-dictation.png`)

- Our own work. MIT-licensed alongside the rest of the repository.
