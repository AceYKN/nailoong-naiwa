# Frozen validation gate

This directory contains the release-gate runner for Specification v2. It is
validation infrastructure only. It does not train a model, upload images, or
belong to the runtime classification path.

Keep the validation corpus outside the repository. Use this layout on a local
drive:

```text
validation-root/
├── nailong/
├── naiwa_frog/
└── other/
```

Put only truth-reviewed validation material in those folders. The manifest is
JSONL and contains only a relative path and one of `NAILONG`, `NAIWA_FROG`, or
`OTHER`:

```json
{"source_relative_path":"nailong/example.png","label":"NAILONG"}
```

Generate it without copying the images:

```powershell
powershell -ExecutionPolicy Bypass -File tools/validation/new-validation-manifest.ps1 `
  -ValidationRoot D:\path\to\validation-root
```

The release gate requires at least 100 Nailong rows, 100 Naiwa Frog rows,
1,000 `OTHER` rows, and 50 files whose decoded format is GIF. It also requires
every manifest path to exist, every row to process, both target classes to be
classified correctly, neither target class to be confused with the other, no
`OTHER` row to become a target, and zero strict `AUTO_RECALL` candidates among
non-frog rows. The Rust test additionally checks the actual decoded format, so
a `.gif` filename alone is not enough.

Run it after configuring the user-local OpenCV and Clang paths described in
`tools/feature_match/README.md`:

```powershell
powershell -ExecutionPolicy Bypass -File tools/validation/run-release-gate.ps1 `
  -ValidationRoot D:\path\to\validation-root `
  -Manifest D:\path\to\validation-root\manifest.jsonl `
  -NailongReference D:\path\to\nailong-reference-1.gif,D:\path\to\nailong-reference-2.png `
  -NaiwaFrogReference D:\path\to\frog-reference-1.gif,D:\path\to\frog-reference-2.png `
  -OpenCvDir C:\path\to\opencv\build `
  -LlvmBin C:\path\to\llvm\bin
```

When `-LlvmBin` is omitted, the runner derives the LLVM bin directory from
`CLANG_PATH` (the parent of `clang.exe`). It only falls back to
`LIBCLANG_PATH` when that directory also contains `clang.exe`; a directory
that contains only `libclang.dll` is not sufficient for the validation gate.

`-NailongReference` and `-NaiwaFrogReference` each accept 1~10 paths. A
single path is valid for the One-Shot MVP; comma-separated paths form the full
class Reference Bank used by the certificate. Every supplied path must exist.

Do not use unreviewed QQ-cache files as truth labels. A large cache count is
not evidence of 100/100/1000 correct samples; the labels must be independently
reviewed before enabling the gate. The runner requires a clean Git checkout and
writes `validation-certificate.json` beside the manifest. That certificate
binds the tested Git revision, the exact bytes of every supplied Reference
Bank image, the manifest bytes, descriptor/engine fingerprints, the exact
OpenCV runtime DLL name and SHA-256, the decision thresholds, the sample
counts and the zero-false-recall result. Until this command passes on a frozen
corpus, `AUTO_RECALL` remains
disabled by the application policy.

The runner validates the corpus with the OpenCV backend but does not enable the
runtime release feature. Only after it exits successfully may a release build
explicitly add `auto-recall-release` alongside `opencv-backend`. The release
build must embed the generated certificate and use the same complete Reference
Bank as the validation gate:

```powershell
$env:NLNF_VALIDATION_CERTIFICATE_JSON = [System.IO.File]::ReadAllText(
  'D:\path\to\validation-root\validation-certificate.json'
)
pnpm --dir apps/desktop tauri:build:opencv:recall
```

Adding, deleting or changing a reference later invalidates the certificate and
automatically downgrades the stored group mode to `OBSERVE`; rerun the gate for
the new exact Reference Bank. Ordinary builds must omit the release feature and
remain unable to activate `AUTO_RECALL`.
