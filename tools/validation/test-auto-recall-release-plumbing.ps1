[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'

function New-SyntheticCertificate([string]$GitSha, [string]$RuntimeSha256) {
    return @{
        schemaVersion = 2
        gitSha = $GitSha
        referenceSetSha256 = ('a' * 64)
        validationManifestSha256 = ('b' * 64)
        descriptorFingerprint = ('c' * 64)
        engineFingerprint = ('d' * 64)
        visionPipelineVersion = 'synthetic-release-plumbing'
        nativeRuntimeName = 'opencv_world4130'
        nativeRuntimeSha256 = $RuntimeSha256
        thresholdsSha256 = ('e' * 64)
        minRecallThreshold = 0.92
        nailongRows = 100
        naiwaFrogRows = 100
        otherRows = 1000
        gifRows = 50
        falseTargetLabel = 0
        falseRecall = 0
    } | ConvertTo-Json -Compress
}

function Import-UserEnvironmentValue([string]$Name) {
    $processValue = [Environment]::GetEnvironmentVariable($Name, 'Process')
    if (-not [string]::IsNullOrWhiteSpace($processValue)) {
        return $processValue
    }
    $userValue = [Environment]::GetEnvironmentVariable($Name, 'User')
    if (-not [string]::IsNullOrWhiteSpace($userValue)) {
        [Environment]::SetEnvironmentVariable($Name, $userValue, 'Process')
        return $userValue
    }
    return $null
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$runtimeDirectory = Import-UserEnvironmentValue 'OPENCV_RUNTIME_DIR'
$opencvDirectory = Import-UserEnvironmentValue 'OPENCV_DIR'
if ([string]::IsNullOrWhiteSpace($runtimeDirectory) -and -not [string]::IsNullOrWhiteSpace($opencvDirectory)) {
    $runtimeDirectory = Join-Path $opencvDirectory 'x64\vc16\bin'
}
if ([string]::IsNullOrWhiteSpace($runtimeDirectory)) {
    throw 'Set OPENCV_RUNTIME_DIR or OPENCV_DIR before running the release plumbing test.'
}

$runtimePath = (Resolve-Path -LiteralPath (Join-Path $runtimeDirectory 'opencv_world4130.dll')).Path
$runtimeSha256 = (Get-FileHash -LiteralPath $runtimePath -Algorithm SHA256).Hash.ToLowerInvariant()
$gitSha = (& git -C $repositoryRoot rev-parse HEAD | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($gitSha)) {
    throw 'Unable to resolve the checkout revision for the release plumbing test.'
}

$env:NLNF_VALIDATION_GIT_SHA = $gitSha
$env:NLNF_OPENCV_RUNTIME_NAME = 'opencv_world4130'
$env:NLNF_OPENCV_RUNTIME_SHA256 = $runtimeSha256
$env:NLNF_VALIDATION_CERTIFICATE_JSON = New-SyntheticCertificate $gitSha $runtimeSha256

Write-Output 'Building the synthetic auto-recall release bundle.'
Push-Location $repositoryRoot
try {
    & pnpm --dir apps/desktop tauri:build:opencv:recall
    if ($LASTEXITCODE -ne 0) {
        exit $LASTEXITCODE
    }
}
finally {
    Pop-Location
}

$releaseDirectory = Join-Path $repositoryRoot 'apps\desktop\src-tauri\target\release'
$releaseExecutable = Join-Path $releaseDirectory 'nlnf-desktop.exe'
$installer = Get-ChildItem -LiteralPath (Join-Path $releaseDirectory 'bundle\nsis') -Filter '*.exe' -File |
    Where-Object { $_.Name -notlike 'uninstall*' } |
    Select-Object -First 1
if (-not (Test-Path -LiteralPath $releaseExecutable -PathType Leaf) -or $null -eq $installer) {
    throw 'The synthetic auto-recall release plumbing did not produce the executable and NSIS installer.'
}
Write-Output "Verified synthetic release plumbing: $($installer.Name)."

$replacement = if ($runtimeSha256[0] -eq '0') { '1' } else { '0' }
$badRuntimeSha256 = $replacement + $runtimeSha256.Substring(1)
$env:NLNF_VALIDATION_CERTIFICATE_JSON = New-SyntheticCertificate $gitSha $badRuntimeSha256
Push-Location $repositoryRoot
try {
    $failureOutput = & pwsh -NoProfile -ExecutionPolicy Bypass -File tools/feature_match/run-opencv-tauri.ps1 build-recall 2>&1 | Out-String
    $failureExitCode = $LASTEXITCODE
}
finally {
    Pop-Location
}
if ($failureExitCode -eq 0) {
    throw 'A certificate with a different runtime SHA unexpectedly passed the release build check.'
}
if ($failureOutput -notmatch 'nativeRuntimeSha256 does not match the actual runtime') {
    throw "The expected runtime identity error was not reported:`n$failureOutput"
}
Write-Output 'Verified mismatched runtime bytes fail the auto-recall release build.'
