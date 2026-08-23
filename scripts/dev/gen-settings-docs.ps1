[CmdletBinding()]
param(
    [switch]$Check,
    [string]$Root
)

$ErrorActionPreference = 'Stop'
$repoRoot = if ([string]::IsNullOrWhiteSpace($Root)) {
    (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
} else {
    (Resolve-Path -LiteralPath $Root).Path
}
$schemaPath = Join-Path $repoRoot 'shared/config/settings_schema.json'
$docsPath = Join-Path $repoRoot 'docs/CONFIGURATION.md'
$begin = '<!-- BEGIN GENERATED SETTINGS REFERENCE -->'
$end = '<!-- END GENERATED SETTINGS REFERENCE -->'
$titles = [ordered]@{
    core = 'Core (the first-time-setup basics)'
    'stt-local' = 'Local speech-to-text (Whisper)'
    'stt-cloud' = 'Cloud speech-to-text (OpenAI-compatible APIs)'
    audio = 'Audio capture & voice activity'
    postprocess = 'Dictionary & post-processing'
    injection = 'Injection, hotkeys & feedback'
    diagnostics = 'Diagnostics, history & automation'
    updates = 'Update checks'
}

function Escape-Cell([object]$Value) {
    if ($null -eq $Value) { return '' }
    return ([string]$Value).Replace('|', '\|').Replace("`r", ' ').Replace("`n", ' ').Trim()
}
function Default-Cell([object]$Value) {
    if ($null -eq $Value -or [string]::IsNullOrEmpty([string]$Value)) { return '_(unset)_' }
    return ('`{0}`' -f (Escape-Cell $Value))
}

$schema = Get-Content -LiteralPath $schemaPath -Raw | ConvertFrom-Json
$categories = @($schema.settings | ForEach-Object { $_.category } | Sort-Object -Unique)
$unknownCategories = @($categories | Where-Object { -not $titles.Contains($_) })
if ($unknownCategories.Count -gt 0) {
    [Console]::Error.WriteLine("unknown settings categories: $($unknownCategories -join ', ')")
    exit 2
}
$lines = [System.Collections.Generic.List[string]]::new()
$lines.Add('_Generated from `shared/config/settings_schema.json` by `scripts/dev/gen-settings-docs.ps1` -- do not edit this block by hand; regenerate with `pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/dev/gen-settings-docs.ps1`._')
$lines.Add('')
$lines.Add('Every runtime setting, grouped by area. **Live** settings apply on the next record start/stop; **Restart** settings (backend, model, device, compute type, hotkey) need the runtime restarted. The env var is read at startup; the same name without the `VOICEPI_` prefix, lower-cased, is the `config.json` key. For rows marked **Nullable**, JSON `null` is an explicit clear that suppresses an ambient environment value; a missing key continues to use the environment and then the schema default.')
$lines.Add('')
foreach ($category in $titles.Keys) {
    $rows = @($schema.settings | Where-Object { $_.category -eq $category })
    if ($rows.Count -eq 0) { continue }
    $basic = @($rows | Where-Object { -not $_.advanced }).Count
    $suffix = if ($basic) { (' -- {0} basic' -f $basic) } else { '' }
    $lines.Add("### $($titles[$category])$suffix")
    $lines.Add('')
    $lines.Add('| Key | Env var | Default | Config JSON | Live/Restart | Description |')
    $lines.Add('|---|---|---|---|---|---|')
    foreach ($setting in $rows) {
        $default = if ($null -eq $setting.default -or [string]::IsNullOrEmpty([string]$setting.default)) { '_(unset)_' } else { ('`{0}`' -f (Escape-Cell $setting.default)) }
        $json = if ($setting.nullable) { 'Nullable' } else { 'Value' }
        $live = if ($setting.live) { 'Live' } else { 'Restart' }
        $lines.Add(('| `{0}` | `{1}` | {2} | {3} | {4} | {5} |' -f (Escape-Cell $setting.key), (Escape-Cell $setting.env), $default, $json, $live, (Escape-Cell $setting.description)))
    }
    $lines.Add('')
}
$doc = Get-Content -LiteralPath $docsPath -Raw
$newline = if ($doc.Contains("`r`n")) { "`r`n" } else { "`n" }
$block = ($lines -join $newline).TrimEnd("`r", "`n") + $newline
$start = $doc.IndexOf($begin)
$stop = $doc.IndexOf($end)
if ($start -lt 0 -or $stop -lt 0) {
    [Console]::Error.WriteLine('settings markers not found')
    exit 2
}
if ($stop -lt $start) {
    [Console]::Error.WriteLine('END marker appears before BEGIN marker')
    exit 2
}
$updated = $doc.Substring(0, $start + $begin.Length) + $newline + $block + $doc.Substring($stop)
if ($Check) {
    if ($updated -cne $doc) {
        Write-Error 'docs/CONFIGURATION.md is out of sync with settings_schema.json.'
        exit 1
    }
    exit 0
}
if ($updated -cne $doc) {
    Set-Content -LiteralPath $docsPath -Value $updated -Encoding utf8NoBOM -NoNewline
    "updated docs/CONFIGURATION.md"
} else {
    'docs/CONFIGURATION.md already up to date'
}
