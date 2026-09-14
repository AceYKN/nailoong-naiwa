# Windows OpenCV backend

`apps/desktop/src-tauri` keeps the native backend behind the `opencv-backend`
feature. Python `opencv-contrib-python` is useful for a quick SIFT smoke test,
but it does not provide the C++ headers/import library required by the Rust
`opencv` crate.

The build needs these user-local paths:

```powershell
$env:OPENCV_DIR = "C:\path\to\opencv\build"
$env:OPENCV_INCLUDE_PATHS = "$env:OPENCV_DIR\include"
$env:OPENCV_LINK_PATHS = "$env:OPENCV_DIR\x64\vc16\lib"
$env:OPENCV_WORLD_NAME = "opencv_world4130"
$env:OPENCV_LINK_LIBS = $env:OPENCV_WORLD_NAME
$env:LIBCLANG_PATH = "C:\path\to\llvm\bin"
# CLANG_PATH may point to clang.exe in a separate LLVM installation.
$env:CLANG_PATH = "C:\path\to\llvm\bin\clang.exe"
$env:Path = "$env:OPENCV_DIR\x64\vc16\bin;$env:LIBCLANG_PATH;$env:Path"
```

If these values were written to the Windows user environment, close the
existing PowerShell and open a new one before running the commands below.
Already-running shells keep their original environment block. `LIBCLANG_PATH`
must contain `libclang.dll`; `CLANG_PATH` must point to an actual `clang.exe`.
The `tauri:dev:opencv` and `tauri:build:opencv` package wrappers also import
missing values from the Windows user environment and add the native runtime
directories to the child process, so they can recover from an older shell.
Before either OpenCV bundle command, the wrapper stages the pinned release DLL
beside `target/release` because Tauri resolves bundle resources before it
starts the final packaging step. Manual `cargo` commands still require a
refreshed shell or explicit process variables.

Run the preflight before starting the native Tauri path:

```powershell
pnpm --dir apps/desktop tauri:doctor:opencv
```

It only checks the local headers, import library, LLVM `libclang.dll`,
`clang.exe`, and release runtime DLL; it does not install or download anything.
The OpenCV
development and bundle commands run the same check automatically, so a missing
native dependency is reported before Tauri or the Rust binding generator runs.

The ordinary Tauri development command uses `http://127.0.0.1:15420`. This is
intentional: some Windows installations reserve the conventional Tauri port
`1420`, which makes Vite fail with `EACCES` before Tauri can create a window.
The isolated E2E configuration continues to use port `5314`.

Then run from the repository root:

```powershell
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --all-targets -- -D warnings
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --lib -- --test-threads=1
```

To run the opt-in real-image smoke test without recording a machine-specific
cache path in the repository:

```powershell
$env:NLNF_SMOKE_NAILONG = "C:\path\to\one-nailong-image.png"
$env:NLNF_SMOKE_NAIWA_FROG = "C:\path\to\one-frog-image.jpg"
$env:NLNF_SMOKE_OTHER = "C:\path\to\one-negative-image.jpg" # optional
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --features opencv-backend --lib real_local_reference_smoke -- --nocapture
```

This is a smoke test, not a frozen accuracy or release gate. It checks the
actual Windows decoder plus the Rust OpenCV engine on two user-selected files.

For a development executable, the OpenCV runtime DLL directory must remain on
`PATH`. The NSIS flow below stages the release DLL beside the packaged
executable, so an installed package does not depend on the developer's PATH.
Do not commit the toolchain, QQ cache paths, descriptor cache, or any API key
to the repository.

## Windows NSIS package

After setting the variables above, run from the repository root:

```powershell
$env:OPENCV_RUNTIME_DIR = "$env:OPENCV_DIR\x64\vc16\bin"
pnpm --dir apps/desktop tauri:build:opencv
```

The OpenCV-only Tauri config runs
`stage-opencv-runtime.ps1` after the release binary is built. The script
copies the exact `$env:OPENCV_WORLD_NAME.dll` beside the executable before NSIS
bundling; the config explicitly includes the pinned `opencv_world4130.dll` as a
bundle resource, and fails closed if the name or runtime DLL does not match. If
`NLNF_OPENCV_RUNTIME_SHA256` is set (as it is for release validation and CI),
the staging step also compares the actual DLL bytes to that hash. The DLL is a
local build input and is intentionally not committed to Git.
