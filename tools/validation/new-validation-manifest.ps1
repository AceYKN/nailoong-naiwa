[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ValidationRoot,

    [string]$OutputManifest
)

$ErrorActionPreference = 'Stop'

$root = (Resolve-Path -LiteralPath $ValidationRoot).Path.TrimEnd('\')
if (-not $OutputManifest) {
    $OutputManifest = Join-Path $root 'manifest.jsonl'
}
$output = [System.IO.Path]::GetFullPath($OutputManifest)

$labelDirectories = [ordered]@{
    NAILONG    = 'nailong'
    NAIWA_FROG = 'naiwa_frog'
    OTHER      = 'other'
}
$extensions = @('.jpg', '.jpeg', '.png', '.webp', '.gif')
$rows = [System.Collections.Generic.List[string]]::new()
$counts = @{}

foreach ($entry in $labelDirectories.GetEnumerator()) {
    $directory = Join-Path $root $entry.Value
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
        throw "Missing validation directory: $directory"
    }

    $files = @(Get-ChildItem -LiteralPath $directory -File -Recurse |
        Where-Object { $extensions -contains $_.Extension.ToLowerInvariant() } |
        Sort-Object FullName)
    $counts[$entry.Key] = $files.Count
    foreach ($file in $files) {
        $relative = $file.FullName.Substring($root.Length + 1).Replace('\', '/')
        $row = [ordered]@{
            source_relative_path = $relative
            label                = $entry.Key
        }
        $rows.Add(($row | ConvertTo-Json -Compress))
    }
}

$outputParent = Split-Path -Parent $output
if ($outputParent -and -not (Test-Path -LiteralPath $outputParent)) {
    New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllLines($output, $rows, $utf8NoBom)

$gifCount = @($rows | ForEach-Object {
    ($_ | ConvertFrom-Json).source_relative_path
} | Where-Object { [System.IO.Path]::GetExtension($_).ToLowerInvariant() -eq '.gif' }).Count

Write-Output "manifest=$output"
Write-Output "rows=$($rows.Count)"
Write-Output "nailong=$($counts.NAILONG)"
Write-Output "naiwa_frog=$($counts.NAIWA_FROG)"
Write-Output "other=$($counts.OTHER)"
Write-Output "gif_rows=$gifCount"
Write-Output 'This manifest is validation-only; it is not model-fitting data.'
