param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [Parameter(Mandatory = $true)]
    [string]$Version,

    [switch]$LegacyOnnxRequired
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$source = (Resolve-Path -LiteralPath $SourceRoot).Path
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $output | Out-Null
$output = (Resolve-Path -LiteralPath $output).Path
$stagingRoot = Join-Path $output 'zip-windows'
$bundle = Join-Path $stagingRoot 'whisper-dictate'
$zip = Join-Path $output "whisper-dictate-windows-$Version.zip"

foreach ($candidate in @($stagingRoot, $zip)) {
    $full = [System.IO.Path]::GetFullPath($candidate)
    $prefix = $output.TrimEnd('\') + '\'
    if (-not $full.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to replace portable-package path outside output directory: $full"
    }
}

if (Test-Path -LiteralPath $stagingRoot) {
    Remove-Item -LiteralPath $stagingRoot -Recurse -Force
}
if (Test-Path -LiteralPath $zip) {
    Remove-Item -LiteralPath $zip -Force
}

New-Item -ItemType Directory -Force -Path $bundle | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'shared\config') | Out-Null
Copy-Item -LiteralPath (Join-Path $source 'shared\config\settings_schema.json') `
    -Destination (Join-Path $bundle 'shared\config')
foreach ($name in @('README.md', 'LICENSE', 'VERSION')) {
    Copy-Item -LiteralPath (Join-Path $source $name) -Destination $bundle
}
Copy-Item -LiteralPath (Join-Path $source 'docs') -Destination $bundle -Recurse
New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'assets') | Out-Null
Copy-Item -LiteralPath (Join-Path $source 'assets\whisper-dictate.ico') `
    -Destination (Join-Path $bundle 'assets')

$releaseDir = Join-Path $source 'target\release'
foreach ($binary in @('wd.exe', 'wd-gui.exe')) {
    Copy-Item -LiteralPath (Join-Path $releaseDir $binary) -Destination $bundle
}
$onnxDlls = @(Get-ChildItem -LiteralPath $releaseDir -Filter 'onnxruntime*.dll' -File)
if ($LegacyOnnxRequired -and $onnxDlls.Count -eq 0) {
    throw 'Legacy portable package requires ONNX Runtime, but no sidecar reached target\release'
}
foreach ($dll in $onnxDlls) {
    Copy-Item -LiteralPath $dll.FullName -Destination $bundle -Force
}

New-Item -ItemType Directory -Force -Path (Join-Path $bundle 'benchmark') | Out-Null
Copy-Item -LiteralPath (Join-Path $source 'benchmark\corpus.json') `
    -Destination (Join-Path $bundle 'benchmark')
Compress-Archive -Path $bundle -DestinationPath $zip -CompressionLevel Optimal

if ($LegacyOnnxRequired) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($zip)
    try {
        $hasOnnx = $archive.Entries | Where-Object { $_.Name -like 'onnxruntime*.dll' }
        if (-not $hasOnnx) {
            throw "Legacy ZIP is missing its required ONNX Runtime sidecar: $zip"
        }
    } finally {
        $archive.Dispose()
    }
}

Write-Output $zip
