param(
    [Parameter(Mandatory = $true)]
    [string]$ManifestPath,

    [Parameter(Mandatory = $true)]
    [string]$ReleaseTag,

    [switch]$AsJson
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if ($ReleaseTag -notmatch '^v(?<major>0|[1-9][0-9]*)\.(?<minor>0|[1-9][0-9]*)\.(?<patch>0|[1-9][0-9]*)(?:-rc\.(?:0|[1-9][0-9]*))?$') {
    throw "Unsupported release tag '$ReleaseTag': expected vX.Y.Z or vX.Y.Z-rc.N."
}
$releaseVersion = [version]::new(
    [int]$Matches.major,
    [int]$Matches.minor,
    [int]$Matches.patch
)
$minimumRebuildVersion = [version]::new(1, 25, 0)
if ($releaseVersion -lt $minimumRebuildVersion) {
    throw "Unsupported release tag '$ReleaseTag': manual native installer rebuilds require v1.25.0 or newer because earlier tags use incompatible binary and portable-package layouts."
}

if (-not (Test-Path -LiteralPath $ManifestPath -PathType Leaf)) {
    throw "Unsupported release tag: Rust manifest not found at '$ManifestPath'. This tag predates the native Rust desktop release."
}

$featureNames = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::Ordinal
)
$inFeatures = $false
foreach ($line in Get-Content -LiteralPath $ManifestPath) {
    if ($line -match '^\s*\[([^]]+)\]\s*$') {
        $inFeatures = $Matches[1] -eq 'features'
        continue
    }
    if ($inFeatures -and $line -match '^\s*([A-Za-z0-9_-]+)\s*=') {
        [void]$featureNames.Add($Matches[1])
    }
}

if ($featureNames.Contains('shipping') -and $featureNames.Contains('shipping-vulkan')) {
    $plan = [pscustomobject]@{
        Mode                = 'named'
        CpuFeatures         = @('shipping')
        VulkanFeatures      = @('shipping-vulkan')
        SupportsVulkan      = $true
        OnnxRuntimeRequired = $false
    }
} else {
    # Preserve only features that the checked-out tag actually declares. Older
    # native releases accumulated these incrementally, so passing today's full
    # list to every historical manifest makes Cargo reject unknown features.
    $cpuFeatures = @(
        'rust-injection',
        'rust-hotkeys',
        'audio-in-rust',
        'audio-capture',
        'whisper-rs-local'
    ) | Where-Object {
        $featureNames.Contains($_) -and
        ($_ -ne 'audio-capture' -or -not $featureNames.Contains('audio-in-rust'))
    }
    $supportsVulkan = $featureNames.Contains('whisper-rs-vulkan')
    $vulkanFeatures = @($cpuFeatures)
    if ($supportsVulkan) {
        $vulkanFeatures += 'whisper-rs-vulkan'
    }
    $plan = [pscustomobject]@{
        Mode                = 'legacy'
        CpuFeatures         = @($cpuFeatures)
        VulkanFeatures      = @($vulkanFeatures)
        SupportsVulkan      = $supportsVulkan
        OnnxRuntimeRequired = $cpuFeatures -contains 'audio-in-rust'
    }
}

if ($AsJson) {
    $plan | ConvertTo-Json -Compress -Depth 4
} else {
    $plan
}
