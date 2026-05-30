Adds local dictation history for recovery, copy, and reinject workflows.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.56.exe** | Windows with NVIDIA CUDA |

## Highlights

- Accepted live dictations are stored locally as JSONL history.
- `--history-list`, `--history-last`, `--history-copy-last`, and `--history-reinject-last` expose recovery workflows.
- `VOICEPI_HISTORY_ENABLED=0` disables history.
- `VOICEPI_HISTORY_JSONL` overrides the history path.

## Notes

- History is local-only and stores compact utterance metadata, not audio.
- File transcription and benchmark commands do not automatically add to live dictation history.
