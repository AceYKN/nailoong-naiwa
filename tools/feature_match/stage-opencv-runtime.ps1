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
$runtimeDll = Get-ChildItem -LiteralPath $runtimeDirectory -Filter "opencv_world*.dll" -File |
  Where-Object { $_.Name -notmatch 'd\.dll$' } |
  Sort-Object Name -Descending |
  Select-Object -First 1
if ($null -eq $runtimeDll) {
  throw "No release opencv_world*.dll was found in $runtimeDirectory; debug DLLs ending in d.dll are not valid for the release build."
}
if (-not (Test-Path -LiteralPath $releaseDirectory -PathType Container)) {
  throw "Release directory does not exist yet: $releaseDirectory"
}

# A previous debug build may have left opencv_world*d.dll beside the release
# executable. Remove only those generated DLLs from this exact release output;
# the release binary must resolve the non-debug import name.
Get-ChildItem -LiteralPath $releaseDirectory -Filter "opencv_world*d.dll" -File |
  Remove-Item -Force
Copy-Item -LiteralPath $runtimeDll.FullName -Destination (Join-Path $releaseDirectory $runtimeDll.Name) -Force
Write-Output "Staged $($runtimeDll.Name) beside the release executable."
