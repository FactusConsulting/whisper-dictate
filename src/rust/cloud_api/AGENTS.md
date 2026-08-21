# Cloud transcription guidance

## Code Review Rules

- `stt_backend` selects the local `whisper` engine or the generic
  OpenAI-compatible `openai` engine. `stt_provider` selects `openai`, `groq`,
  `nemotron`, or `custom`; do not create a duplicate backend for a compatible
  provider.
- Resolve credentials from the selected provider and endpoint. Never send an
  OpenAI key to Groq, a Groq key to OpenAI, or any credential to an untrusted
  host; never log keys or complete request bodies.
- Validate endpoint hosts and preserve transcription prompt limits,
  timeout/error classification, and provenance labels.
- Provider changes need tests for OpenAI, Groq, custom endpoints, malformed
  URLs, credential precedence, and fallback behavior.
