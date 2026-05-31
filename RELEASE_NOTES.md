Adds opt-in spoken formatting commands and dictionary suggestion tooling for
turning benchmark/history misses into deterministic replacements.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.59.exe** | Windows with NVIDIA CUDA |

## Highlights

- `VOICEPI_FORMAT_COMMANDS` enables deterministic spoken formatting commands.
- English command set: `new line`, `comma`, `period`, `question mark`, `bullet list`, and related commands.
- Danish command set: `ny linje`, `komma`, `punktum`, `spørgsmålstegn`, `punktliste`, and related commands.
- Formatting command results are recorded in metrics/history as `format_commands_*`.
- `--dictionary-suggest JSONL` proposes smart replacements from benchmark or history output without mutating your dictionary.
- The Windows Settings UI can preview benchmark/history replacement suggestions and apply them to the configured dictionary.

## Notes

- The feature is off by default to avoid changing literal dictation.
- Enable globally or per profile with `VOICEPI_FORMAT_COMMANDS=en`, `da`, or `both`.
- Dictionary suggestions are manual: inspect them before pressing Apply in the UI.
