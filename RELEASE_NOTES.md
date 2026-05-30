Adds an opt-in command hook for advanced local automation.

## Download

| Asset | Use on |
|---|---|
| **whisper-dictate-windows-nvidia-setup-0.2.58.exe** | Windows with NVIDIA CUDA |

## Highlights

- `VOICEPI_COMMAND_HOOK` can run an advanced opt-in command after each accepted utterance.
- The hook receives the structured utterance event as JSON on stdin.
- The hook is executed with `shell=False`, so transcript text is not interpolated into a shell command.
- Hook result fields are recorded in metrics/history as `command_hook_*`.

## Notes

- Prefer JSON-array command form, for example `["python","D:\\scripts\\handle-dictation.py"]`.
- `VOICEPI_COMMAND_HOOK_TIMEOUT_MS` defaults to `2000`; timeout/failure is logged but dictation injection still succeeds.
