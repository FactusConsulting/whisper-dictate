# test fixtures

Small audio fixtures used by simulate-ptt smoke tests.

## `hello.wav`

- 16 kHz mono 16-bit PCM WAV, 500 ms, ~16 KB.
- Amplitude-modulated 440 Hz sine wave (4 Hz modulator) — deliberately
  synthetic so nobody's speech is checked into the repo. The modulation gives
  it the loud/quiet contrast `_looks_like_speech` requires so the sample
  reaches the transcription step; the actual transcript text is not
  deterministic. Tests that need to assert on the transcript stub the model.

Regenerate with the snippet in this directory's git history (or
`py -3.12 -c ...`, see the commit that added the fixture) — it's fully
deterministic.
