# Benchmark corpus

This directory contains a small local evaluation corpus for comparing STT
backends on the phrases whisper-dictate actually needs to handle: Danish,
English, mixed Danish/English, terminal commands, product names and technical
terms.

The repo stores the manifest only. Audio recordings are local artifacts and are
ignored by git.

## Record audio

Use the native corpus recorder to capture missing samples one item at a time.
It writes to the per-user audio dir (`%APPDATA%\WhisperDictate\benchmark\audio`
on Windows, the XDG equivalent elsewhere), so recordings survive reinstalls:

```powershell
whisper-dictate corpus-record <ID>
```

The System tab in the app has a UI equivalent (picker + Record button).

## Run a benchmark

```powershell
whisper-dictate bench
```

The native runner resolves `benchmark/corpus.json` (app root first, then the
per-user appdata dir), runs the configured backend against every item and
prints per-item JSONL plus one `[benchmark]` summary line. The same code path
drives the System tab's "Run benchmark" button.

Each JSONL row includes backend/model timing plus corpus metadata:

- `reference_text`
- `wer`
- `cer`
- `term_hits`
- `term_misses`
- `exact_match`

Missing audio files are emitted as skipped rows, so it is safe to run the
benchmark before the whole corpus is recorded.
