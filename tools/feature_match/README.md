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
$env:OPENCV_LINK_LIBS = "opencv_world4130"
$env:LIBCLANG_PATH = "C:\path\to\llvm\bin"
$env:Path = "$env:OPENCV_DIR\x64\vc16\bin;$env:LIBCLANG_PATH;$env:Path"
```

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
copies the selected `opencv_world*.dll` beside the executable before NSIS
bundling; the config explicitly includes that staged DLL as a bundle resource,
and fails closed if the runtime DLL is missing. The DLL is a local build input
and is intentionally not committed to Git.
