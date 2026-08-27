# Speech-to-text backends

whisper-dictate exposes two speech-to-text engines. The cloud engine uses an
OpenAI-compatible request path for OpenAI, Groq, and custom providers, plus
the Riva gRPC path for NVIDIA Nemotron 3.5 ASR.

| Engine / provider | `stt_backend` | `stt_provider` | Configuration |
|---|---|---|---|
| Local Whisper | `whisper` | _(not used)_ | Set `model`, `device`, and optional `VOICEPI_WHISPER_MODEL_PATH`. |
| OpenAI | `openai` | `openai` | Set the OpenAI base URL, model, and API key. |
| Groq | `openai` | `groq` | Set `https://api.groq.com/openai/v1`, a supported Whisper model, and a Groq API key. |
| Custom OpenAI-compatible endpoint | `openai` | `custom` | Set the endpoint URL, the model name expected by that server, and its API key. |
| NVIDIA Nemotron 3.5 ASR (NIM) | `openai` | `nemotron` | Choose the English profile (`nvidia/nemotron-speech-streaming-en-0.6b`) or the multilingual profile (`nvidia/nemotron-3.5-asr-streaming-0.6b`) in the Speech tab. Deploy the matching local NIM profile with `type=en-US` or `type=multi`; Auto only omits the request language hint and therefore requires `type=multi`. Hosted Riva gRPC endpoints such as `https://grpc.nvcf.nvidia.com:443` use the profile attached to their function id. |

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
- **Nemotron 3.5 ASR** — NVIDIA NIM's streaming endpoint, normally
  `http://localhost:9000/v1`. The Speech tab offers two model profiles:
  `nvidia/nemotron-speech-streaming-en-0.6b` (English-only) and
  `nvidia/nemotron-3.5-asr-streaming-0.6b` (multilingual, including automatic
  language detection). Configure the local NIM with the matching
  `NIM_TAGS_SELECTOR=type=en-US` or `NIM_TAGS_SELECTOR=type=multi` profile.
  Leave Language on Auto with the multilingual profile; Auto does not select
  a deployment profile. The Riva adapter omits `language_code` (the `multi`
  value is a deployment selector, not a request language), and compact UI
  hints such as `en`/`da` are sent as Nemotron's regional `en-US`/`da-DK`
  locales. The English profile is appropriate when every utterance is English;
  selecting it in the Speech tab automatically changes an Auto/non-English
  language hint to English; use Multilingual / Auto when languages may change
  between utterances. Saving a hand-edited English-profile config with Auto is
  rejected with an actionable validation message instead of sending an
  unsupported request.
  Hosted NVCF uses the profile attached to its function id, so changing the
  dropdown cannot switch a hosted deployment that exposes only one profile.
  A local NIM needs no runtime API key; remote deployments can use
  `VOICEPI_STT_API_KEY`. For NVIDIA's
  [hosted Nemotron Build endpoint](https://build.nvidia.com/nvidia/nemotron-asr-streaming/api),
  set the URL to `https://grpc.nvcf.nvidia.com:443` and provide the NVIDIA
  function API key. Test API calls Riva's `GetRivaSpeechRecognitionConfig` RPC
  (including the hosted function id) instead of probing `/models`, and live
  transcription uses Riva's `StreamingRecognize` RPC with the same credentials.

NVIDIA's NIM quick start (requires an NGC API key and an NVIDIA GPU) is:

```powershell
docker run --rm --runtime=nvidia --gpus all --shm-size=8GB `
  -e NGC_API_KEY -e NIM_HTTP_API_PORT=9000 -p 9000:9000 `
  -e NIM_TAGS_SELECTOR=type=multi `
  nvcr.io/nim/nvidia/nemotron-asr-streaming:latest
```

For an English-only deployment, use `-e NIM_TAGS_SELECTOR=type=en-US` and
select `nvidia/nemotron-speech-streaming-en-0.6b` in the app. Do not select
that profile when you need Auto language detection; the multilingual profile
is the one that can detect a different language on each utterance.

To expose the local Riva port as well, add
`-e NIM_GRPC_API_PORT=50051 -p 50051:50051`. Point **Test API** at
`http://localhost:50051` to exercise that local gRPC service and use the same
URL for live dictation; alternatively keep live dictation on the HTTP port
(`http://localhost:9000/v1`).

The UI stores provider credentials in the OS credential store. Headless runs
can use `VOICEPI_STT_API_KEY`, `OPENAI_API_KEY`, or `GROQ_API_KEY` as described
in [`CONFIGURATION.md`](CONFIGURATION.md).

### Hosted Nemotron integration test

The repository includes a cross-platform live test for the hosted Riva path:

```bash
NEMOTRON_API_KEY=... cargo test --manifest-path src/rust/Cargo.toml \
  --test nemotron_cloud_stt -- --nocapture
```

The dedicated `nemotron-integration-rust` workflow runs this test on Ubuntu and
Windows when the repository's `NEMOTRON_API_KEY` secret is available. It is an
informational, non-required check because hosted service availability and
quota are outside the application's control; runs without the secret skip
without contacting NVIDIA. The test uses the bundled synthetic “hello world”
speech fixture, sends no key in command-line arguments, and exercises Auto
language by omitting the Riva `language_code`.

### Nemotron credential and startup errors

- `NGC_API_KEY` is an NGC registry/model-download credential consumed by
  Docker/NIM. `NVIDIA_API_KEY` is the hosted Build/NVCF invocation credential;
  they are not interchangeable. The app intentionally accepts the latter
  only after it is saved as the Nemotron provider key (or mapped for one
  headless session to `VOICEPI_STT_API_KEY`).
- `401` or `403` while NIM downloads a manifest means the NGC account/key does
  not have access to that model/profile. Check the model entitlement in the
  NVIDIA NGC/Build account; changing the app URL cannot fix an NGC permission
  failure.
- `TensorRT is not available` or `could not load ... libcuda.so` is a local
  container runtime problem, not an API-key problem. Verify that Docker is
  using the WSL2/NVIDIA-enabled daemon and that
  `docker run --rm --gpus all nvidia/cuda:12.6.3-base-ubuntu22.04 nvidia-smi`
  succeeds before starting the NIM container.
- A successful hosted gRPC **Test API** check proves that the key can reach
  the Riva service. The runtime uses that same Riva endpoint for live
  transcription; an `http: invalid format` error indicates an older binary
  that still attempted to append `/audio/transcriptions` to the bare gRPC
  authority. Update to the latest release and keep the endpoint as
  `https://grpc.nvcf.nvidia.com:443` (or the documented bare authority).

Loopback endpoints remain local when `VOICEPI_LOCAL_ONLY=1` is enabled.

For the complete setting and command reference, see
[`CONFIGURATION.md`](CONFIGURATION.md).
