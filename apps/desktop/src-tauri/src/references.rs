//! Local Reference Bank management.
//!
//! Reference images are copied into the app-data directory, never moved from
//! the user's QQ cache. Metadata is returned to the SQLite boundary by the
//! caller; descriptor precomputation is supplied by the feature-gated OpenCV
//! engine after the bytes pass this manager's input checks.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::{
    decoder,
    image_policy::{self, ImageFormat},
    phash,
    vision::{ReferenceClass, MAX_REFERENCES_PER_CLASS},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceAsset {
    pub id: String,
    pub class: ReferenceClass,
    pub file_path: PathBuf,
    pub sha256: String,
    pub phash: String,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
}

pub struct ReferenceManager {
    root: PathBuf,
}

impl ReferenceManager {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn class_directory(&self, class: ReferenceClass) -> PathBuf {
        self.root.join(class_directory_name(class))
    }

    pub fn count(&self, class: ReferenceClass) -> Result<usize, String> {
        let directory = self.class_directory(class);
        if !directory.exists() {
            return Ok(0);
        }
        let mut count = 0usize;
        for entry in std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read reference directory: {error}"))?
        {
            let entry = entry.map_err(|error| format!("cannot read reference entry: {error}"))?;
            if entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
                && is_supported_extension(entry.path().as_path())
            {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| "reference count overflow".to_owned())?;
            }
        }
        Ok(count)
    }

    /// Validate, hash and copy one user-selected image into the Reference
    /// Bank. The source bytes are never altered and the write is atomic.
    pub fn add(&self, class: ReferenceClass, bytes: &[u8]) -> Result<ReferenceAsset, String> {
        let current_count = self.count(class)?;
        if current_count >= MAX_REFERENCES_PER_CLASS {
            return Err(format!(
                "{} 参考图已达到上限 {} 张",
                class_display_name(class),
                MAX_REFERENCES_PER_CLASS
            ));
        }

        let inspection = image_policy::inspect_image(bytes).map_err(|error| error.to_string())?;
        // Use the same endpoint-inclusive sampling policy as query images.
        // WIC can expose a different composed GIF frame when asked for a
        // one-frame decode, which would make an identical animated reference
        // miss the pHash shortcut during normal multi-frame classification.
        let decoded = decoder::decode_image(bytes, image_policy::MAX_SAMPLE_FRAMES)?;
        let frame = decoded
            .frames
            .first()
            .ok_or_else(|| "reference image did not produce a decoded frame".to_owned())?;
        let phash = phash::compute_rgb(&frame.rgb, frame.width, frame.height)?;
        let id = format!("{}-{}", class_prefix(class), &inspection.sha256[..12]);
        let directory = self.class_directory(class);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("cannot create reference directory: {error}"))?;
        let destination = directory.join(format!("{}.{}", id, extension(inspection.format)));
        if destination.exists() {
            return Err("该参考图已经存在于 Reference Bank".to_owned());
        }
        write_reference_image(&directory, &destination, bytes)?;
        Ok(ReferenceAsset {
            id,
            class,
            file_path: destination,
            sha256: inspection.sha256,
            phash: format!("{phash:016x}"),
            width: frame.width,
            height: frame.height,
            format: inspection.format,
        })
    }
}

/// Persist a reference without ever moving the user-selected source file.
///
/// The temporary file is deliberately created in the destination directory.
/// That keeps the normal path atomic on Windows, including when the selected
/// source image lives on another drive. A few Windows filesystem providers
/// still report `ERROR_NOT_SAME_DEVICE` for a same-directory rename; in that
/// case the bytes are copied to the destination and verified before the
/// temporary file is removed.
fn write_reference_image(directory: &Path, destination: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let filename = destination
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "reference destination has no valid filename".to_owned())?;
    let mut temporary = directory.join(format!(".{filename}.{}.tmp", std::process::id()));
    for attempt in 0..100u32 {
        if !temporary.exists() {
            break;
        }
        temporary = directory.join(format!(".{filename}.{}-{attempt}.tmp", std::process::id()));
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("cannot write reference image: {error}"))?;
    if let Err(error) = file.write_all(bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot write reference image: {error}"));
    }
    if let Err(error) = file.sync_all() {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot flush reference image: {error}"));
    }
    drop(file);

    match std::fs::rename(&temporary, destination) {
        Ok(()) => Ok(()),
        Err(error) if matches!(error.raw_os_error(), Some(17 | 18)) => {
            let copy_result = std::fs::copy(&temporary, destination);
            match copy_result {
                Ok(copied) if copied == bytes.len() as u64 => {
                    let _ = std::fs::remove_file(&temporary);
                    Ok(())
                }
                Ok(copied) => {
                    let _ = std::fs::remove_file(&temporary);
                    let _ = std::fs::remove_file(destination);
                    Err(format!(
                        "cannot verify copied reference image: expected {} bytes, got {copied}",
                        bytes.len()
                    ))
                }
                Err(copy_error) => {
                    let _ = std::fs::remove_file(&temporary);
                    Err(format!(
                        "cannot finalize reference image after cross-device rename ({error}): {copy_error}"
                    ))
                }
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(format!("cannot finalize reference image: {error}"))
        }
    }
}

fn class_directory_name(class: ReferenceClass) -> &'static str {
    match class {
        ReferenceClass::Nailong => "nailong",
        ReferenceClass::NaiwaFrog => "naiwa_frog",
    }
}

fn class_prefix(class: ReferenceClass) -> &'static str {
    match class {
        ReferenceClass::Nailong => "NL",
        ReferenceClass::NaiwaFrog => "NF",
    }
}

fn class_display_name(class: ReferenceClass) -> &'static str {
    match class {
        ReferenceClass::Nailong => "奶龙",
        ReferenceClass::NaiwaFrog => "奶蛙",
    }
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
    }
}

fn is_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{ReferenceClass, ReferenceManager};

    fn png_bytes() -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut encoder = png::Encoder::new(Cursor::new(&mut output), 8, 8);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[220_u8, 180, 80].repeat(64))
                .unwrap();
        }
        output
    }

    #[test]
    fn adds_reference_without_mutating_source_bytes() {
        let root = std::env::temp_dir().join(format!("nlnf-reference-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let source = png_bytes();
        let original = source.clone();
        let manager = ReferenceManager::new(&root);
        let asset = manager.add(ReferenceClass::Nailong, &source).unwrap();
        assert_eq!(source, original);
        assert_eq!(manager.count(ReferenceClass::Nailong).unwrap(), 1);
        assert!(asset.file_path.is_file());
        assert_eq!(manager.count(ReferenceClass::NaiwaFrog).unwrap(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn enforces_ten_reference_limit_from_directory_contents() {
        let root =
            std::env::temp_dir().join(format!("nlnf-reference-limit-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let manager = ReferenceManager::new(&root);
        let directory = manager.class_directory(ReferenceClass::NaiwaFrog);
        std::fs::create_dir_all(&directory).unwrap();
        for index in 0..10 {
            std::fs::write(directory.join(format!("NF{index:02}.png")), b"placeholder").unwrap();
        }
        let result = manager.add(ReferenceClass::NaiwaFrog, &png_bytes());
        assert!(result.unwrap_err().contains("上限"));
        let _ = std::fs::remove_dir_all(root);
    }
}
