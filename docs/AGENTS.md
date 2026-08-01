# Documentation guidance

## Code Review Rules

- Keep user-facing documentation outside `docs/dev/` current. Put release,
  testing, ownership, design, and other engineering procedures under
  `docs/dev/`.
- Do not add architecture audits, migration diaries, roadmap snapshots, or
  documentation for removed runtimes and models.
- When documentation describes CLI flags, settings, providers, or UI
  behavior, keep the relevant configuration and architecture pages current.
- Document the current supported values and behavior, including explicit
  limitations and fallback paths; do not preserve historical alternatives just
  to explain how the repository used to work.
