Improves post-processing mode compatibility and makes dictionary benchmark
suggestions safer by filtering common words and noisy fragments.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.60.exe** | Windows with NVIDIA CUDA |

## Highlights

- `VOICEPI_POST_MODE=bullet-list` is accepted as an alias for the existing `bullets` post-processing mode.
- Dictionary suggestions now reject common words, known source terms, and noisy benchmark fragments such as ordinary Danish/English function words.
- The current benchmark corpus no longer proposes unsafe replacements after applying the safer suggestion rules.

## Notes

- Raw post-processing remains the default.
- Dictionary suggestions are still manual: inspect them before pressing Apply in the UI.
