# Docker build guidance

## Code Review Rules

- CI Dockerfiles must keep their base image configurable through
  `CI_BASE_IMAGE`; workflow resolution uses the organization Docker mirror
  and falls back to the upstream image when the mirror is unavailable.
- Do not hardcode mirror URLs, credentials, retired runtime payloads, or stale
  model backends in Docker build inputs.
