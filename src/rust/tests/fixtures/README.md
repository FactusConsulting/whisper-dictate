# Rust integration-test fixtures

## `hello_speech.wav`

- 16 kHz mono 16-bit PCM WAV, ~1.25 s, ~40 KB.
- **Machine-synthesized speech** of the phrase "Hello world.", generated
  with `espeak-ng` (NOT a recording of any real person — the same
  no-human-speech privacy stance as `src/python/tests/fixtures/hello.wav`,
  just intelligible enough for an actual ASR round-trip).

Used by `tests/groq_cloud_stt.rs` so the live Groq cloud-STT integration
test can assert **real transcription** (a non-empty transcript containing
"hello"/"world"), not just a successful HTTP round-trip. The synthetic
`hello.wav` tone can't do that — Whisper legitimately returns empty text
for a pure sine wave.

### Regenerate (fully deterministic, no real speech)

```sh
espeak-ng -s 150 -w /tmp/raw.wav "Hello world."
python3 - <<'PY'
import wave, numpy as np
w = wave.open("/tmp/raw.wav"); sr = w.getframerate(); n = w.getnframes()
pcm = np.frombuffer(w.readframes(n), dtype=np.int16).astype(np.float64); w.close()
tgt = 16000
new_n = int(round(len(pcm) * tgt / sr))
res = np.interp(np.linspace(0, 1, new_n, endpoint=False),
                np.linspace(0, 1, len(pcm), endpoint=False), pcm)
res = ((res / (np.max(np.abs(res)) or 1.0)) * (0.7 * 32767)).astype(np.int16)
o = wave.open("src/rust/tests/fixtures/hello_speech.wav", "w")
o.setnchannels(1); o.setsampwidth(2); o.setframerate(tgt)
o.writeframes(res.tobytes()); o.close()
PY
```
