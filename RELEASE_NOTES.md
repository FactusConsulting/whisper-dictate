Adds reproducible audio-file transcription for debugging and backend benchmarks.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.53.exe** | Windows with NVIDIA CUDA |

## Highlights

- New `--transcribe-file PATH` command uses the selected backend/config and exits.
- Native support for 16-bit WAV with mono conversion and resampling to 16 kHz.
- mp3/m4a/other formats can be decoded when `ffmpeg` is installed.
- `--transcribe-file ... --json` emits structured JSON with backend/model, timings, language, source file and dictionary replacement metadata.

## Notes

- File transcription reuses the same dictionary/replacement pipeline as live dictation.
- This is the foundation for the upcoming benchmark/evaluation workflow.
