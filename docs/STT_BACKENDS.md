# Speech-to-text backends

whisper-dictate can transcribe on a supported local GPU or send audio to an
explicitly configured API endpoint. OpenAI, Groq, and custom providers use an
OpenAI-compatible request path. NVIDIA Nemotron 3.5 ASR supports both a local
in-process runtime and Riva gRPC endpoints.

| Engine / provider | `stt_backend` | `stt_provider` | Configuration |
|---|---|---|---|
| Local Whisper | `whisper` | _(not used)_ | Use a Vulkan-capable build and GPU, select a model that fits, and optionally set `VOICEPI_WHISPER_MODEL_PATH`. |
| In-process Nemotron 3.5 ASR | `openai` | `nemotron` | Use `inproc://nemotron` and a supported GPU. The app downloads and verifies the official NeMo-Speech.cpp runtime and model. No API key, Docker, or server is needed. |
| OpenAI | `openai` | `openai` | Set the OpenAI base URL, model, and API key. |
| Groq | `openai` | `groq` | Set `https://api.groq.com/openai/v1`, a supported Whisper model, and a Groq API key. |
| Custom OpenAI-compatible endpoint | `openai` | `custom` | Set the endpoint URL, the model name expected by that server, and its API key. |
| NVIDIA Nemotron 3.5 ASR (Riva) | `openai` | `nemotron` | Set a Riva endpoint: for example, `grpc://localhost:50051` for a local NIM or `https://grpc.nvcf.nvidia.com:443` for hosted Riva. |

Groq is not a separate `stt_backend` value because it speaks the same
OpenAI-compatible transcription API as OpenAI. The runtime still records the
provider separately so it can choose the correct URL, model list, credential,
prompt limit, and provenance label.

## When local transcription is unavailable

Local Whisper and in-process Nemotron require a supported GPU. Neither falls
back to CPU or switches to a hosted provider if it cannot load or run. If your
GPU cannot fit the chosen model, close other GPU-heavy apps and retry or choose
a smaller supported model, if one is available. If you do not have a suitable
GPU, consider a [hosted provider](#cloud-providers) that supports your language.
The app does not switch providers or send audio just because it recommends
one. Choose and configure a hosted route yourself if you want to use it.
`VOICEPI_LOCAL_ONLY=1` continues to block remote routes; compatible loopback
endpoints remain available.

## Local Whisper

Local transcription uses whisper.cpp with GGML model files. The app must be
built with Vulkan support (`whisper-rs-vulkan` for source builds), and one
Vulkan adapter must have enough memory; the app does not combine memory
across GPUs. Whisper Turbo (`large-v3-turbo`) needs an estimated 2.75 GiB,
while Whisper Quality (`large-v3`) and a ~3.1 GB Edda GGML file need about
4.25 GiB. The estimate for a custom model depends on its file size.

The Windows installer enables Vulkan when built with the Vulkan SDK. The
published Linux bundle and current Nix package do not enable Vulkan for
Whisper. On Linux, build with `--features shipping-vulkan` for local Whisper,
or choose a hosted endpoint with the published bundle.

Settings checks total GPU memory to indicate which models can fit. A model
can still fail to load if too little memory is free at runtime or the GPU
cannot initialize it. The app reports the error rather than running on the
CPU. The model is loaded lazily and can be released after an idle period with
`VOICEPI_WHISPER_IDLE_UNLOAD_S`; the next utterance reloads it.

### Danish Turbo and Danish Quality

In **Speech → Local Whisper**, choose **Danish Turbo** (`edda-v0.2`) or
**Danish Quality** (`edda-v0.2-large`), download the model, then save and
restart the runtime. Start with Turbo for Danish dictation. Quality uses
the full large-v3 model and requires more GPU memory. Neither model is
downloaded or selected automatically.

Both presets always transcribe in Danish, including preview and file
transcription. The app does not overwrite your language setting. Choose a
multilingual model to use it again. Both presets work offline once downloaded.

### Use an Edda model from a local file

If you already have a compatible Edda GGML file, select local Whisper, set the
device to Vulkan and language to Danish (`da`), and enable no-timestamps
decoding explicitly. A custom file takes precedence over any catalog preset
and does not lock the language. Exit the running app first, then launch it
from the same PowerShell session:

```powershell
$env:VOICEPI_STT_BACKEND = 'whisper'
$env:VOICEPI_DEVICE = 'vulkan'
$env:VOICEPI_WHISPER_MODEL_PATH = 'C:\models\edda.ggml.bin'
$env:VOICEPI_WHISPER_NO_TIMESTAMPS = '1'
$env:VOICEPI_LANG = 'da'
wd ui
```

Use the path to your own GGML file. If `config.json` already specifies the
speech engine, device, or language, set the corresponding values in Settings;
those saved values take precedence over the environment variables.
`VOICEPI_WHISPER_NO_TIMESTAMPS=1` disables timestamp generation during local
decoding, not just timestamp display. There is no automatic Edda detection:
this option applies to any local Whisper model loaded by that process.
Default decoding is unchanged when the variable is unset. When switching
back to a built-in model, remove the model path and no-timestamps variables
from the launch shell, restore your language setting, and restart the app.

## In-process Nemotron

The shipping Rust build includes a dynamic adapter for NVIDIA's official
[NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp) C ABI. On
supported hardware, choose Nemotron and **Multilingual / Auto** in
**Speech** to use `inproc://nemotron` with the official multilingual model.
On first use, the runtime downloads the pinned
`nvidia/nemotron-3.5-asr-streaming-0.6b` GGUF (~742 MB) and the matching
platform runtime archive into the user cache, verifies each SHA-256, and loads
the library in-process. No Docker, NIM server, or API key is required. You
can use a custom `.gguf` file that fits the GPU. For offline deployments,
install the official pinned runtime archive in the versioned Nemotron
runtime cache. `VOICEPI_NEMOTRON_LIBRARY` can only point to the verified
managed library for the chosen accelerator.

`Language=Auto` is passed explicitly as `auto` to the multilingual checkpoint
and is detected afresh for each utterance. The managed model requires
at least 2 GiB of available GPU memory. Auto prefers Vulkan and can use
CUDA on supported NVIDIA systems. It never starts a CPU recognizer.
**Test local model** checks GPU model loading without recording,
transcribing audio, or using an API key.

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
- **Nemotron 3.5 ASR** — NVIDIA NIM's Riva streaming endpoint, normally
  `grpc://localhost:50051`. The Speech tab offers two model profiles:
  `nvidia/nemotron-speech-streaming-en-0.6b` (English-only) and
  `nvidia/nemotron-3.5-asr-streaming-0.6b` (multilingual, including automatic
  language detection). Configure the local NIM with the matching
  `NIM_TAGS_SELECTOR=type=en-US` or `NIM_TAGS_SELECTOR=type=multi` profile.
  Leave Language on Auto with the multilingual profile; Auto is sent as the
  current Riva request value `language_code=auto` (the `multi` value is a
  deployment selector, not a request language), and compact UI hints such as
  `en`/`da` are sent as Nemotron's regional `en-US`/`da-DK` locales. The
  English profile is appropriate when every utterance is English;
  selecting it in the Speech tab automatically changes an Auto/non-English
  language hint to English; use Multilingual / Auto when languages may change
  between utterances. Saving a hand-edited English-profile config with Auto is
  rejected with an actionable validation message instead of sending an
  unsupported request.
  **English** selects NVIDIA's public hosted NVCF endpoint. **Multilingual /
  Auto** offers the local route only on suitable hardware. For a
  user-managed NIM or hosted multilingual function, enter its address in
  the advanced Speech settings.
  NVCF still uses the profile attached to its function id; editing the model id
  alone cannot change a custom deployment.
  A local NIM needs no runtime API key; remote deployments can use
  `VOICEPI_STT_API_KEY`. For NVIDIA's
  [hosted Nemotron Build endpoint](https://build.nvidia.com/nvidia/nemotron-asr-streaming/api),
  set the URL to `https://grpc.nvcf.nvidia.com:443` and provide the NVIDIA
  function API key. The public Build function is currently English-only; the
  multilingual model dropdown cannot change that remote deployment. For a
  multilingual hosted function, append the function id issued for that
  deployment: `https://grpc.nvcf.nvidia.com:443?function-id=YOUR_FUNCTION_ID`. Test API
  sends a small real Riva `StreamingRecognize` smoke request (the same RPC as
  live dictation), rather than probing `/models` or a config RPC.

NVIDIA's NIM quick start (requires an NGC API key and an NVIDIA GPU) is:

```powershell
docker run --rm --runtime=nvidia --gpus all --shm-size=8GB `
  -e NGC_API_KEY -e NIM_HTTP_API_PORT=9000 -e NIM_GRPC_API_PORT=50051 `
  -p 9000:9000 -p 50051:50051 `
  -e NIM_TAGS_SELECTOR=type=multi `
  nvcr.io/nim/nvidia/nemotron-asr-streaming:latest
```

For an English-only deployment, use `-e NIM_TAGS_SELECTOR=type=en-US` and
select `nvidia/nemotron-speech-streaming-en-0.6b` in the app. Do not select
that profile when you need Auto language detection; the multilingual profile
is the one that can detect a different language on each utterance.

Point **Test API** and live dictation at `grpc://localhost:50051` to exercise
the local Riva service. Port 9000 is NIM's HTTP/WebSocket surface and is not a
valid URL for this application's Nemotron streaming client.

### Third-party Hviske models

In **Speech → Cloud STT → Custom**, select **Hviske v5.3** or **Hviske v6**.
The app shows the license and audio-routing notice before changing the model.
The published weights are [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/).
For commercial use, [contact syv.ai](https://syv.ai/kontakt).

Run the model in a separate GPU-backed server that accepts
`POST /v1/audio/transcriptions` and returns JSON with a `text` field. See
the [v5.3 setup](https://huggingface.co/syvai/hviske-v5.3/blob/5d1a09822018702dc51d763e3a867b62d26b3501/README.md#run-with-vllm-openai-compatible-api)
and the [v6 plugin requirements](https://huggingface.co/syvai/hviske-v6/blob/323b5ffdf54ef22196340a0b37cb11e668fefc5a/vllm/README.md).
For v6, verify that your deployment can transcribe audio; a successful
server start alone does not establish compatibility.

Enter the server's `/v1` base URL and save. For a server on the same PC,
`http://127.0.0.1:8000/v1` is the usual URL. The two preset model IDs send
`da` and omit speech prompts to preserve the server's decoder controls;
local dictionary replacements still apply. For [syv.ai's hosted API](https://platform.syv.ai/docs),
use the model ID shown by that service and its API key; its model IDs can
differ from the self-hosted presets.

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
speech fixture, sends no key in command-line arguments, and defaults to the
public English function (`en-US`). Set `NEMOTRON_GRPC_ENDPOINT` with a custom
`function-id`, `NEMOTRON_MODEL`, and `NEMOTRON_LANGUAGE_CODE=auto` to exercise
an owned multilingual deployment.

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
- A successful hosted gRPC **Test API** check proves that the key, selected
  function, language, and streaming protobuf request can reach the Riva
  service. The runtime uses that same Riva endpoint for live transcription; an
  `http: invalid format` error indicates an older binary or an HTTP URL pasted
  into the Nemotron field. Use `grpc://localhost:50051` locally or
  `https://grpc.nvcf.nvidia.com:443` (optionally with
  `?function-id=YOUR_FUNCTION_ID`) for hosted NVCF.

Loopback endpoints remain local when `VOICEPI_LOCAL_ONLY=1` is enabled.

For the complete setting and command reference, see
[`CONFIGURATION.md`](CONFIGURATION.md).
