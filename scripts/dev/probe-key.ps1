[CmdletBinding()]
param(
    [string]$Chord,
    [double]$Duration = 15,
    [ValidateSet('auto','rdev','evdev','register')]
    [string]$Driver = 'auto',
    [switch]$Json,
    [switch]$ExitOnChord,
    [string]$Config,
    [switch]$Release
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$cargoArgs = @('run')
if ($Release) { $cargoArgs += '--release' }
$cargoArgs += @('--manifest-path', (Join-Path $repoRoot 'src/rust/Cargo.toml'), '--features', 'rust-hotkeys', '--', 'hotkey', 'capture', '--for', "$Duration", '--driver', $Driver)
if ($Json) { $cargoArgs += '--json' }
if ($ExitOnChord) { $cargoArgs += '--exit-on-chord' }
if (-not [string]::IsNullOrWhiteSpace($Chord)) { $cargoArgs += @('--chord', $Chord) }
if (-not [string]::IsNullOrWhiteSpace($Config)) { $cargoArgs += @('--config', $Config) }

# Capture stdout and stderr separately. JSON mode must remain valid JSONL on
# stdout even when cargo emits build diagnostics; diagnostics are forwarded to
# stderr for operators and CI logs.
$stderrPath = [System.IO.Path]::GetTempFileName()
try {
    if ($Json) {
        $captured = @(& cargo @cargoArgs 2> $stderrPath)
        $nativeErrors = @(Get-Content -LiteralPath $stderrPath -ErrorAction SilentlyContinue)
        $captured | ForEach-Object { Write-Output $_ }
        $nativeErrors | ForEach-Object { [Console]::Error.WriteLine([string]$_) }
    } else {
        $captured = @(& cargo @cargoArgs 2>&1)
        $nativeErrors = @()
        $captured | ForEach-Object { Write-Output $_ }
    }
    $nativeExit = $LASTEXITCODE
    $text = (($captured + $nativeErrors) | ForEach-Object { [string]$_ }) -join "`n"
} finally {
    Remove-Item -LiteralPath $stderrPath -Force -ErrorAction SilentlyContinue
}
if ($nativeExit -ne 0) {
    if ($text -match '(?i)((unknown|unsupported).*key|key .*not supported)') { exit 3 }
    exit 1
}
if ([string]::IsNullOrWhiteSpace($Chord)) {
    exit 0
}
if ($text -match '(?i)(chord_matched|CHORD MATCHED|exit_on_chord|exit-on-chord fired)') {
    exit 0
}
if ($text -match '(?i)"events"\s*:\s*(\d+)') {
    $events = [int]$Matches[1]
    if ($events -gt 0) { exit 2 }
    exit 1
}
if ($text -match '(?i)Events:\s*(\d+)') {
    $events = [int]$Matches[1]
    if ($events -gt 0) { exit 2 }
    exit 1
}
# A successful process with no terminal summary is not a verified probe.
exit 1
