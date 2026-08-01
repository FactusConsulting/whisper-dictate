# Local Whisper guidance

## Code Review Rules

- Model loading remains lazy and idle unloading must preserve the documented
  unload/reload behavior and error handling.
- Report the accelerator actually used at model-load time; do not confuse the
  configured device preference with a successful GPU load.
- Numeric precision and quantization come from the model file. Do not add a
  retired runtime `compute_type` setting or a second precision control.
- Model lifecycle, accelerator fallback, idle unload, and malformed setting
  paths need focused tests and useful debug/trace diagnostics.
