#!/usr/bin/env bash
# Validate the report and exit status together; absence of hardware is the only
# tolerated failure. A stream that opened but never delivered audio is a failure.
set -euo pipefail
report=${1:?report path required}
status=${2:?process exit status required}

if ! jq -s -e --argjson status "$status" '
  def count: type == "number" and . >= 0 and floor == .;
  length == 1 and (.[0] |
    type == "object" and .kind == "audio_capture_self_test" and
    (.succeeded | type == "boolean") and (.error | type == "string") and
    (.frames_captured | count) and (.samples_captured | count) and
    (.sample_rate | count) and .requested_duration_ms == 500 and
    (if $status == 0 then
       .succeeded == true and .error == "" and
       .frames_captured > 0 and .samples_captured > 0 and .sample_rate > 0
     elif $status == 1 then
       .succeeded == false and .frames_captured == 0 and
       .samples_captured == 0 and .sample_rate == 0 and
       (.error == "open capture: no default input device available" or
        .error == "open capture: supported_input_configs: The requested audio device is not available. It may have been disconnected.")
     else false end))
' "$report" >/dev/null; then
  echo "audio-capture smoke failed: invalid report or unexpected failure (exit $status)" >&2
  exit 1
fi
if [ "$status" -eq 1 ]; then
  echo '::notice::audio-capture self-test skipped: no usable input device in this headless container'
else
  echo 'audio-capture self-test passed'
fi
