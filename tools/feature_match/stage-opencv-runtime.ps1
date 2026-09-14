param(
  [string]$RuntimeDir = $env:OPENCV_RUNTIME_DIR
)

$ErrorActionPreference = "Stop"

$preflight = Join-Path $PSScriptRoot 'check-opencv-env.ps1'
& $preflight -RequireRuntime
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$desktopRoot = Join-Path $repositoryRoot "apps\desktop\src-tauri"
$targetRoot = if ($env:CARGO_TARGET_DIR) {
  (Resolve-Path $env:CARGO_TARGET_DIR).Path
} else {
  Join-Path $desktopRoot "target"
}
$releaseDirectory = Join-Path $targetRoot "release"

if ([string]::IsNullOrWhiteSpace($RuntimeDir)) {
  if (-not [string]::IsNullOrWhiteSpace($env:OPENCV_DIR)) {
    $RuntimeDir = Join-Path $env:OPENCV_DIR "x64\vc16\bin"
  } else {
    throw "Set OPENCV_RUNTIME_DIR or OPENCV_DIR before building the OpenCV bundle."
  }
}

$runtimeDirectory = (Resolve-Path $RuntimeDir).Path
$expectedWorldName = 'opencv_world4130'
$worldName = if ([string]::IsNullOrWhiteSpace($env:OPENCV_WORLD_NAME)) {
  $expectedWorldName
} else {
  $env:OPENCV_WORLD_NAME.Trim()
}
if ($worldName -ne $expectedWorldName -or $worldName -notmatch '^[A-Za-z0-9_-]+$') {
  throw "OPENCV_WORLD_NAME must be '$expectedWorldName' to match the pinned Tauri bundle resource."
}
$runtimeDll = Get-Item -LiteralPath (Join-Path $runtimeDirectory "$worldName.dll") -ErrorAction SilentlyContinue
if ($null -eq $runtimeDll -or $runtimeDll.Name -match 'd\.dll$') {
  throw "The exact release runtime $worldName.dll was not found in $runtimeDirectory; debug DLLs are not valid for the release build."
}
$runtimeSha256 = (Get-FileHash -LiteralPath $runtimeDll.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
if (-not [string]::IsNullOrWhiteSpace($env:NLNF_OPENCV_RUNTIME_NAME) -and
    $env:NLNF_OPENCV_RUNTIME_NAME.Trim() -ne $expectedWorldName) {
  throw "NLNF_OPENCV_RUNTIME_NAME must be '$expectedWorldName'."
}
if (-not [string]::IsNullOrWhiteSpace($env:NLNF_OPENCV_RUNTIME_SHA256)) {
  $expectedRuntimeSha256 = $env:NLNF_OPENCV_RUNTIME_SHA256.Trim().ToLowerInvariant()
  if ($expectedRuntimeSha256 -notmatch '^[0-9a-f]{64}$') {
    throw 'NLNF_OPENCV_RUNTIME_SHA256 must be a 64-character SHA-256 value.'
  }
  if ($runtimeSha256 -ne $expectedRuntimeSha256) {
    throw "OpenCV runtime SHA-256 mismatch: expected $expectedRuntimeSha256, got $runtimeSha256"
  }
}
if (-not (Test-Path -LiteralPath $releaseDirectory -PathType Container)) {
  New-Item -ItemType Directory -Path $releaseDirectory -Force | Out-Null
}
$bundleConfig = Join-Path $desktopRoot 'tauri.opencv.conf.json'
$bundleConfigText = Get-Content -LiteralPath $bundleConfig -Raw
if ($bundleConfigText -notmatch [regex]::Escape("target/release/$worldName.dll")) {
  throw "The Tauri OpenCV bundle resource does not match $worldName.dll."
}

# A previous debug build may have left opencv_world*d.dll beside the release
# executable. Remove only those generated DLLs from this exact release output;
# the release binary must resolve the non-debug import name.
Get-ChildItem -LiteralPath $releaseDirectory -Filter "opencv_world*d.dll" -File |
  Remove-Item -Force
$releaseRuntimePath = [System.IO.Path]::GetFullPath((Join-Path $releaseDirectory $runtimeDll.Name))
if (-not [System.StringComparer]::OrdinalIgnoreCase.Equals($runtimeDll.FullName, $releaseRuntimePath)) {
  Copy-Item -LiteralPath $runtimeDll.FullName -Destination $releaseRuntimePath -Force
}
Write-Output "Staged $($runtimeDll.Name) beside the release executable (SHA-256 $runtimeSha256)."
