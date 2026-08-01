# Script guidance

## Code Review Rules

- PowerShell automation must work without the user profile; preserve explicit
  `-NoProfile` invocation and robust path quoting.
- POSIX and Windows scripts must keep their platform-specific launch behavior,
  use safe argument handling, and emit ASCII- or UTF-8-safe diagnostics.
- Script changes must not invoke removed runtimes or dead compatibility paths.
  Add or update the narrowest smoke/policy test that guards the changed flow.
- Debug and trace modes should expose decision points and failures without
  printing credentials or full user audio/text payloads.
