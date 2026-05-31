Adds opt-in spoken formatting commands for punctuation and line breaks.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.59.exe** | Windows with NVIDIA CUDA |

## Highlights

- `VOICEPI_FORMAT_COMMANDS` enables deterministic spoken formatting commands.
- English command set: `new line`, `comma`, `period`, `question mark`, `bullet list`, and related commands.
- Danish command set: `ny linje`, `komma`, `punktum`, `spørgsmålstegn`, `punktliste`, and related commands.
- Formatting command results are recorded in metrics/history as `format_commands_*`.

## Notes

- The feature is off by default to avoid changing literal dictation.
- Enable globally or per profile with `VOICEPI_FORMAT_COMMANDS=en`, `da`, or `both`.
