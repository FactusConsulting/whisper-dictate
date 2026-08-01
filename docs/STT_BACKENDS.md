# Speech-to-text backends

whisper-dictate exposes two speech-to-text engines. The cloud engine uses one
OpenAI-compatible request path, with a provider selector for OpenAI, Groq, or
another compatible endpoint.

| Engine / provider | `stt_backend` | `stt_provider` | Configuration |
|---|---|---|---|
| Local Whisper | `whisper` | _(not used)_ | Set `model`, `device`, and optional `VOICEPI_WHISPER_MODEL_PATH`. |
| OpenAI | `openai` | `openai` | Set the OpenAI base URL, model, and API key. |
| Groq | `openai` | `groq` | Set `https://api.groq.com/openai/v1`, a supported Whisper model, and a Groq API key. |
| Custom OpenAI-compatible endpoint | `openai` | `custom` | Set the endpoint URL, the model name expected by that server, and its API key. |

Groq is not a separate `stt_backend` value because it speaks the same
OpenAI-compatible transcription API as OpenAI. The runtime still records the
provider separately so it can choose the correct URL, model list, credential,
prompt limit, and provenance label.

## Local Whisper

Local transcription uses whisper.cpp with GGML model files. Shipping builds
include the native local backend; source builds need the `whisper-rs-local`
Cargo feature. The model is loaded lazily and can be released after an idle
period with `VOICEPI_WHISPER_IDLE_UNLOAD_S`; the next utterance reloads it.

## Cloud providers

The cloud engine accepts OpenAI-compatible `/v1/audio/transcriptions`
endpoints. Configure the provider, endpoint, and model in the Speech settings
tab or with environment variables. The supported built-in providers are:

- **OpenAI** — `https://api.openai.com/v1`; models include
  `gpt-4o-mini-transcribe`, `gpt-4o-transcribe`, and `whisper-1`.
- **Groq** — `https://api.groq.com/openai/v1`; models include
  `whisper-large-v3-turbo`, `whisper-large-v3`, and
  `distil-whisper-large-v3-en`.
- **Custom** — any reachable OpenAI-compatible server; enter its URL and
  model name directly.

The UI stores provider credentials in the OS credential store. Headless runs
can use `VOICEPI_STT_API_KEY`, `OPENAI_API_KEY`, or `GROQ_API_KEY` as described
in [`CONFIGURATION.md`](CONFIGURATION.md).

Loopback endpoints remain local when `VOICEPI_LOCAL_ONLY=1` is enabled.

For the complete setting and command reference, see
[`CONFIGURATION.md`](CONFIGURATION.md).
