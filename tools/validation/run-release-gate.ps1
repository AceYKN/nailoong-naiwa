[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ValidationRoot,

    [Parameter(Mandatory = $true)]
    [string]$Manifest,

    [Parameter(Mandatory = $true)]
    [string[]]$NailongReference,

    [Parameter(Mandatory = $true)]
    [string[]]$NaiwaFrogReference,

    [string]$OpenCvDir = $env:OPENCV_DIR,
    [string]$LlvmBin = $env:LIBCLANG_PATH
)

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..\..')).Path
$gitStatus = (& git -C $repoRoot status --porcelain | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    throw 'Unable to read the repository status before the release gate.'
}
if (-not [string]::IsNullOrWhiteSpace($gitStatus)) {
    throw 'The release gate requires a clean checkout so the certificate binds the tested source revision.'
}
$gitSha = (& git -C $repoRoot rev-parse HEAD | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($gitSha)) {
    throw 'Unable to resolve the Git revision for the validation certificate.'
}

if (-not (Test-Path -LiteralPath $ValidationRoot -PathType Container)) {
    throw "Validation root does not exist: $ValidationRoot"
}
$nailongReferences = @($NailongReference | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
$naiwaFrogReferences = @($NaiwaFrogReference | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
if ($nailongReferences.Count -lt 1 -or $nailongReferences.Count -gt 10) {
    throw "Provide 1 to 10 Nailong reference images; received $($nailongReferences.Count)."
}
if ($naiwaFrogReferences.Count -lt 1 -or $naiwaFrogReferences.Count -gt 10) {
    throw "Provide 1 to 10 Naiwa Frog reference images; received $($naiwaFrogReferences.Count)."
}
$manifestPath = (Resolve-Path -LiteralPath $Manifest -ErrorAction Stop).Path
$resolvedNailongReferences = @($nailongReferences | ForEach-Object {
    (Resolve-Path -LiteralPath $_ -ErrorAction Stop).Path
})
$resolvedNaiwaFrogReferences = @($naiwaFrogReferences | ForEach-Object {
    (Resolve-Path -LiteralPath $_ -ErrorAction Stop).Path
})
foreach ($file in @($manifestPath) + $resolvedNailongReferences + $resolvedNaiwaFrogReferences) {
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

$opencvBin = Join-Path $OpenCvDir 'x64\vc16\bin'
$opencvInclude = Join-Path $OpenCvDir 'include'
$opencvLib = Join-Path $OpenCvDir 'x64\vc16\lib'
foreach ($path in @($opencvBin, $opencvInclude, $opencvLib, $LlvmBin)) {
    if (-not (Test-Path -LiteralPath $path -PathType Container)) {
        throw "OpenCV/Clang path does not exist: $path"
    }
}
$clangPath = Join-Path $LlvmBin 'clang.exe'
if (-not (Test-Path -LiteralPath $clangPath -PathType Leaf)) {
    throw "LLVM clang binary was not found: $clangPath"
}
$runtimeName = 'opencv_world4130'
$runtimeDll = Join-Path $opencvBin "$runtimeName.dll"
if (-not (Test-Path -LiteralPath $runtimeDll -PathType Leaf)) {
    throw "The pinned OpenCV runtime DLL was not found: $runtimeDll"
}
$runtimeSha256 = (Get-FileHash -LiteralPath $runtimeDll -Algorithm SHA256).Hash.ToLowerInvariant()

$env:OPENCV_DIR = $OpenCvDir
$env:OPENCV_INCLUDE_PATHS = $opencvInclude
$env:OPENCV_LINK_PATHS = $opencvLib
$env:OPENCV_WORLD_NAME = 'opencv_world4130'
$env:OPENCV_LINK_LIBS = $env:OPENCV_WORLD_NAME
$env:LIBCLANG_PATH = $LlvmBin
$env:CLANG_PATH = $clangPath
$env:Path = "$opencvBin;$LlvmBin;$env:Path"
$env:NLNF_VALIDATION_ROOT = (Resolve-Path -LiteralPath $ValidationRoot).Path
$env:NLNF_VALIDATION_MANIFEST = $manifestPath
$env:NLNF_SMOKE_NAILONG_REFERENCES = [string]::Join(';', $resolvedNailongReferences)
$env:NLNF_SMOKE_NAIWA_FROG_REFERENCES = [string]::Join(';', $resolvedNaiwaFrogReferences)
# Keep the old single-reference variables for callers that still inspect them.
$env:NLNF_SMOKE_NAILONG = $resolvedNailongReferences[0]
$env:NLNF_SMOKE_NAIWA_FROG = $resolvedNaiwaFrogReferences[0]
$env:NLNF_REQUIRE_RELEASE_GATE = '1'
$env:NLNF_VALIDATION_GIT_SHA = $gitSha
$env:NLNF_OPENCV_RUNTIME_NAME = $runtimeName
$env:NLNF_OPENCV_RUNTIME_SHA256 = $runtimeSha256
$env:NLNF_VALIDATION_CERTIFICATE_OUTPUT = Join-Path (Resolve-Path -LiteralPath $ValidationRoot).Path 'validation-certificate.json'

Write-Output "manifest_rows=$($rows.Count)"
Write-Output "nailong_rows=$($counts.NAILONG)"
Write-Output "naiwa_frog_rows=$($counts.NAIWA_FROG)"
Write-Output "other_rows=$($counts.OTHER)"
Write-Output "gif_rows=$gifRows"
Write-Output "nailong_references=$($resolvedNailongReferences.Count)"
Write-Output "naiwa_frog_references=$($resolvedNaiwaFrogReferences.Count)"
Write-Output "opencv_runtime=$runtimeName.dll"
Write-Output "opencv_runtime_sha256=$runtimeSha256"
Write-Output 'Running the local-only release gate; no image is uploaded.'

Push-Location $repoRoot
try {
    & cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --lib local_manifest_validation_reports_false_recall_safety -- --nocapture --test-threads=1
    $exitCode = $LASTEXITCODE
}
finally {
    Pop-Location
}
if ($exitCode -ne 0) {
    exit $exitCode
}
if (-not (Test-Path -LiteralPath $env:NLNF_VALIDATION_CERTIFICATE_OUTPUT -PathType Leaf)) {
    throw "The release gate passed without producing its certificate: $($env:NLNF_VALIDATION_CERTIFICATE_OUTPUT)"
}
Write-Output "validation_certificate=$($env:NLNF_VALIDATION_CERTIFICATE_OUTPUT)"
Write-Output 'The certificate is bound to this clean Git revision, full Reference Bank, manifest, thresholds, vision fingerprints and exact OpenCV runtime bytes.'
exit 0
