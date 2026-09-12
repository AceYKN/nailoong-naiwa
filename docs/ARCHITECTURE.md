# Architecture — Specification v2.0

```text
React UI
   │ Tauri command bridge
   ▼
Rust desktop core
   ├── bounded image decode and frame sampling
   ├── pHash coarse filter
   ├── OpenCV SIFT feature extraction (AKAZE fallback pending native support)
   ├── BF/FLANN + Lowe Ratio + RANSAC Homography
   ├── deterministic scoring and DecisionEngine
   ├── ReferenceManager + versioned SQLite cache
   ├── QQAdapter (Mock → Observe → guarded Auto Recall)
   └── Tauri QQ service worker (loopback OneBot event listener + moderation log)
```

## Product boundary

The reference bank defines the two visual classes. It is not a training set:

```text
references/nailong/     1..10 reference images
references/naiwa_frog/  1..10 reference images
```

The minimum MVP is one reference per class. The data model must remain plural so a new pose, crop, GIF frame or close-up can be added without retraining or rebuilding a model.

## Vision pipeline

1. Validate bytes, dimensions, pixel count and animation limits before decode.
2. Decode JPEG, PNG, WebP or GIF; resize while preserving aspect ratio and cap the working dimension.
3. Compute pHash for fast same/near-image candidates.
4. Extract SIFT descriptors. AKAZE remains a planned fallback until the Windows Rust binding/runtime combination is verified.
5. Compare query descriptors with each eligible reference using BF/FLANN KNN with `k=2`.
6. Apply Lowe ratio filtering, then RANSAC Homography.
7. Store `MatchResult` including good matches, inliers, inlier ratio, spatial coverage, reprojection error and pHash distance.
8. Compute one score per reference and use the maximum score per class.
9. Apply `T_MATCH` and `MIN_MARGIN`; ambiguous or weak evidence is `UNKNOWN`.

Color/HSV is auxiliary only. pHash alone cannot recognize a different pose. The production implementation must not use a neural model or a cloud API.

## Decision and QQ safety

The deterministic Rust decision layer is independent of OpenCV and QQ. `OTHER` means sufficient evidence that neither reference bank wins; `UNKNOWN` means insufficient or ambiguous evidence. Both are non-recall outcomes.

`AUTO_RECALL` requires all of the following:

- label is `NAIWA_FROG`;
- score reaches `T_RECALL`;
- score margin reaches `MIN_RECALL_MARGIN`;
- minimum inliers, inlier ratio and spatial coverage pass;
- reprojection error is below the configured maximum;
- confidence is `VERY_HIGH`;
- for animations, at least two sampled frames pass the strict frog gate;
- the message has not already been processed.

Adapter errors, missing permissions, image-fetch errors and incomplete geometry fail closed. The default mode is `OFF`; `OBSERVE` records `WOULD_RECALL` only.

## ReferenceManager and storage

`ReferenceManager` owns file validation, SHA-256, pHash metadata, add/delete operations and app-data copies. It enforces 1~10 references per class. With the OpenCV feature enabled, adding a reference also writes a versioned descriptor cache beside the app database; a missing or invalid cache is safely re-extracted. Adding or deleting a reference increments `reference_set_version`.

The cache key is:

```text
image_sha256 + reference_set_version
```

The SQLite schema stores logical references in `reference_images`, prediction cache, QQ groups, moderation logs and settings. Descriptor blobs may live beside the database with a path and SHA-256 recorded in the reference table. A stale cache can never survive a reference bank version change.

## UI pages

The desktop UI keeps four pages:

- `识别`: upload one or more files, show label/score/confidence and offer add-as-reference actions;
- `QQ`: connect to loopback OneBot API/reverse events, configure per-group OFF/OBSERVE/AUTO_RECALL, and review persisted moderation events;
- `参考图`: manage each class's 1~10 references; preview can be added after descriptor-backed thumbnails are wired;
- `设置`: thresholds, local storage, decoder/runtime diagnostics and developer-mode toggle.

There is no Feedback page or training loop in the v2 UI. Recognition failures can be added directly as a new reference image and take effect through the reference-bank version.

The QQ service is explicit and fail-closed. The UI must initiate the
connection; the action endpoint and reverse-event listener must both resolve
to loopback. The Token stays in the worker's memory and is omitted from the
status payload and logs. `OFF` ignores image events, `OBSERVE` records
`WOULD_RECALL` candidates without calling `delete_msg`, and `AUTO_RECALL` is
guarded by a UI confirmation plus the deterministic geometry gates. Recent
events are held in a bounded in-memory view and persisted in `moderation_log`
so they can be recovered after restart.

## Migration boundary

The v1 chain has been removed from the current checkout and is not part of the v2 runtime path:

- training/evaluation scripts and dataset builders;
- ONNX metadata/model package and Windows ML classifier wiring;
- DeepSeek upload/prelabel tools;
- labeler as a required product step.

The migration does not delete local user QQ cache images. Historical specification text may mention the removed chain only to document the replacement decision.
