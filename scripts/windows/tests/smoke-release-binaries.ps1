# Extracted release validation; preserve the CI contract.
$ErrorActionPreference = 'Stop'
$exe = "target/release/wd.exe"
$guiExe = "target/release/wd-gui.exe"

function Assert-PeSubsystem([string] $path, [UInt16] $expected, [string] $name) {
  $bytes = [System.IO.File]::ReadAllBytes($path)
  $peOffset = [System.BitConverter]::ToUInt32($bytes, 0x3C)
  $subsystem = [System.BitConverter]::ToUInt16($bytes, $peOffset + 0x5C)
  if ($subsystem -ne $expected) {
    throw "$name PE subsystem is $subsystem (expected $expected)."
  }
}

Assert-PeSubsystem $exe 3 'wd.exe'
Assert-PeSubsystem $guiExe 2 'wd-gui.exe'
Write-Host "release PE subsystem checks passed"

$out = & $exe config path
if ([string]::IsNullOrWhiteSpace($out)) {
  throw "release CLI produced NO stdout for 'config path' — CLI dispatch is broken"
}
Write-Host "release CLI stdout OK: $out"
$st = & $exe self-test audio-capture --json --duration-ms 200
if ([string]::IsNullOrWhiteSpace($st)) {
  throw "release CLI produced NO stdout for 'self-test audio-capture --json' — audio verb dispatch is broken"
}
try {
  $doc = $st | ConvertFrom-Json -ErrorAction Stop
} catch {
  throw "release CLI 'self-test audio-capture --json' emitted non-JSON stdout — audio verb JSON envelope regression: $_ / raw stdout: $st"
}
if ($doc.kind -ne 'audio_capture_self_test') {
  throw "release CLI 'self-test audio-capture --json' returned kind='$($doc.kind)' (expected 'audio_capture_self_test') — envelope contract regression"
}
Write-Host "audio-capture self-test JSON envelope OK (kind=audio_capture_self_test, succeeded=$($doc.succeeded))"

exit 0
