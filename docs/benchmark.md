# Benchmark corpus

This directory contains a small local evaluation corpus for comparing STT
backends on the phrases whisper-dictate actually needs to handle: Danish,
English, mixed Danish/English, terminal commands, product names and technical
terms.

The repo stores the manifest only. Audio recordings are local artifacts and are
ignored by git.

## Record audio

Install the normal runtime dependencies first, then record missing samples:

```powershell
py -3.12 scripts\benchmark\record-corpus.py --manifest benchmark\corpus.json --seconds 7
```

The script records each missing item to `benchmark\audio\<id>.wav`.

## Run a benchmark

```powershell
whisper-dictate run `
  --benchmark-corpus benchmark\corpus.json `
  --benchmark-backends "whisper:large-v3,parakeet:nvidia/parakeet-tdt-0.6b-v3" `
  --benchmark-jsonl benchmark\results.jsonl
```

Each JSONL row includes backend/model timing plus corpus metadata:

- `reference_text`
- `wer`
- `cer`
- `term_hits`
- `term_misses`
- `exact_match`

Missing audio files are emitted as skipped rows, so it is safe to run the
benchmark before the whole corpus is recorded.
