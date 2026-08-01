# Hotkey guidance

## Code Review Rules

- Preserve left/right modifier identity and chord semantics across Windows,
  X11, and Wayland drivers.
- Backend selection and fallback must be explicit, observable in debug/trace
  logs, and covered by tests; never silently downgrade a requested driver.
- Foreground-window behavior matters on Windows. Modifier-only chords and
  combinations must be verified while the Rust UI is active, not only when it
  is in the background.
- Keep the single-owner push-to-talk lock above individual hotkey drivers so
  CLI and UI instances cannot record the same utterance.
