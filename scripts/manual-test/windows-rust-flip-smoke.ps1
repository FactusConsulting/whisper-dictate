# windows-rust-flip-smoke.ps1
#
# Manual Windows smoke test for the native runtime defaults: the mic picker
# (`devices`), post-processing (`postprocess`),
# and the external-api chat shell-out. Validates the CLI-level contracts these
# default paths rely on — the pieces CI cannot cover because the runners have no
# real audio device and no interactive GUI.
#
# Run from anywhere in the repo (or double-click it) — it locates the crate
# root itself from its own script path:
#
#     .\scripts\manual-test\windows-rust-flip-smoke.ps1
#
# Optional: export a cloud key first to also exercise a real post-process call:
#     $env:VOICEPI_POST_API_KEY = "gsk_..."   # Groq
#
# See scripts/manual-test/README.md for the full-app manual checklist that this
# script does NOT cover (GUI picker, live dictation, opt-out behaviour).

$ErrorActionPreference = "Stop"
$pass = 0; $fail = 0
function Check($name, $cond, $detail = "") {
  if ($cond) { Write-Host "PASS  $name" -ForegroundColor Green; $script:pass++ }
  else       { Write-Host "FAIL  $name  $detail" -ForegroundColor Red;  $script:fail++ }
}

# Resolve the crate root from this script's own location (scripts/manual-test/..
# /../src/rust) so the script works regardless of the caller's working directory.
$repoRoot = Resolve-Path (Join-Path $PSScriptRoot "..\..")
$rustDir = Join-Path $repoRoot "src\rust"
Push-Location $rustDir
try {

Write-Host "`n== building CLI (audio-capture) ==" -ForegroundColor Cyan
cargo build --bin wd --features audio-capture --release
$exe = ".\target\release\wd.exe"

# Version (informational — not asserted, so this script stays reusable across RCs)
$ver = (& $exe --version) 2>$null
Write-Host "version: $ver" -ForegroundColor Cyan

# 1) devices: picker request (with DirectSound) lists real mics, no primary-driver alias
$ds = ('{"action":"list","include_directsound":true}' | & $exe devices 2>$null) | ConvertFrom-Json
Check "devices (picker) returns >=1 mic" ($ds.devices.Count -ge 1) "count=$($ds.devices.Count)"
Check "no 'Primary Sound Capture Driver' alias" (-not ($ds.devices.name -contains "Primary Sound Capture Driver"))

# 2) devices: plain CLI request (no DirectSound) is cpal/WASAPI only, still lists mics
$plain = ('{"action":"list"}' | & $exe devices 2>$null) | ConvertFrom-Json
Check "devices (plain) returns >=1 mic" ($plain.devices.Count -ge 1) "count=$($plain.devices.Count)"

# 3) DirectSound enumeration actually returns devices (diagnostic on stderr).
# Capture stderr to a FILE. The obvious-looking `2>&1 1>$null` does not work:
# PowerShell merges stderr INTO stdout first, then discards the merged stream,
# so the diagnostic is swallowed and the check sees an empty string.
$env:VOICEPI_DEBUG_DIRECTSOUND = "1"
$errFile = [System.IO.Path]::GetTempFileName()
try {
  $null = '{"action":"list","include_directsound":true}' | & $exe devices 2>$errFile
  $dsErr = (Get-Content $errFile -Raw)
} finally {
  $env:VOICEPI_DEBUG_DIRECTSOUND = ""
  Remove-Item $errFile -ErrorAction SilentlyContinue
}
Check "DirectSound enumerated device(s)" ($dsErr -match "\[devices:directsound\] enumerated [1-9]") "stderr: $dsErr"

# 4) postprocess: raw passthrough (no network) returns text unchanged
$pp = ('{"action":"process","text":"hello world","settings":{"processor":"none","mode":"raw","model":"x","base_url":"http://localhost:11434","timeout_ms":1000,"max_input_chars":4000,"max_output_chars":4000,"api_key":"","redact":false,"redact_terms":"","local_only":false}}' | & $exe postprocess 2>$null) | ConvertFrom-Json
Check "postprocess raw passthrough unchanged" ($pp.text -eq "hello world" -and -not $pp.fallback)

# 5) postprocess: transport failure (unresolvable host) => fallback_kind = transport
$ppT = ('{"action":"process","text":"clean this","settings":{"processor":"openai","mode":"clean","model":"gpt-4o-mini","base_url":"http://nonexistent.invalid/v1","timeout_ms":1000,"max_input_chars":4000,"max_output_chars":4000,"api_key":"test","redact":false,"redact_terms":"","local_only":false}}' | & $exe postprocess 2>$null) | ConvertFrom-Json
Check "postprocess DNS failure => transport fallback" ($ppT.fallback -and $ppT.fallback_kind -eq "transport") "kind=$($ppT.fallback_kind)"

# 6) external-api: empty key => terminal (no retry); unresolvable host => transport
$eaTerm = ('{"action":"chat_completion","base_url":"https://api.openai.com/v1","api_key":" ","model":"gpt","prompt":"x","timeout_ms":1000}' | & $exe external-api 2>$null) | ConvertFrom-Json
Check "external-api empty key => terminal" ((-not $eaTerm.ok) -and $eaTerm.kind -eq "terminal")
$eaTrans = ('{"action":"chat_completion","base_url":"http://nonexistent.invalid/v1","api_key":"k","model":"gpt","prompt":"x","timeout_ms":1000}' | & $exe external-api 2>$null) | ConvertFrom-Json
Check "external-api DNS failure => transport" ((-not $eaTrans.ok) -and $eaTrans.kind -eq "transport") "kind=$($eaTrans.kind)"

# 7) OPTIONAL real cloud post-process — only runs when VOICEPI_POST_API_KEY is set
if ($env:VOICEPI_POST_API_KEY) {
  $real = (('{"action":"process","text":"so basically um the the meeting is at three","settings":{"processor":"groq","mode":"clean","model":"llama-3.1-8b-instant","base_url":"https://api.groq.com/openai/v1","timeout_ms":8000,"max_input_chars":4000,"max_output_chars":4000,"api_key":"' + $env:VOICEPI_POST_API_KEY + '","redact":false,"redact_terms":"","local_only":false}}') | & $exe postprocess 2>$null) | ConvertFrom-Json
  Check "real cloud post-process cleaned the text" ((-not $real.fallback) -and $real.changed) "err=$($real.error)"
  if (-not $real.fallback) { Write-Host "   cleaned => $($real.text)" -ForegroundColor DarkGray }
} else {
  Write-Host "SKIP  real cloud post-process (set VOICEPI_POST_API_KEY to enable)" -ForegroundColor Yellow
}

Write-Host "`n== $pass passed, $fail failed ==" -ForegroundColor Cyan
if ($fail -gt 0) { exit 1 }

}
finally {
  Pop-Location
}
