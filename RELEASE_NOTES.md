Adds microphone calibration commands for actionable audio threshold tuning.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.55.exe** | Windows with NVIDIA CUDA |

## Highlights

- New `--calibrate-mic [SECONDS]` command records a short sample and prints pass/warn/fail diagnostics.
- New `--calibrate-file PATH` analyzes an existing audio file with the same calibration logic.
- `--json` outputs structured calibration data for automation.
- Recommendations include `VOICEPI_TARGET_DBFS`, `VOICEPI_MIN_INPUT_DBFS`, and `VOICEPI_MIN_SNR_DB`.

## Notes

- Calibration runs before model load, so it is quick and works without Whisper/Parakeet startup.
- Use calibration output to tune Quality settings when the mic or room is the bottleneck.
