# Documentation guidance

## Code Review Rules

- Keep documentation under `docs/` current and user-facing. Put release,
  testing, ownership, design, and other engineering procedures under
  `docs/dev/`.
- Do not add architecture audits, migration diaries, roadmap snapshots, or
  documentation for removed runtimes and models.
- When CLI flags, settings, providers, or UI behavior change, update the
  relevant configuration and architecture pages and check their links.
- Document the current supported values and behavior, including explicit
  limitations and fallback paths; do not preserve historical alternatives just
  to explain how the repository used to work.
