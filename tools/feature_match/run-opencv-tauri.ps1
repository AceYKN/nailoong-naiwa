[CmdletBinding()]
param(
  [Parameter(Mandatory = $true, Position = 0)]
  [ValidateSet('dev', 'dev-e2e', 'build', 'build-recall')]
  [string]$Command
)

$ErrorActionPreference = 'Stop'
$expectedWorldName = 'opencv_world4130'

function Import-UserEnvironmentValue([string]$Name) {
  $processValue = [Environment]::GetEnvironmentVariable($Name, 'Process')
  if (-not [string]::IsNullOrWhiteSpace($processValue)) {
    return
  }
  $userValue = [Environment]::GetEnvironmentVariable($Name, 'User')
  if (-not [string]::IsNullOrWhiteSpace($userValue)) {
    [Environment]::SetEnvironmentVariable($Name, $userValue, 'Process')
  }
}

@(
  'OPENCV_DIR',
  'OPENCV_INCLUDE_PATHS',
  'OPENCV_LINK_PATHS',
  'OPENCV_WORLD_NAME',
  'OPENCV_LINK_LIBS',
  'OPENCV_RUNTIME_DIR',
  'LIBCLANG_PATH',
  'CLANG_PATH'
) | ForEach-Object { Import-UserEnvironmentValue $_ }

function Set-DefaultEnvironmentValue([string]$Name, [string]$Value) {
  if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($Name, 'Process')) -and
      -not [string]::IsNullOrWhiteSpace($Value)) {
    [Environment]::SetEnvironmentVariable($Name, $Value, 'Process')
  }
}

if (-not [string]::IsNullOrWhiteSpace($env:OPENCV_DIR)) {
  Set-DefaultEnvironmentValue 'OPENCV_INCLUDE_PATHS' (Join-Path $env:OPENCV_DIR 'include')
  Set-DefaultEnvironmentValue 'OPENCV_LINK_PATHS' (Join-Path $env:OPENCV_DIR 'x64\vc16\lib')
  Set-DefaultEnvironmentValue 'OPENCV_RUNTIME_DIR' (Join-Path $env:OPENCV_DIR 'x64\vc16\bin')
}
Set-DefaultEnvironmentValue 'OPENCV_WORLD_NAME' $expectedWorldName
Set-DefaultEnvironmentValue 'OPENCV_LINK_LIBS' $expectedWorldName

$preflight = Join-Path $PSScriptRoot 'check-opencv-env.ps1'
& pwsh -NoProfile -ExecutionPolicy Bypass -File $preflight -RequireRuntime
if ($LASTEXITCODE -ne 0) {
  exit $LASTEXITCODE
}

$clangDirectory = if ([string]::IsNullOrWhiteSpace($env:CLANG_PATH)) {
  $null
} else {
  [IO.Path]::GetDirectoryName($env:CLANG_PATH)
}
$pathEntries = @(
  $env:OPENCV_RUNTIME_DIR,
  $env:LIBCLANG_PATH,
  $clangDirectory
)
$currentPathEntries = if ([string]::IsNullOrWhiteSpace($env:Path)) {
  @()
} else {
  $env:Path -split ';'
}
$env:Path = (@($pathEntries + $currentPathEntries) |
  Where-Object { -not [string]::IsNullOrWhiteSpace($_) } |
  Select-Object -Unique) -join ';'

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$desktopRoot = Join-Path $repositoryRoot 'apps\desktop'
$exitCode = 1

Push-Location $desktopRoot
try {
  switch ($Command) {
    'dev' {
      & pnpm exec tauri dev --features opencv-backend
    }
    'dev-e2e' {
      & pnpm exec tauri dev --config src-tauri/tauri.e2e.conf.json --features opencv-backend
    }
    'build' {
      & pnpm exec tauri build --config src-tauri/tauri.opencv.conf.json --features opencv-backend --bundles nsis --no-sign --ci
    }
    'build-recall' {
      & pnpm exec tauri build --config src-tauri/tauri.opencv.conf.json --features opencv-backend,auto-recall-release --bundles nsis --no-sign --ci
    }
  }
  $exitCode = $LASTEXITCODE
}
finally {
  Pop-Location
}

exit $exitCode
