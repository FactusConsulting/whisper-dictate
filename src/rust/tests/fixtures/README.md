# Rust integration-test fixtures

## `hello_speech.wav`

- 16 kHz mono 16-bit PCM WAV, ~1.25 s, ~40 KB.
- **Machine-synthesized speech** of the phrase "Hello world.", generated
  with `espeak-ng` (NOT a recording of any real person — a strict
  no-human-speech privacy stance,
  just intelligible enough for an actual ASR round-trip).

Used by `tests/groq_cloud_stt.rs` so the live Groq cloud-STT integration
test can assert **real transcription** (a non-empty transcript containing
"hello"/"world"), not just a successful HTTP round-trip. The synthetic
`hello.wav` tone can't do that — Whisper legitimately returns empty text
for a pure sine wave.

### Regenerate (deterministic, no real speech)

`espeak-ng` renders the phrase to a WAV, then any offline resampler
normalises it: mono, 16-bit PCM, 16 kHz, peak-normalised to about 70%
full scale, ~1.25 s. The committed fixture is the artifact — regeneration
is only needed when intentionally changing it.
