#!/usr/bin/env bash
set -euo pipefail
binary=${1:?built wd binary required}
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
report=$(mktemp)
trap 'rm -f -- "$report"' EXIT
status=0
"$binary" self-test audio-capture --duration-ms 500 --json >"$report" || status=$?
bash "$script_dir/check-audio-capture-report.sh" "$report" "$status"
