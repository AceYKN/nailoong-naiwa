[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ValidationRoot,

    [Parameter(Mandatory = $true)]
    [string]$Manifest,

    [Parameter(Mandatory = $true)]
    [string]$NailongReference,

    [Parameter(Mandatory = $true)]
    [string]$NaiwaFrogReference,

    [string]$OpenCvDir = $env:OPENCV_DIR,
    [string]$LlvmBin = $env:LIBCLANG_PATH
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $ValidationRoot -PathType Container)) {
    throw "Validation root does not exist: $ValidationRoot"
}
foreach ($file in @($Manifest, $NailongReference, $NaiwaFrogReference)) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "Required validation file does not exist: $file"
    }
}
if ([string]::IsNullOrWhiteSpace($OpenCvDir) -or [string]::IsNullOrWhiteSpace($LlvmBin)) {
    throw 'Set OPENCV_DIR and LIBCLANG_PATH, or pass -OpenCvDir and -LlvmBin.'
}

$rows = @(Get-Content -LiteralPath $Manifest -Encoding UTF8 |
    Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
    ForEach-Object { $_ | ConvertFrom-Json })
$counts = @{
    NAILONG    = @($rows | Where-Object label -eq 'NAILONG').Count
    NAIWA_FROG = @($rows | Where-Object label -eq 'NAIWA_FROG').Count
    OTHER      = @($rows | Where-Object label -eq 'OTHER').Count
}
$gifRows = @($rows | Where-Object {
    [System.IO.Path]::GetExtension($_.source_relative_path).ToLowerInvariant() -eq '.gif'
}).Count

if ($counts.NAILONG -lt 100 -or $counts.NAIWA_FROG -lt 100 -or $counts.OTHER -lt 1000 -or $gifRows -lt 50) {
    throw "Release gate manifest is below minimum counts: NAILONG=$($counts.NAILONG), NAIWA_FROG=$($counts.NAIWA_FROG), OTHER=$($counts.OTHER), GIF=$gifRows."
}

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$opencvBin = Join-Path $OpenCvDir 'x64\vc16\bin'
$opencvInclude = Join-Path $OpenCvDir 'include'
$opencvLib = Join-Path $OpenCvDir 'x64\vc16\lib'
foreach ($path in @($opencvBin, $opencvInclude, $opencvLib, $LlvmBin)) {
    if (-not (Test-Path -LiteralPath $path -PathType Container)) {
        throw "OpenCV/Clang path does not exist: $path"
    }
}

$env:OPENCV_DIR = $OpenCvDir
$env:OPENCV_INCLUDE_PATHS = $opencvInclude
$env:OPENCV_LINK_PATHS = $opencvLib
$env:OPENCV_LINK_LIBS = 'opencv_world4130'
$env:LIBCLANG_PATH = $LlvmBin
$env:Path = "$opencvBin;$LlvmBin;$env:Path"
$env:NLNF_VALIDATION_ROOT = (Resolve-Path -LiteralPath $ValidationRoot).Path
$env:NLNF_VALIDATION_MANIFEST = (Resolve-Path -LiteralPath $Manifest).Path
$env:NLNF_SMOKE_NAILONG = (Resolve-Path -LiteralPath $NailongReference).Path
$env:NLNF_SMOKE_NAIWA_FROG = (Resolve-Path -LiteralPath $NaiwaFrogReference).Path
$env:NLNF_REQUIRE_RELEASE_GATE = '1'

Write-Output "manifest_rows=$($rows.Count)"
Write-Output "nailong_rows=$($counts.NAILONG)"
Write-Output "naiwa_frog_rows=$($counts.NAIWA_FROG)"
Write-Output "other_rows=$($counts.OTHER)"
Write-Output "gif_rows=$gifRows"
Write-Output 'Running the local-only release gate; no image is uploaded.'

Push-Location $repoRoot
try {
    & cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --lib local_manifest_validation_reports_false_recall_safety -- --nocapture --test-threads=1
    $exitCode = $LASTEXITCODE
}
finally {
    Pop-Location
}
exit $exitCode
