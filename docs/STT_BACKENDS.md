# STT backend evaluation (#24)

A standing evaluation of speech-to-text backends for whisper-dictate: what ships
today, and which additional adapters are worth building. This is a research note,
not a roadmap commitment — update it when the landscape or our needs change.

## What ships today

| Backend | `VOICEPI_STT_BACKEND` / provider | Runs | Strengths | Limits |
|---|---|---|---|---|
| **faster-whisper** (CTranslate2) | `whisper` | CPU + CUDA, fully local | The workhorse: accurate, multilingual, `large-v3`/`-turbo`, int8/float16. No network. | Heavier on CPU than tiny engines |
| **OpenAI-compatible cloud** | `openai` → provider `OpenAI` / `Groq` | Remote | Zero local compute; Groq is cheap + fast `whisper-large-v3` | Sends audio off-box; needs a key; `VOICEPI_LOCAL_ONLY` blocks it |
| **Custom (self-hosted, OpenAI-compatible)** | `openai` → provider `Custom` | Local container or LAN | Any OpenAI-compatible server (faster-whisper-server, speaches, vLLM-whisper, LocalAI…). Loopback is allowed under local-only | You run/maintain the server |

The decisive point for "should we add more adapters": the **Custom
(OpenAI-compatible)** provider (shipped in #128) already unlocks the entire
ecosystem of self-hosted servers that expose `/v1/audio/transcriptions` —
**without any new code**. So a candidate backend only justifies a *native*
adapter if it is **not** reachable through an OpenAI-compatible endpoint.

### Removed: NVIDIA Parakeet (NeMo)

The optional NVIDIA Parakeet (NeMo) adapter was dropped in Wave 8 of issue
[#348](https://github.com/FactusConsulting/whisper-dictate/issues/348). It was
unmaintained for ~6 months, there are no Rust NeMo bindings to migrate to under
the Python-removal roadmap, and `whisper-large-v3-turbo` covers the
Danish/mixed-Danish-English use case it was kept for. Existing
`stt_backend = "parakeet"` configs migrate to `"whisper"` on the next launch and
the obsolete `parakeet_*` keys are stripped on the next save (see
CONFIGURATION.md → "NVIDIA Parakeet backend (removed in Wave 8 of #348)").

## Candidates evaluated

| Candidate | Reachable via existing paths? | Verdict |
|---|---|---|
| **whisper.cpp / GGML servers** | Yes — most expose an OpenAI-compatible server, or run via `faster-whisper` already | **Skip** native adapter; use Custom provider |
| **speaches / faster-whisper-server / LocalAI / vLLM-whisper** | Yes — OpenAI-compatible | **Skip**; use Custom provider (documented in CONFIGURATION.md) |
| **NVIDIA Canary** (NeMo) | No — needs NeMo | **Skip:** the NeMo runtime path is gone with Parakeet, so adding it back means re-introducing the same maintenance burden (CUDA-only, no Rust NeMo bindings). Only reconsider if the Python-removal roadmap reverses. |
| **Moonshine** (on-device English) | No | **Watch:** very low-latency on-device English; niche until there's demand |
| **Vosk / Kaldi** | No | **Skip:** materially lower accuracy than Whisper; no clear win |
| **Deepgram / AssemblyAI / Azure / Google STT** | No — bespoke REST/streaming APIs, not OpenAI-shaped | **Defer:** each needs a dedicated adapter + key handling; only build on real user demand |
| **wav2vec2 / SeamlessM4T** | No | **Skip:** research-grade; not a dictation upgrade |

## Recommendation

1. **Prefer the Custom provider over new adapters.** Point users at a local
   OpenAI-compatible container for anything Whisper-flavoured (see
   CONFIGURATION.md's self-hosted STT recipe). This is now the answer for most
   "can you add backend X?" requests.
2. **No native NeMo path on the roadmap.** With Parakeet removed in Wave 8 of
   #348, NeMo plumbing is gone; revisit only if a future wave brings back a
   maintained Rust-friendly path.
3. **Cloud non-OpenAI APIs (Deepgram/AssemblyAI/Azure/Google): demand-gated.**
   Track requests; build a bespoke adapter only when there is a concrete user.

No code change accompanies this note — it records the decision that the
OpenAI-compatible Custom provider covers the realistic backend gap, so the
backlog item is "evaluated; defer native adapters pending demand."
