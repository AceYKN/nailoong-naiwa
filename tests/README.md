# Verification tests

Specification v2 uses a native Rust/OpenCV vision engine and has no Python
preprocessing, ONNX inference, training fixture, or cloud prelabeling step.
Cross-component verification therefore covers the Rust decoder and matcher,
the Tauri command bridge, the reference bank, and the fail-closed QQ adapter.

The release evidence and its current limitations are recorded in
`docs/ACCEPTANCE_MATRIX.md`; the test corpus must remain separate from the
reference images shipped with the application.
