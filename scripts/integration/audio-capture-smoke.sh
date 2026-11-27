#!/usr/bin/env bash
set -euo pipefail
binary=${1:?built wd binary required}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
report=$(mktemp)
trap 'rm -f -- "$report"' EXIT
status=0
# Use ALSA's synthetic source explicitly; the app must not select it as a mic.
"$binary" self-test audio-capture --device null --duration-ms 500 --json >"$report" || status=$?
bash "$script_dir/check-audio-capture-report.sh" "$report" "$status"
