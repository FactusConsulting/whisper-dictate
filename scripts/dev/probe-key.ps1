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
& cargo @cargoArgs
exit $LASTEXITCODE
