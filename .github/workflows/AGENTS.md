# Workflow guidance

## Code Review Rules

- Keep Docker image resolution on the organization-level
  `NEXUS_DOCKER_MIRROR` variable with the documented direct-Docker fallback.
  Never hardcode a mirror URL or credential.
- Workflow path filters must run repository-policy coverage when changes touch
  packaging, Docker, Nix, schemas, or workflow files themselves.
- Do not print secrets, API keys, tokens, or credential-store contents in job
  logs. Keep diagnostic output useful but sanitized.
- Workflow changes need syntax/lint validation and a test proving that the
  intended matrix or fallback path still runs.
