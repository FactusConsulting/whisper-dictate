#!/usr/bin/env bash
#
# Real-audio, cross-OS smoke for the Rust CLI + in-process engine.
#
# Two halves, both against Groq's cloud API on a real WAV:
#
#   1-3) component chain:
#          cloud-transcribe (Groq STT) -> postprocess (Groq clean) -> format-text
#   4)   full in-process Rust engine end-to-end:
#          simulate-session drives the real DictateSession over a WAV
#          (transcribe -> post-process -> format -> inject-preview)
#
# Step 4 is the Rust-engine counterpart of the Python `simulate-ptt` offline
# drive, so BOTH engines get CLI integration coverage while Python still
# exists. Everything runs with NO local Whisper model, GPU, or microphone
# (Groq does STT over HTTP), so it behaves identically on ubuntu-latest and
# windows-2025 (invoke with `shell: bash`).
#
# Skips cleanly (exit 0) when GROQ_API_KEY is absent, e.g. fork PRs -- same
# contract as the Python `groq-integration` job. Intended to run NON-REQUIRED:
# a Groq network / quota hiccup must never gate a merge.
#
# Usage:
#   GROQ_API_KEY=... bash scripts/integration/groq-cli-smoke.sh [WAV_PATH]

set -euo pipefail

WAV="${1:-src/python/tests/fixtures/hello.wav}"
# Real (machine-synthesized) speech for the full-session drive, so step 4 can
# assert an actual transcript rather than just a round trip.
SPEECH_WAV="src/rust/tests/fixtures/hello_speech.wav"
GROQ_BASE="https://api.groq.com/openai/v1"
STT_MODEL="whisper-large-v3-turbo"
POST_MODEL="llama-3.3-70b-versatile"

if [[ -z "${GROQ_API_KEY:-}" ]]; then
  echo "[groq-cli-smoke] GROQ_API_KEY not set; skipping (fork PR / no secret)."
  exit 0
fi

if [[ ! -f "$WAV" ]]; then
  echo "[groq-cli-smoke] FAIL: WAV fixture not found: $WAV" >&2
  exit 1
fi

# `python3` on ubuntu, `python` on the windows runner's bash -- resolve once.
PY="$(command -v python3 || command -v python || true)"
if [[ -z "$PY" ]]; then
  echo "[groq-cli-smoke] FAIL: no python interpreter on PATH for JSON parsing" >&2
  exit 1
fi

# Invoke the CLI through cargo so the binary path / .exe suffix is handled for
# us on both OSes. `--quiet` keeps build chatter off stdout so the JSON is
# clean; the first call pays the (cached) build cost.
run_cli() {
  cargo run --quiet --release \
    --manifest-path src/rust/Cargo.toml --target-dir src/rust/target \
    -p whisper-dictate-app -- "$@"
}

# Extract a top-level string field from a JSON object on stdin. python3 is
# preinstalled on both GitHub-hosted ubuntu and windows runners.
json_field() { "$PY" -c "import sys,json;print(json.load(sys.stdin)[sys.argv[1]])" "$1"; }

echo "[groq-cli-smoke] 1/4 cloud-transcribe '$WAV' via Groq ($STT_MODEL)"
stt_json="$(run_cli cloud-transcribe \
  --base-url "$GROQ_BASE" --api-key "$GROQ_API_KEY" \
  --model "$STT_MODEL" --audio-wav-path "$WAV")"
transcript="$(printf '%s' "$stt_json" | json_field text)"
if [[ -z "${transcript//[[:space:]]/}" ]]; then
  echo "[groq-cli-smoke] FAIL: empty transcript from cloud-transcribe: $stt_json" >&2
  exit 1
fi
echo "[groq-cli-smoke]   transcript: $transcript"

echo "[groq-cli-smoke] 2/4 postprocess (Groq clean, $POST_MODEL)"
# Build the stdin envelope the `postprocess` verb consumes. postprocess_text
# falls back to the input text on any provider error, so this asserts the verb
# runs and yields non-empty text (wiring), not that the LLM changed anything.
post_envelope="$("$PY" - "$transcript" "$GROQ_API_KEY" "$GROQ_BASE" "$POST_MODEL" <<'PY'
import json, sys
text, key, base, model = sys.argv[1:5]
print(json.dumps({
    "action": "process",
    "text": text,
    "settings": {
        "processor": "groq", "mode": "clean", "model": model,
        "base_url": base, "api_key": key,
    },
}))
PY
)"
post_json="$(printf '%s' "$post_envelope" | run_cli postprocess)"
post_text="$(printf '%s' "$post_json" | json_field text)"
if [[ -z "${post_text//[[:space:]]/}" ]]; then
  echo "[groq-cli-smoke] FAIL: empty post-process text: $post_json" >&2
  exit 1
fi
echo "[groq-cli-smoke]   post-processed: $post_text"

echo "[groq-cli-smoke] 3/4 format-text (en command set)"
fmt_json="$(run_cli format-text --text "$post_text" --command-set en)"
printf '%s' "$fmt_json" | json_field text >/dev/null

echo "[groq-cli-smoke] 4/4 simulate-session: drive DictateSession over '$SPEECH_WAV'"
if [[ ! -f "$SPEECH_WAV" ]]; then
  echo "[groq-cli-smoke] FAIL: speech fixture not found: $SPEECH_WAV" >&2
  exit 1
fi
# The in-process Rust engine reads its cloud STT backend from VOICEPI_STT_*.
# The verb prints the final (preview-injected) transcript to stdout. With no
# post-processor / format-commands set, that is the raw Groq transcript of the
# spoken "hello world" clip -- assert it is non-empty and carries the words.
session_text="$(
  VOICEPI_STT_BASE_URL="$GROQ_BASE" \
  VOICEPI_STT_MODEL="$STT_MODEL" \
  VOICEPI_STT_API_KEY="$GROQ_API_KEY" \
    run_cli simulate-session --wav "$SPEECH_WAV"
)"
session_lc="$(printf '%s' "$session_text" | tr '[:upper:]' '[:lower:]')"
if [[ -z "${session_text//[[:space:]]/}" ]]; then
  echo "[groq-cli-smoke] FAIL: DictateSession produced no transcript" >&2
  exit 1
fi
if [[ "$session_lc" != *hello* && "$session_lc" != *world* ]]; then
  echo "[groq-cli-smoke] FAIL: session transcript missing spoken words: $session_text" >&2
  exit 1
fi
echo "[groq-cli-smoke]   session transcript: $session_text"

echo "[groq-cli-smoke] OK: component chain + full DictateSession drive succeeded on real audio."
