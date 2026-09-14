use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_FILE_SIZE_BYTES: usize = 25 * 1024 * 1024;
pub const MAX_WIDTH: u32 = 8192;
pub const MAX_HEIGHT: u32 = 8192;
pub const MAX_PIXELS: u64 = 50_000_000;
pub const MAX_DECODE_FRAMES: u32 = 500;
pub const MAX_SAMPLE_FRAMES: u32 = 12;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
    Gif,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImageInspection {
    pub bytes: usize,
    pub sha256: String,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    pub animated: bool,
    pub frame_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePolicyError(String);

impl std::fmt::Display for ImagePolicyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ImagePolicyError {}

impl From<&str> for ImagePolicyError {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ImagePolicyError {
    fn from(value: String) -> Self {
        Self(value)
    }
}

pub fn inspect_image(bytes: &[u8]) -> Result<ImageInspection, ImagePolicyError> {
    if bytes.is_empty() {
        return Err("image is empty".into());
    }
    if bytes.len() > MAX_FILE_SIZE_BYTES {
        return Err(format!(
            "image exceeds the {} MiB file limit",
            MAX_FILE_SIZE_BYTES / (1024 * 1024)
        )
        .into());
    }

    let mut inspection = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        parse_png(bytes)?
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        parse_gif(bytes)?
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        parse_jpeg(bytes)?
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        parse_webp(bytes)?
    } else {
        return Err("unsupported image format; expected PNG, JPEG, WebP, or GIF".into());
    };

    validate_dimensions(inspection.width, inspection.height)?;
    if inspection.frame_count == 0 || inspection.frame_count > MAX_DECODE_FRAMES {
        return Err(format!(
            "image frame count must be between 1 and {}",
            MAX_DECODE_FRAMES
        )
        .into());
    }
    inspection.sha256 = hex::encode(Sha256::digest(bytes));
    Ok(inspection)
}

fn validate_dimensions(width: u32, height: u32) -> Result<(), ImagePolicyError> {
    if width == 0 || height == 0 {
        return Err("image dimensions must be non-zero".into());
    }
    if width > MAX_WIDTH || height > MAX_HEIGHT {
        return Err(format!(
            "image dimensions {}x{} exceed the {}x{} limit",
            width, height, MAX_WIDTH, MAX_HEIGHT
        )
        .into());
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(format!("image has more than {} decoded pixels", MAX_PIXELS).into());
    }
    Ok(())
}

/// Apply the same bounds to dimensions reported by a decoder, not only to
/// dimensions read from the container header. A platform decoder must not be
/// allowed to turn a small compressed payload into an unexpectedly large
/// frame after the header policy has already passed.
pub(crate) fn validate_decoded_dimensions(width: u32, height: u32) -> Result<(), String> {
    validate_dimensions(width, height).map_err(|error| error.to_string())
}

fn parse_png(bytes: &[u8]) -> Result<ImageInspection, ImagePolicyError> {
    if bytes.len() < 33 {
        return Err("truncated PNG header".into());
    }
    let ihdr_length = be_u32(&bytes[8..12])?;
    if ihdr_length < 13 || &bytes[12..16] != b"IHDR" {
        return Err("PNG is missing a valid IHDR chunk".into());
    }
    let width = be_u32(&bytes[16..20])?;
    let height = be_u32(&bytes[20..24])?;
    let mut cursor = 8usize;
    let mut frame_count = 1u32;
    let mut animated = false;
    while cursor < bytes.len() {
        let length_end = cursor
            .checked_add(4)
            .ok_or_else(|| ImagePolicyError::from("PNG chunk length overflow"))?;
        let type_end = length_end
            .checked_add(4)
            .ok_or_else(|| ImagePolicyError::from("PNG chunk type overflow"))?;
        let data_end = type_end
            .checked_add(be_u32(
                bytes
                    .get(cursor..length_end)
                    .ok_or_else(|| ImagePolicyError::from("truncated PNG chunk length"))?,
            )? as usize)
            .ok_or_else(|| ImagePolicyError::from("PNG chunk data overflow"))?;
        let chunk_end = data_end
            .checked_add(4)
            .ok_or_else(|| ImagePolicyError::from("PNG chunk CRC overflow"))?;
        if chunk_end > bytes.len() {
            return Err("truncated PNG chunk".into());
        }
        let chunk_type = &bytes[length_end..type_end];
        let chunk_data = &bytes[type_end..data_end];
        if chunk_type == b"acTL" {
            if chunk_data.len() < 4 {
                return Err("truncated APNG animation control".into());
            }
            animated = true;
            frame_count = be_u32(&chunk_data[0..4])?;
        } else if chunk_type == b"fcTL" {
            animated = true;
            frame_count = frame_count.max(1);
        } else if chunk_type == b"IEND" {
            break;
        }
        cursor = chunk_end;
    }
    Ok(ImageInspection {
        bytes: bytes.len(),
        sha256: String::new(),
        format: ImageFormat::Png,
        width,
        height,
        animated,
        frame_count,
    })
}

fn parse_gif(bytes: &[u8]) -> Result<ImageInspection, ImagePolicyError> {
    if bytes.len() < 13 {
        return Err("truncated GIF header".into());
    }
    let width = le_u16(&bytes[6..8])? as u32;
    let height = le_u16(&bytes[8..10])? as u32;
    let packed = bytes[10];
    let mut cursor = 13usize;
    if packed & 0x80 != 0 {
        cursor = cursor
            .checked_add(color_table_bytes(packed & 0x07)?)
            .ok_or_else(|| ImagePolicyError::from("GIF color table overflow"))?;
    }
    let mut frame_count = 0u32;
    loop {
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| ImagePolicyError::from("GIF ended before its trailer"))?;
        cursor += 1;
        match marker {
            0x3b => break,
            0x2c => {
                let descriptor_end = cursor
                    .checked_add(9)
                    .ok_or_else(|| ImagePolicyError::from("GIF image descriptor overflow"))?;
                let descriptor = bytes
                    .get(cursor..descriptor_end)
                    .ok_or_else(|| ImagePolicyError::from("truncated GIF image descriptor"))?;
                let frame_width = le_u16(&descriptor[4..6])? as u32;
                let frame_height = le_u16(&descriptor[6..8])? as u32;
                validate_dimensions(frame_width, frame_height)?;
                cursor = descriptor_end;
                let image_packed = descriptor[8];
                if image_packed & 0x80 != 0 {
                    cursor = cursor
                        .checked_add(color_table_bytes(image_packed & 0x07)?)
                        .ok_or_else(|| ImagePolicyError::from("GIF local color table overflow"))?;
                    if cursor > bytes.len() {
                        return Err("truncated GIF local color table".into());
                    }
                }
                cursor = bytes
                    .get(cursor..)
                    .and_then(|remaining| remaining.first())
                    .map(|_| cursor + 1)
                    .ok_or_else(|| ImagePolicyError::from("GIF is missing LZW code size"))?;
                cursor = skip_sub_blocks(bytes, cursor)?;
                frame_count = frame_count
                    .checked_add(1)
                    .ok_or_else(|| ImagePolicyError::from("GIF frame count overflow"))?;
                if frame_count > MAX_DECODE_FRAMES {
                    return Err(format!("GIF has more than {} frames", MAX_DECODE_FRAMES).into());
                }
            }
            0x21 => {
                if bytes.get(cursor).is_none() {
                    return Err("GIF extension has no label".into());
                }
                cursor += 1;
                cursor = skip_sub_blocks(bytes, cursor)?;
            }
            _ => return Err(format!("unknown GIF block marker 0x{marker:02x}").into()),
        }
    }
    Ok(ImageInspection {
        bytes: bytes.len(),
        sha256: String::new(),
        format: ImageFormat::Gif,
        width,
        height,
        animated: frame_count > 1,
        frame_count,
    })
}

fn parse_jpeg(bytes: &[u8]) -> Result<ImageInspection, ImagePolicyError> {
    let mut cursor = 2usize;
    while cursor < bytes.len() {
        while bytes.get(cursor) == Some(&0xff) {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| ImagePolicyError::from("truncated JPEG marker"))?;
        cursor += 1;
        if marker == 0xd9 {
            break;
        }
        if marker == 0xda {
            break;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        let length_end = cursor
            .checked_add(2)
            .ok_or_else(|| ImagePolicyError::from("JPEG segment length overflow"))?;
        let length =
            usize::from(be_u16(bytes.get(cursor..length_end).ok_or_else(|| {
                ImagePolicyError::from("truncated JPEG segment length")
            })?)?);
        if length < 2 {
            return Err("JPEG segment has an invalid length".into());
        }
        let segment_end = cursor
            .checked_add(length)
            .ok_or_else(|| ImagePolicyError::from("JPEG segment overflow"))?;
        let segment = bytes
            .get(cursor..segment_end)
            .ok_or_else(|| ImagePolicyError::from("truncated JPEG segment"))?;
        if is_jpeg_sof(marker) {
            if segment.len() < 7 {
                return Err("truncated JPEG frame header".into());
            }
            let height = u32::from(be_u16(&segment[3..5])?);
            let width = u32::from(be_u16(&segment[5..7])?);
            return Ok(ImageInspection {
                bytes: bytes.len(),
                sha256: String::new(),
                format: ImageFormat::Jpeg,
                width,
                height,
                animated: false,
                frame_count: 1,
            });
        }
        cursor = segment_end;
    }
    Err("JPEG contains no frame dimensions".into())
}

fn parse_webp(bytes: &[u8]) -> Result<ImageInspection, ImagePolicyError> {
    let mut cursor = 12usize;
    let mut width = None;
    let mut height = None;
    let mut animated = false;
    let mut frame_count = 0u32;
    while cursor < bytes.len() {
        let header_end = cursor
            .checked_add(8)
            .ok_or_else(|| ImagePolicyError::from("WebP chunk header overflow"))?;
        let header = bytes
            .get(cursor..header_end)
            .ok_or_else(|| ImagePolicyError::from("truncated WebP chunk header"))?;
        let chunk_size = le_u32(&header[4..8])? as usize;
        let data_start = header_end;
        let data_end = data_start
            .checked_add(chunk_size)
            .ok_or_else(|| ImagePolicyError::from("WebP chunk overflow"))?;
        let data = bytes
            .get(data_start..data_end)
            .ok_or_else(|| ImagePolicyError::from("truncated WebP chunk"))?;
        match &header[0..4] {
            b"VP8X" => {
                if data.len() < 10 {
                    return Err("truncated WebP VP8X header".into());
                }
                animated |= data[0] & 0x02 != 0;
                width = Some(
                    1 + u32::from(data[4]) + (u32::from(data[5]) << 8) + (u32::from(data[6]) << 16),
                );
                height = Some(
                    1 + u32::from(data[7]) + (u32::from(data[8]) << 8) + (u32::from(data[9]) << 16),
                );
            }
            b"VP8 " if width.is_none() => {
                if data.len() < 10 || &data[3..6] != b"\x9d\x01\x2a" {
                    return Err("invalid WebP VP8 frame header".into());
                }
                width = Some(u32::from(le_u16(&data[6..8])?) & 0x3fff);
                height = Some(u32::from(le_u16(&data[8..10])?) & 0x3fff);
            }
            b"VP8L" if width.is_none() => {
                if data.len() < 5 || data[0] != 0x2f {
                    return Err("invalid WebP VP8L frame header".into());
                }
                let bits = u32::from(data[1])
                    | (u32::from(data[2]) << 8)
                    | (u32::from(data[3]) << 16)
                    | (u32::from(data[4]) << 24);
                width = Some((bits & 0x3fff) + 1);
                height = Some(((bits >> 14) & 0x3fff) + 1);
            }
            b"ANIM" => animated = true,
            b"ANMF" => {
                animated = true;
                frame_count = frame_count
                    .checked_add(1)
                    .ok_or_else(|| ImagePolicyError::from("WebP frame count overflow"))?;
                if frame_count > MAX_DECODE_FRAMES {
                    return Err(format!("WebP has more than {} frames", MAX_DECODE_FRAMES).into());
                }
            }
            _ => {}
        }
        cursor = data_end + (chunk_size & 1);
    }
    let width = width.ok_or_else(|| ImagePolicyError::from("WebP contains no dimensions"))?;
    let height = height.ok_or_else(|| ImagePolicyError::from("WebP contains no dimensions"))?;
    if animated && frame_count == 0 {
        return Err("animated WebP contains no ANMF frame".into());
    }
    Ok(ImageInspection {
        bytes: bytes.len(),
        sha256: String::new(),
        format: ImageFormat::Webp,
        width,
        height,
        animated,
        frame_count: frame_count.max(1),
    })
}

fn skip_sub_blocks(bytes: &[u8], mut cursor: usize) -> Result<usize, ImagePolicyError> {
    loop {
        let size = usize::from(
            *bytes
                .get(cursor)
                .ok_or_else(|| ImagePolicyError::from("truncated GIF data sub-block"))?,
        );
        cursor += 1;
        if size == 0 {
            return Ok(cursor);
        }
        cursor = cursor
            .checked_add(size)
            .ok_or_else(|| ImagePolicyError::from("GIF data sub-block overflow"))?;
        if cursor > bytes.len() {
            return Err("truncated GIF data sub-block".into());
        }
    }
}

fn color_table_bytes(size_code: u8) -> Result<usize, ImagePolicyError> {
    let entries = 2usize
        .checked_pow(u32::from(size_code) + 1)
        .ok_or_else(|| ImagePolicyError::from("GIF color table size overflow"))?;
    entries
        .checked_mul(3)
        .ok_or_else(|| ImagePolicyError::from("GIF color table byte size overflow"))
}

fn is_jpeg_sof(marker: u8) -> bool {
    matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf)
}

fn be_u16(bytes: &[u8]) -> Result<u16, ImagePolicyError> {
    let pair = bytes
        .get(..2)
        .ok_or_else(|| ImagePolicyError::from("truncated integer"))?;
    Ok(u16::from_be_bytes([pair[0], pair[1]]))
}

fn le_u16(bytes: &[u8]) -> Result<u16, ImagePolicyError> {
    let pair = bytes
        .get(..2)
        .ok_or_else(|| ImagePolicyError::from("truncated integer"))?;
    Ok(u16::from_le_bytes([pair[0], pair[1]]))
}

fn be_u32(bytes: &[u8]) -> Result<u32, ImagePolicyError> {
    let word = bytes
        .get(..4)
        .ok_or_else(|| ImagePolicyError::from("truncated integer"))?;
    Ok(u32::from_be_bytes([word[0], word[1], word[2], word[3]]))
}

fn le_u32(bytes: &[u8]) -> Result<u32, ImagePolicyError> {
    let word = bytes
        .get(..4)
        .ok_or_else(|| ImagePolicyError::from("truncated integer"))?;
    Ok(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

#[cfg(test)]
mod tests {
    use super::{
        inspect_image, validate_decoded_dimensions, ImageFormat, MAX_FILE_SIZE_BYTES, MAX_PIXELS,
    };

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(b"IEND");
        bytes.extend_from_slice(&[0; 4]);
        bytes
    }

    fn gif(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&[0x2c, 0, 0, 0, 0]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0, 2, 2, 0, 0, 0, 0x3b]);
        bytes
    }

    fn jpeg(width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xc0, 0, 17, 8];
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[1, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0]);
        bytes
    }

    fn webp_vp8x(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"RIFF\0\0\0\0WEBPVP8X\n\0\0\0".to_vec();
        bytes.extend_from_slice(&[
            0,
            0,
            0,
            0,
            ((width - 1) & 0xff) as u8,
            (((width - 1) >> 8) & 0xff) as u8,
            (((width - 1) >> 16) & 0xff) as u8,
            ((height - 1) & 0xff) as u8,
            (((height - 1) >> 8) & 0xff) as u8,
            (((height - 1) >> 16) & 0xff) as u8,
        ]);
        bytes
    }

    #[test]
    fn accepts_png_and_reports_dimensions() {
        let result = inspect_image(&png(320, 240)).expect("valid PNG header");
        assert_eq!(result.format, ImageFormat::Png);
        assert_eq!((result.width, result.height), (320, 240));
        assert_eq!(result.frame_count, 1);
        assert_eq!(result.sha256.len(), 64);
        assert!(result.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn accepts_gif_and_counts_one_frame() {
        let result = inspect_image(&gif(32, 24)).expect("valid GIF structure");
        assert_eq!(result.format, ImageFormat::Gif);
        assert_eq!(result.frame_count, 1);
        assert!(!result.animated);
    }

    #[test]
    fn accepts_jpeg_and_reports_dimensions() {
        let result = inspect_image(&jpeg(640, 480)).expect("valid JPEG frame header");
        assert_eq!(result.format, ImageFormat::Jpeg);
        assert_eq!((result.width, result.height), (640, 480));
    }

    #[test]
    fn accepts_webp_vp8x_and_reports_dimensions() {
        let result = inspect_image(&webp_vp8x(128, 96)).expect("valid WebP VP8X header");
        assert_eq!(result.format, ImageFormat::Webp);
        assert_eq!((result.width, result.height), (128, 96));
    }

    #[test]
    fn rejects_dimensions_above_pixel_limit() {
        let result = inspect_image(&png(8192, 8192));
        assert!(result.unwrap_err().to_string().contains("decoded pixels"));
        assert!(u64::from(8192u32) * u64::from(8192u32) > MAX_PIXELS);
    }

    #[test]
    fn applies_the_same_policy_to_decoder_reported_dimensions() {
        assert!(validate_decoded_dimensions(8193, 1).is_err());
        assert!(validate_decoded_dimensions(8192, 8192).is_err());
        assert!(validate_decoded_dimensions(8192, 6103).is_ok());
    }

    #[test]
    fn rejects_oversized_payload_before_parsing() {
        let bytes = vec![0u8; MAX_FILE_SIZE_BYTES + 1];
        assert!(inspect_image(&bytes)
            .unwrap_err()
            .to_string()
            .contains("file limit"));
    }

    #[test]
    fn rejects_unknown_format() {
        assert!(inspect_image(b"not an image").is_err());
    }
}
