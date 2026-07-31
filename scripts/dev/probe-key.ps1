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

# Capture output so the wrapper can preserve the documented verdict contract:
# a successful listener install is not the same as a verified chord. The Rust
# CLI deliberately exits 0 when its observation window expires; this script
# maps the emitted match/count data to the operator-facing 0/1/2 statuses.
$captured = @(& cargo @cargoArgs 2>&1)
$nativeExit = $LASTEXITCODE
$captured | ForEach-Object { Write-Output $_ }
$text = ($captured | ForEach-Object { [string]$_ }) -join "`n"
if ($nativeExit -ne 0) {
    if ($text -match '(?i)(unknown|unsupported).*key') { exit 3 }
    exit 1
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
