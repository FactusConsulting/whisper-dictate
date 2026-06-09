# STT backend evaluation (#24)

A standing evaluation of speech-to-text backends for whisper-dictate: what ships
today, and which additional adapters are worth building. This is a research note,
not a roadmap commitment — update it when the landscape or our needs change.

## What ships today

| Backend | `VOICEPI_STT_BACKEND` / provider | Runs | Strengths | Limits |
|---|---|---|---|---|
| **faster-whisper** (CTranslate2) | `whisper` | CPU + CUDA, fully local | The workhorse: accurate, multilingual, `large-v3`/`-turbo`, int8/float16. No network. | Heavier on CPU than tiny engines |
| **NVIDIA Parakeet** (NeMo) | `parakeet` | CUDA only, local | Very fast; `parakeet-tdt-0.6b-v3` is strong on Danish/mixed and many languages | NVIDIA GPU + NeMo deps; no CPU path |
| **OpenAI-compatible cloud** | `openai` → provider `OpenAI` / `Groq` | Remote | Zero local compute; Groq is cheap + fast `whisper-large-v3` | Sends audio off-box; needs a key; `VOICEPI_LOCAL_ONLY` blocks it |
| **Custom (self-hosted, OpenAI-compatible)** | `openai` → provider `Custom` | Local container or LAN | Any OpenAI-compatible server (faster-whisper-server, speaches, vLLM-whisper, LocalAI…). Loopback is allowed under local-only | You run/maintain the server |

The decisive point for "should we add more adapters": the **Custom
(OpenAI-compatible)** provider (shipped in #128) already unlocks the entire
ecosystem of self-hosted servers that expose `/v1/audio/transcriptions` —
**without any new code**. So a candidate backend only justifies a *native*
adapter if it is **not** reachable through an OpenAI-compatible endpoint.

## Candidates evaluated

| Candidate | Reachable via existing paths? | Verdict |
|---|---|---|
| **whisper.cpp / GGML servers** | Yes — most expose an OpenAI-compatible server, or run via `faster-whisper` already | **Skip** native adapter; use Custom provider |
| **speaches / faster-whisper-server / LocalAI / vLLM-whisper** | Yes — OpenAI-compatible | **Skip**; use Custom provider (documented in README) |
| **NVIDIA Canary** (NeMo) | No — needs NeMo, like Parakeet | **Consider (low effort):** slots into the existing Parakeet/NeMo plumbing; strong multilingual quality. Best ROI of the natives |
| **Moonshine** (on-device English) | No | **Watch:** very low-latency on-device English; niche until there's demand |
| **Vosk / Kaldi** | No | **Skip:** materially lower accuracy than Whisper/Parakeet; no clear win |
| **Deepgram / AssemblyAI / Azure / Google STT** | No — bespoke REST/streaming APIs, not OpenAI-shaped | **Defer:** each needs a dedicated adapter + key handling; only build on real user demand |
| **wav2vec2 / SeamlessM4T** | No | **Skip:** research-grade; not a dictation upgrade |

## Recommendation

1. **Prefer the Custom provider over new adapters.** Point users at a local
   OpenAI-compatible container for anything Whisper-flavoured (see the README
   "Self-host the STT model in a container" section). This is now the answer for
   most "can you add backend X?" requests.
2. **Only native add worth queuing: NVIDIA Canary** via the NeMo path, if/when
   multilingual quality beyond Parakeet is requested — it reuses existing
   plumbing and stays GPU-local.
3. **Cloud non-OpenAI APIs (Deepgram/AssemblyAI/Azure/Google): demand-gated.**
   Track requests; build a bespoke adapter only when there is a concrete user.

No code change accompanies this note — it records the decision that the
OpenAI-compatible Custom provider covers the realistic backend gap, so the
backlog item is "evaluated; defer native adapters pending demand."
