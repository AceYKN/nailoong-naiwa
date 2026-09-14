[CmdletBinding()]
param(
  [switch]$RequireRuntime
)

$ErrorActionPreference = "Stop"

$expectedWorldName = 'opencv_world4130'
$failures = @()

function Add-Failure([string]$Message) {
  $script:failures += $Message
}

function Get-ExistingDirectory([string]$Path, [string]$Label) {
  if ([string]::IsNullOrWhiteSpace($Path)) {
    Add-Failure "$Label is not set."
    return $null
  }
  if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
    Add-Failure "$Label does not exist: $Path"
    return $null
  }
  return (Get-Item -LiteralPath $Path).FullName
}

function Require-File([string]$Path, [string]$Label) {
  if ([string]::IsNullOrWhiteSpace($Path) -or
      -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    Add-Failure "$Label was not found: $Path"
  }
}

$opencvDir = Get-ExistingDirectory $env:OPENCV_DIR 'OPENCV_DIR'
$includeDir = if ([string]::IsNullOrWhiteSpace($env:OPENCV_INCLUDE_PATHS)) {
  if ($null -eq $opencvDir) { $null } else { Join-Path $opencvDir 'include' }
} else {
  $env:OPENCV_INCLUDE_PATHS
}
$linkDir = if ([string]::IsNullOrWhiteSpace($env:OPENCV_LINK_PATHS)) {
  if ($null -eq $opencvDir) { $null } else { Join-Path $opencvDir 'x64\vc16\lib' }
} else {
  $env:OPENCV_LINK_PATHS
}
$includeDir = Get-ExistingDirectory $includeDir 'OPENCV_INCLUDE_PATHS'
$linkDir = Get-ExistingDirectory $linkDir 'OPENCV_LINK_PATHS'

$worldName = if ([string]::IsNullOrWhiteSpace($env:OPENCV_WORLD_NAME)) {
  $expectedWorldName
} else {
  $env:OPENCV_WORLD_NAME.Trim()
}
if ($worldName -ne $expectedWorldName) {
  Add-Failure "OPENCV_WORLD_NAME must be '$expectedWorldName' (got '$worldName')."
}
$linkLibraries = if ([string]::IsNullOrWhiteSpace($env:OPENCV_LINK_LIBS)) {
  $expectedWorldName
} else {
  $env:OPENCV_LINK_LIBS.Trim()
}
if ($linkLibraries -notmatch "(^|[;\s])$expectedWorldName([;\s]|$)") {
  Add-Failure "OPENCV_LINK_LIBS must include '$expectedWorldName' (got '$linkLibraries')."
}

$headerFile = $null
if ($null -ne $includeDir) {
  $headerFile = Join-Path $includeDir 'opencv2\core.hpp'
}
$importLibrary = $null
if ($null -ne $linkDir) {
  $importLibrary = Join-Path $linkDir "$expectedWorldName.lib"
}
Require-File $headerFile 'OpenCV C++ headers'
Require-File $importLibrary 'OpenCV import library'

$libclangDir = Get-ExistingDirectory $env:LIBCLANG_PATH 'LIBCLANG_PATH'
$libclangFile = $null
if ($null -ne $libclangDir) {
  $libclangFile = Join-Path $libclangDir 'libclang.dll'
}
Require-File $libclangFile 'LLVM libclang runtime'
if (-not [string]::IsNullOrWhiteSpace($env:CLANG_PATH)) {
  Require-File $env:CLANG_PATH 'CLANG_PATH'
}

if ($RequireRuntime) {
  $runtimeDir = $env:OPENCV_RUNTIME_DIR
  if ([string]::IsNullOrWhiteSpace($runtimeDir) -and $null -ne $opencvDir) {
    $runtimeDir = Join-Path $opencvDir 'x64\vc16\bin'
  }
  $runtimeDir = Get-ExistingDirectory $runtimeDir 'OPENCV_RUNTIME_DIR'
  $runtimeDll = $null
  if ($null -ne $runtimeDir) {
    $runtimeDll = Join-Path $runtimeDir "$expectedWorldName.dll"
  }
  Require-File $runtimeDll 'OpenCV release runtime DLL'
  if ($null -ne $runtimeDll -and (Test-Path -LiteralPath $runtimeDll -PathType Leaf)) {
    if ([IO.Path]::GetFileNameWithoutExtension($runtimeDll) -match 'd$') {
      Add-Failure "Debug OpenCV runtime is not valid for this build: $runtimeDll"
    }
  }
}

if ($failures.Count -gt 0) {
  Write-Host 'NLNF OpenCV/Tauri preflight failed. This is a native dependency issue before Tauri starts.'
  Write-Host 'Python opencv wheels do not provide the C++ headers/import library and LLVM libclang required by the Rust opencv crate.'
  Write-Host ''
  $failures | ForEach-Object { Write-Host "- $_" }
  Write-Host ''
  Write-Host 'See tools/feature_match/README.md for the user-local OpenCV 4.13.0 and LLVM 20.1.8 paths.'
  exit 1
}

Write-Output "NLNF OpenCV/Tauri preflight passed."
Write-Output "  OpenCV headers: $includeDir"
Write-Output "  OpenCV import library: $(Join-Path $linkDir "$expectedWorldName.lib")"
Write-Output "  LLVM libclang: $(Join-Path $libclangDir 'libclang.dll')"
if ($RequireRuntime) {
  Write-Output "  OpenCV runtime: $(Join-Path $runtimeDir "$expectedWorldName.dll")"
}
