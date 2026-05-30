# Repository Instructions

## Regression Tests

Whenever fixing a bug, add a regression test in the same change unless there is
a clear technical reason not to. Prefer the narrowest useful test:

- Unit tests for pure logic, parsing, configuration, command construction, and
  small platform-specific guards.
- Integration or smoke tests when the bug is in process launch, installer
  behavior, runtime wiring, dependency setup, or cross-module behavior.
- Both when the bug has a small isolated cause and a higher-level workflow that
  could regress independently.

If a regression test is not practical, document the reason in the commit or PR
summary and include the manual verification that covers the bug.
