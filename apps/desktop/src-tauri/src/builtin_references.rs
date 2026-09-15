//! Reviewed, bundled reference images for a fresh local installation.
//!
//! These images are not a model corpus. They are copied into the app-data
//! Reference Bank once, so a fresh Windows install can classify without a
//! separate import step. Existing user-populated classes are preserved.

use std::collections::HashSet;

use crate::vision::{ReferenceClass, MAX_REFERENCES_PER_CLASS};

pub(crate) struct BuiltinReference {
    pub(crate) label: &'static str,
    pub(crate) class: ReferenceClass,
    pub(crate) bytes: &'static [u8],
}

pub(crate) const BUILTIN_REFERENCES: &[BuiltinReference] = &[
    BuiltinReference {
        label: "NF-OFF-001",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-001-naiwa-hero.png"
        )),
    },
    BuiltinReference {
        label: "NF-OFF-002",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-002-naiwa-belly.png"
        )),
    },
    BuiltinReference {
        label: "NF-OFF-003",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-003-naiwa-lying.png"
        )),
    },
    BuiltinReference {
        label: "NF-OFF-004",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-004-naiwa-rolling.png"
        )),
    },
    BuiltinReference {
        label: "NF-OFF-005",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-005-naiwa-wave.png"
        )),
    },
    BuiltinReference {
        label: "NF-OFF-006",
        class: ReferenceClass::NaiwaFrog,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NF-OFF-006-naiwa-logo.png"
        )),
    },
    BuiltinReference {
        label: "NL-OFF-011",
        class: ReferenceClass::Nailong,
        bytes: include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/reference-seed/NL-OFF-011-nailong-brand-example-10.png"
        )),
    },
];

fn class_name(class: ReferenceClass) -> &'static str {
    match class {
        ReferenceClass::Nailong => "NAILONG",
        ReferenceClass::NaiwaFrog => "NAIWA_FROG",
    }
}

pub(crate) fn seed_if_needed(app: &tauri::AppHandle) -> Result<usize, String> {
    let database = crate::open_database(app)?;
    if database
        .builtin_reference_bank_seeded()
        .map_err(|error| error.to_string())?
    {
        return Ok(0);
    }

    let existing = database
        .list_references()
        .map_err(|error| error.to_string())?;
    let existing_hashes: HashSet<String> = existing
        .iter()
        .map(|record| record.sha256.clone())
        .collect();
    let mut pending = Vec::new();

    for class in [ReferenceClass::Nailong, ReferenceClass::NaiwaFrog] {
        let class_label = class_name(class);
        let class_records: Vec<_> = existing
            .iter()
            .filter(|record| record.class == class_label)
            .collect();
        let candidates: Vec<_> = BUILTIN_REFERENCES
            .iter()
            .filter(|reference| reference.class == class)
            .collect();

        // An empty class is initialized from the bundled defaults. A partial
        // built-in seed can be completed after an interrupted startup. A
        // class containing unrelated user references is left untouched.
        let can_seed = class_records.is_empty()
            || class_records.iter().all(|record| {
                candidates
                    .iter()
                    .any(|reference| crate::release::sha256_hex(reference.bytes) == record.sha256)
            });
        if can_seed {
            for reference in candidates {
                let sha256 = crate::release::sha256_hex(reference.bytes);
                if !existing_hashes.contains(&sha256) {
                    pending.push(reference);
                }
            }
        }
    }
    drop(database);

    let added = add_pending(app, pending)?;

    let database = crate::open_database(app)?;
    database
        .mark_builtin_reference_bank_seeded()
        .map_err(|error| error.to_string())?;
    Ok(added)
}

/// Add missing bundled references without replacing or deleting user images.
///
/// This is intentionally separate from the one-time startup seed. Existing
/// installations may already have the seed marker from an older build while
/// still having room for the newer bundled reference bank.
pub(crate) fn restore_missing(app: &tauri::AppHandle) -> Result<usize, String> {
    let database = crate::open_database(app)?;
    let existing = database
        .list_references()
        .map_err(|error| error.to_string())?;
    let pending = pending_builtins(&existing);
    drop(database);

    add_pending(app, pending)
}

fn pending_builtins(
    existing: &[crate::storage::ReferenceRecord],
) -> Vec<&'static BuiltinReference> {
    let mut existing_hashes: HashSet<String> = existing
        .iter()
        .map(|record| record.sha256.to_ascii_lowercase())
        .collect();
    let mut pending = Vec::new();

    for class in [ReferenceClass::Nailong, ReferenceClass::NaiwaFrog] {
        let class_count = existing
            .iter()
            .filter(|record| record.class == class_name(class))
            .count();
        let available = MAX_REFERENCES_PER_CLASS.saturating_sub(class_count);
        if available == 0 {
            continue;
        }

        let mut selected = 0usize;
        for reference in BUILTIN_REFERENCES
            .iter()
            .filter(|reference| reference.class == class)
        {
            if selected >= available {
                break;
            }
            let sha256 = crate::release::sha256_hex(reference.bytes);
            if existing_hashes.insert(sha256) {
                pending.push(reference);
                selected += 1;
            }
        }
    }
    pending
}

fn add_pending(
    app: &tauri::AppHandle,
    pending: Vec<&'static BuiltinReference>,
) -> Result<usize, String> {
    let mut added = 0usize;
    for reference in pending {
        super::add_reference(
            app.clone(),
            class_name(reference.class).to_owned(),
            reference.bytes.to_vec(),
        )
        .map_err(|error| format!("内置参考图 {} 导入失败: {error}", reference.label))?;
        added += 1;
    }
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_references_cover_both_classes() {
        let nailong = BUILTIN_REFERENCES
            .iter()
            .filter(|reference| reference.class == ReferenceClass::Nailong)
            .count();
        let naiwa_frog = BUILTIN_REFERENCES
            .iter()
            .filter(|reference| reference.class == ReferenceClass::NaiwaFrog)
            .count();

        assert_eq!(nailong, 1);
        assert_eq!(naiwa_frog, 6);
        assert!(BUILTIN_REFERENCES
            .iter()
            .all(|reference| !reference.bytes.is_empty()));
    }

    #[test]
    fn restore_selection_preserves_user_references_and_respects_capacity() {
        let user_record = |class: &str, index: usize| crate::storage::ReferenceRecord {
            id: format!("USER-{class}-{index}"),
            class: class.to_owned(),
            file_path: format!("C:/references/{class}/{index}.png"),
            sha256: format!("{index:064x}"),
            phash: "0".repeat(16),
            descriptor_path: None,
            width: 1,
            height: 1,
            created_at: "test".to_owned(),
        };

        let partial = vec![user_record("NAILONG", 1), user_record("NAIWA_FROG", 2)];
        let pending = pending_builtins(&partial);
        assert_eq!(
            pending
                .iter()
                .filter(|reference| reference.class == ReferenceClass::Nailong)
                .count(),
            1
        );
        assert_eq!(
            pending
                .iter()
                .filter(|reference| reference.class == ReferenceClass::NaiwaFrog)
                .count(),
            6
        );

        let full_nailong = (0..MAX_REFERENCES_PER_CLASS)
            .map(|index| user_record("NAILONG", 100 + index))
            .collect::<Vec<_>>();
        let pending = pending_builtins(&full_nailong);
        assert!(pending
            .iter()
            .all(|reference| reference.class != ReferenceClass::Nailong));
    }

    #[cfg(feature = "opencv-backend")]
    #[test]
    fn bundled_references_classify_as_their_declared_classes() {
        use crate::{decoder, vision_opencv::OpenCvVisionEngine};

        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let mut decoded = Vec::with_capacity(BUILTIN_REFERENCES.len());
        let mut references = Vec::with_capacity(BUILTIN_REFERENCES.len());

        for bundled in BUILTIN_REFERENCES {
            let image = decoder::decode_image(bundled.bytes, 12).unwrap();
            let frame = image.frames.first().unwrap();
            references.push(
                engine
                    .extract_reference(bundled.label, bundled.class, frame)
                    .unwrap(),
            );
            decoded.push((bundled, image));
        }

        for (bundled, image) in decoded {
            let result = engine.classify_frames(&image.frames, &references).unwrap();
            let expected = match bundled.class {
                ReferenceClass::Nailong => crate::vision::ClassificationLabel::Nailong,
                ReferenceClass::NaiwaFrog => crate::vision::ClassificationLabel::NaiwaFrog,
            };
            assert_eq!(
                result.label, expected,
                "bundled reference {}",
                bundled.label
            );
        }
    }
}
