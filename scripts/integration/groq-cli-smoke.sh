#!/usr/bin/env bash
#
# Real-audio, cross-OS component smoke for the Rust CLI.
#
# Drives the shipping stock verbs end-to-end against a real 0.5 s WAV
# ("hello") using Groq's cloud API:
#
#   cloud-transcribe (Groq STT)  ->  postprocess (Groq LLM clean)  ->  format-text
#
# This exercises the actual Rust components -- cloud STT (ureq + hound WAV
# decode), the post-processing pipeline, and the spoken-format-command layer --
# on a genuine audio snippet, with NO local Whisper model, GPU, or microphone.
# Because Groq does the transcription over HTTP, it runs identically on
# ubuntu-latest and windows-2025 (invoke with `shell: bash`).
#
# Skips cleanly (exit 0) when GROQ_API_KEY is absent, e.g. fork PRs -- same
# contract as the Python `groq-integration` job. Intended to run NON-REQUIRED:
# a Groq network / quota hiccup must never gate a merge.
#
# Usage:
#   GROQ_API_KEY=... bash scripts/integration/groq-cli-smoke.sh [WAV_PATH]
#
# Verifies wiring (auth + URL + model + WAV decode + the verb chain), not exact
# transcript text -- matching the Python groq smoke's "not exact output" stance.

set -euo pipefail

WAV="${1:-src/python/tests/fixtures/hello.wav}"
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

echo "[groq-cli-smoke] 1/3 cloud-transcribe '$WAV' via Groq ($STT_MODEL)"
stt_json="$(run_cli cloud-transcribe \
  --base-url "$GROQ_BASE" --api-key "$GROQ_API_KEY" \
  --model "$STT_MODEL" --audio-wav-path "$WAV")"
transcript="$(printf '%s' "$stt_json" | json_field text)"
if [[ -z "${transcript//[[:space:]]/}" ]]; then
  echo "[groq-cli-smoke] FAIL: empty transcript from cloud-transcribe: $stt_json" >&2
  exit 1
fi
echo "[groq-cli-smoke]   transcript: $transcript"

echo "[groq-cli-smoke] 2/3 postprocess (Groq clean, $POST_MODEL)"
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

echo "[groq-cli-smoke] 3/3 format-text (en command set)"
fmt_json="$(run_cli format-text --text "$post_text" --command-set en)"
printf '%s' "$fmt_json" | json_field text >/dev/null

echo "[groq-cli-smoke] OK: STT -> post-process -> format chain succeeded on real audio."
