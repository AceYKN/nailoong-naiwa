param(
  [string]$RuntimeDir = $env:OPENCV_RUNTIME_DIR
)

$ErrorActionPreference = "Stop"

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
if (-not (Test-Path -LiteralPath $releaseDirectory -PathType Container)) {
  throw "Release directory does not exist yet: $releaseDirectory"
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
Copy-Item -LiteralPath $runtimeDll.FullName -Destination (Join-Path $releaseDirectory $runtimeDll.Name) -Force
Write-Output "Staged $($runtimeDll.Name) beside the release executable."
