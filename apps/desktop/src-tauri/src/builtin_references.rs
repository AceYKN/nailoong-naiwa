//! Reviewed, bundled reference images for a fresh local installation.
//!
//! These images are not a training set. They are copied into the app-data
//! Reference Bank once, so a fresh Windows install can classify without a
//! separate import step. Existing user-populated classes are preserved.

use std::collections::HashSet;

use crate::vision::ReferenceClass;

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

    let mut added = 0usize;
    for reference in pending {
        super::add_reference(
            app.clone(),
            class_name(reference.class).to_owned(),
            reference.bytes.to_vec(),
        )
        .map_err(|error| format!("内置参考图 {} 初始化失败: {error}", reference.label))?;
        added += 1;
    }

    let database = crate::open_database(app)?;
    database
        .mark_builtin_reference_bank_seeded()
        .map_err(|error| error.to_string())?;
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
