//! Bounded desktop image decoding and vision-frame preparation.
//!
//! The header policy is checked before handing bytes to a decoder. Windows
//! uses the system WIC backend for supported formats and streams only the
//! selected animation frames. WebP also has a pure-Rust fallback because WIC
//! is not guaranteed to ship a WebP decoder on every Windows image.

use std::io::Cursor;

use serde::Serialize;

use crate::image_policy::{self, ImageFormat, ImageInspection, MAX_SAMPLE_FRAMES};

#[cfg(windows)]
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
#[cfg(windows)]
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat24bppRGB, IWICImagingFactory, IWICPalette,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnLoad,
};
#[cfg(windows)]
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

pub const VISION_FRAME_WIDTH: u32 = 224;
pub const VISION_FRAME_HEIGHT: u32 = 224;
pub const RGB_CHANNELS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub source_index: u32,
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub inspection: ImageInspection,
    pub sampled_frame_indices: Vec<u32>,
    pub frames: Vec<DecodedFrame>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DecodeSummary {
    pub format: ImageFormat,
    pub source_frame_count: u32,
    pub sampled_frame_indices: Vec<u32>,
    pub frame_shape: [usize; 4],
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreprocessedFrameBatch {
    pub shape: [usize; 4],
    pub data: Vec<f32>,
}

pub fn decode_image(bytes: &[u8], max_sample_frames: u32) -> Result<DecodedImage, String> {
    let inspection = image_policy::inspect_image(bytes).map_err(|error| error.to_string())?;
    let sample_limit = max_sample_frames.min(MAX_SAMPLE_FRAMES);
    if sample_limit == 0 {
        return Err("max_sample_frames must be at least 1".to_owned());
    }
    let sampled_frame_indices = sample_indices(inspection.frame_count, sample_limit);
    #[cfg(windows)]
    let mut frames = match decode_with_wic(bytes, &sampled_frame_indices) {
        Ok(frames) => frames,
        Err(wic_error) if inspection.format == ImageFormat::Webp => {
            decode_with_pure_rust_webp(bytes, &sampled_frame_indices).map_err(|fallback_error| {
                format!(
                    "WIC WebP decoder failed ({wic_error}); pure-Rust fallback failed: {fallback_error}"
                )
            })?
        }
        Err(error) => return Err(error),
    };
    #[cfg(windows)]
    if inspection.format == ImageFormat::Jpeg {
        apply_jpeg_exif_orientation(&mut frames, exif_orientation(bytes))?;
    }
    #[cfg(not(windows))]
    let frames = {
        match inspection.format {
            ImageFormat::Png => {
                if inspection.animated {
                    return Err("animated PNG decoding backend is not installed".to_owned());
                }
                vec![decode_png(bytes)?]
            }
            ImageFormat::Webp => decode_with_pure_rust_webp(bytes, &sampled_frame_indices)?,
            format => {
                return Err(format!(
                    "{} decoder backend is not installed; refusing to fabricate decoded frames",
                    format_name(format)
                ));
            }
        }
    };
    Ok(DecodedImage {
        inspection,
        sampled_frame_indices,
        frames,
    })
}

pub fn summarize(bytes: &[u8], max_sample_frames: u32) -> Result<DecodeSummary, String> {
    let decoded = decode_image(bytes, max_sample_frames)?;
    let batch = preprocess(&decoded.frames)?;
    Ok(DecodeSummary {
        format: decoded.inspection.format,
        source_frame_count: decoded.inspection.frame_count,
        sampled_frame_indices: decoded.sampled_frame_indices,
        frame_shape: batch.shape,
    })
}

/// Retain every frame for short animations and use evenly spaced,
/// endpoint-inclusive indices for longer ones.
pub fn sample_indices(frame_count: u32, max_sample_frames: u32) -> Vec<u32> {
    if frame_count == 0 || max_sample_frames == 0 {
        return Vec::new();
    }
    if frame_count <= max_sample_frames {
        return (0..frame_count).collect();
    }
    let last = frame_count - 1;
    (0..max_sample_frames)
        .map(|index| {
            let value = f64::from(index) * f64::from(last) / f64::from(max_sample_frames - 1);
            python_round_nonnegative(value)
        })
        .collect()
}

fn python_round_nonnegative(value: f64) -> u32 {
    let lower = value.floor() as u32;
    let fraction = value - f64::from(lower);
    if fraction < 0.5 || (fraction == 0.5 && lower.is_multiple_of(2)) {
        lower
    } else {
        lower + 1
    }
}

/// Produce a bounded NCHW RGB frame batch for compatibility diagnostics.
/// The resize is a separable Lanczos-3 implementation so it does not depend
/// on a platform image library.
pub fn preprocess(frames: &[DecodedFrame]) -> Result<PreprocessedFrameBatch, String> {
    if frames.is_empty() {
        return Err("cannot prepare an empty frame batch".to_owned());
    }
    let plane_size = VISION_FRAME_WIDTH as usize * VISION_FRAME_HEIGHT as usize;
    let mut data = Vec::with_capacity(frames.len() * RGB_CHANNELS * plane_size);
    let means = [0.485_f32, 0.456, 0.406];
    let stds = [0.229_f32, 0.224, 0.225];

    for frame in frames {
        let resized = resize_lanczos3(frame)?;
        let (pixels, remainder) = resized.as_chunks::<RGB_CHANNELS>();
        if !remainder.is_empty() {
            return Err("resized RGB buffer is not channel aligned".to_owned());
        }
        for channel in 0..RGB_CHANNELS {
            for pixel in pixels {
                let value = f32::from(pixel[channel]) / 255.0;
                data.push((value - means[channel]) / stds[channel]);
            }
        }
    }
    Ok(PreprocessedFrameBatch {
        shape: [
            frames.len(),
            RGB_CHANNELS,
            VISION_FRAME_HEIGHT as usize,
            VISION_FRAME_WIDTH as usize,
        ],
        data,
    })
}

#[cfg(windows)]
fn apply_jpeg_exif_orientation(
    frames: &mut [DecodedFrame],
    orientation: u16,
) -> Result<(), String> {
    for frame in frames {
        apply_exif_orientation(frame, orientation)?;
    }
    Ok(())
}

/// Read the first valid EXIF Orientation tag from a JPEG APP1 segment.
/// Malformed or absent EXIF is treated as the normal orientation; WIC still
/// remains responsible for decoding the actual pixels.
#[cfg(any(windows, test))]
fn exif_orientation(bytes: &[u8]) -> u16 {
    if bytes.get(0..2) != Some(&[0xff, 0xd8]) {
        return 1;
    }
    let mut offset = 2usize;
    while offset + 4 <= bytes.len() && bytes[offset] == 0xff {
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            break;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if offset + 2 > bytes.len() {
            break;
        }
        let segment_length = usize::from(u16::from_be_bytes([bytes[offset], bytes[offset + 1]]));
        if segment_length < 2 || offset + segment_length > bytes.len() {
            break;
        }
        if marker == 0xe1 {
            if let Some(value) = parse_exif_orientation(&bytes[offset + 2..offset + segment_length])
            {
                return value;
            }
        }
        offset += segment_length;
    }
    1
}

#[cfg(any(windows, test))]
fn parse_exif_orientation(payload: &[u8]) -> Option<u16> {
    if payload.get(0..6) != Some(b"Exif\0\0") {
        return None;
    }
    let tiff = &payload[6..];
    let little_endian = match tiff.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return None,
    };
    let read_u16 = |data: &[u8], offset: usize| -> Option<u16> {
        let bytes = data.get(offset..offset + 2)?;
        Some(if little_endian {
            u16::from_le_bytes([bytes[0], bytes[1]])
        } else {
            u16::from_be_bytes([bytes[0], bytes[1]])
        })
    };
    let read_u32 = |data: &[u8], offset: usize| -> Option<u32> {
        let bytes = data.get(offset..offset + 4)?;
        Some(if little_endian {
            u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        } else {
            u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    };
    if read_u16(tiff, 2)? != 42 {
        return None;
    }
    let ifd_offset = usize::try_from(read_u32(tiff, 4)?).ok()?;
    let entry_count = usize::from(read_u16(tiff, ifd_offset)?);
    for index in 0..entry_count {
        let entry_offset = ifd_offset
            .checked_add(2)?
            .checked_add(index.checked_mul(12)?)?;
        if read_u16(tiff, entry_offset)? != 0x0112 {
            continue;
        }
        let value_type = read_u16(tiff, entry_offset + 2)?;
        let value_count = read_u32(tiff, entry_offset + 4)?;
        if value_count != 1 {
            return None;
        }
        let value = match value_type {
            3 => read_u16(tiff, entry_offset + 8)?,
            4 => u16::try_from(read_u32(tiff, entry_offset + 8)?).ok()?,
            _ => return None,
        };
        return (1..=8).contains(&value).then_some(value);
    }
    None
}

#[cfg(any(windows, test))]
fn apply_exif_orientation(frame: &mut DecodedFrame, orientation: u16) -> Result<(), String> {
    if orientation == 1 {
        return Ok(());
    }
    if !(1..=8).contains(&orientation) {
        return Err(format!("unsupported EXIF orientation: {orientation}"));
    }
    let source_width =
        usize::try_from(frame.width).map_err(|_| "frame width overflow".to_owned())?;
    let source_height =
        usize::try_from(frame.height).map_err(|_| "frame height overflow".to_owned())?;
    let expected = source_width
        .checked_mul(source_height)
        .and_then(|value| value.checked_mul(3))
        .ok_or_else(|| "frame buffer size overflow".to_owned())?;
    if expected != frame.rgb.len() || source_width == 0 || source_height == 0 {
        return Err("decoded RGB buffer has invalid dimensions".to_owned());
    }
    let swaps_axes = matches!(orientation, 5..=8);
    let destination_width = if swaps_axes {
        source_height
    } else {
        source_width
    };
    let destination_height = if swaps_axes {
        source_width
    } else {
        source_height
    };
    let destination_size = destination_width
        .checked_mul(destination_height)
        .and_then(|value| value.checked_mul(3))
        .ok_or_else(|| "oriented frame buffer size overflow".to_owned())?;
    let mut oriented = vec![0_u8; destination_size];
    for destination_y in 0..destination_height {
        for destination_x in 0..destination_width {
            let (source_x, source_y) = match orientation {
                2 => (source_width - 1 - destination_x, destination_y),
                3 => (
                    source_width - 1 - destination_x,
                    source_height - 1 - destination_y,
                ),
                4 => (destination_x, source_height - 1 - destination_y),
                5 => (destination_y, destination_x),
                6 => (destination_y, source_height - 1 - destination_x),
                7 => (
                    source_width - 1 - destination_y,
                    source_height - 1 - destination_x,
                ),
                8 => (source_width - 1 - destination_y, destination_x),
                _ => (destination_x, destination_y),
            };
            let source_offset = (source_y * source_width + source_x) * 3;
            let destination_offset = (destination_y * destination_width + destination_x) * 3;
            oriented[destination_offset..destination_offset + 3]
                .copy_from_slice(&frame.rgb[source_offset..source_offset + 3]);
        }
    }
    frame.width =
        u32::try_from(destination_width).map_err(|_| "oriented width overflow".to_owned())?;
    frame.height =
        u32::try_from(destination_height).map_err(|_| "oriented height overflow".to_owned())?;
    frame.rgb = oriented;
    Ok(())
}

#[cfg(not(windows))]
fn decode_png(bytes: &[u8]) -> Result<DecodedFrame, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("cannot read PNG header: {error}"))?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("cannot decode PNG: {error}"))?;
    let data = &buffer[..info.buffer_size()];
    let rgb = match info.color_type {
        png::ColorType::Rgb => data.to_vec(),
        png::ColorType::Rgba => {
            let (pixels, remainder) = data.as_chunks::<4>();
            if !remainder.is_empty() {
                return Err("PNG RGBA buffer is not channel aligned".to_owned());
            }
            pixels
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect()
        }
        png::ColorType::Grayscale => data.iter().flat_map(|value| [*value; 3]).collect(),
        png::ColorType::GrayscaleAlpha => {
            let (pixels, remainder) = data.as_chunks::<2>();
            if !remainder.is_empty() {
                return Err("PNG grayscale-alpha buffer is not channel aligned".to_owned());
            }
            pixels.iter().flat_map(|pixel| [pixel[0]; 3]).collect()
        }
        png::ColorType::Indexed => {
            return Err("PNG decoder returned an unexpanded indexed image".to_owned())
        }
    };
    Ok(DecodedFrame {
        source_index: 0,
        width: info.width,
        height: info.height,
        rgb,
    })
}

/// Decode WebP without relying on the platform codec registry. This is used
/// as the Windows WIC fallback and on non-Windows builds where only the
/// dependency-light PNG decoder is otherwise shipped.
fn decode_with_pure_rust_webp(bytes: &[u8], wanted: &[u32]) -> Result<Vec<DecodedFrame>, String> {
    if wanted.is_empty() {
        return Err("WebP decoder received no requested frames".to_owned());
    }

    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|error| format!("cannot create pure-Rust WebP decoder: {error}"))?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err("pure-Rust WebP decoder returned an empty image".to_owned());
    }
    let buffer_size = decoder
        .output_buffer_size()
        .ok_or_else(|| "pure-Rust WebP frame buffer size overflow".to_owned())?;
    let has_alpha = decoder.has_alpha();
    let mut buffer = vec![0_u8; buffer_size];
    let mut decoded = Vec::with_capacity(wanted.len());

    if decoder.is_animated() {
        let frame_count = decoder.num_frames();
        if frame_count == 0 || frame_count > image_policy::MAX_DECODE_FRAMES {
            return Err(format!(
                "pure-Rust WebP reported an unsafe frame count: {frame_count}"
            ));
        }
        if wanted.iter().any(|index| *index >= frame_count) {
            return Err("WebP frame count disagrees with the sampled frame indices".to_owned());
        }
        for source_index in 0..frame_count {
            decoder
                .read_frame(&mut buffer)
                .map_err(|error| format!("cannot decode WebP frame {source_index}: {error}"))?;
            if wanted.binary_search(&source_index).is_ok() {
                decoded.push(DecodedFrame {
                    source_index,
                    width,
                    height,
                    rgb: webp_rgb(&buffer, has_alpha)?,
                });
            }
        }
    } else {
        if wanted.iter().any(|index| *index != 0) {
            return Err("static WebP cannot provide a non-zero frame index".to_owned());
        }
        decoder
            .read_image(&mut buffer)
            .map_err(|error| format!("cannot decode WebP image: {error}"))?;
        decoded.push(DecodedFrame {
            source_index: 0,
            width,
            height,
            rgb: webp_rgb(&buffer, has_alpha)?,
        });
    }

    if decoded.len() != wanted.len() {
        return Err("pure-Rust WebP decoder returned an incomplete frame selection".to_owned());
    }
    Ok(decoded)
}

fn webp_rgb(buffer: &[u8], has_alpha: bool) -> Result<Vec<u8>, String> {
    if !has_alpha {
        if !buffer.len().is_multiple_of(3) {
            return Err("WebP RGB buffer is not channel aligned".to_owned());
        }
        return Ok(buffer.to_vec());
    }
    let (pixels, remainder) = buffer.as_chunks::<4>();
    if !remainder.is_empty() {
        return Err("WebP RGBA buffer is not channel aligned".to_owned());
    }
    Ok(pixels
        .iter()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect())
}

fn resize_lanczos3(frame: &DecodedFrame) -> Result<Vec<u8>, String> {
    let expected = usize::try_from(frame.width).ok().and_then(|width| {
        usize::try_from(frame.height)
            .ok()
            .map(|height| width * height * 3)
    });
    if expected != Some(frame.rgb.len()) || frame.width == 0 || frame.height == 0 {
        return Err("decoded RGB buffer has invalid dimensions".to_owned());
    }
    let horizontal = contributions(frame.width as usize, VISION_FRAME_WIDTH as usize);
    let vertical = contributions(frame.height as usize, VISION_FRAME_HEIGHT as usize);
    let mut intermediate = vec![0.0_f32; VISION_FRAME_HEIGHT as usize * frame.width as usize * 3];
    for y in 0..VISION_FRAME_HEIGHT as usize {
        for x in 0..frame.width as usize {
            for channel in 0..3 {
                let mut value = 0.0;
                for (source_y, weight) in &vertical[y] {
                    value +=
                        f32::from(frame.rgb[(source_y * frame.width as usize + x) * 3 + channel])
                            * *weight;
                }
                intermediate[(y * frame.width as usize + x) * 3 + channel] =
                    value.clamp(0.0, 255.0);
            }
        }
    }
    let mut output = vec![0_u8; VISION_FRAME_HEIGHT as usize * VISION_FRAME_WIDTH as usize * 3];
    for y in 0..VISION_FRAME_HEIGHT as usize {
        for x in 0..VISION_FRAME_WIDTH as usize {
            for channel in 0..3 {
                let mut value = 0.0;
                for (source_x, weight) in &horizontal[x] {
                    value += intermediate[(y * frame.width as usize + *source_x) * 3 + channel]
                        * *weight;
                }
                output[(y * VISION_FRAME_WIDTH as usize + x) * 3 + channel] =
                    value.clamp(0.0, 255.0).round() as u8;
            }
        }
    }
    Ok(output)
}

fn contributions(source_len: usize, target_len: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = target_len as f32 / source_len as f32;
    let filter_scale = scale.min(1.0);
    let support = 3.0 / filter_scale;
    (0..target_len)
        .map(|target| {
            let center = (target as f32 + 0.5) / scale - 0.5;
            let left = (center - support).ceil() as isize;
            let right = (center + support).floor() as isize;
            let mut weights = Vec::new();
            for source in left..=right {
                let clamped = source.clamp(0, source_len as isize - 1) as usize;
                let distance = (center - source as f32) * filter_scale;
                let weight = lanczos(distance);
                if weight != 0.0 {
                    weights.push((clamped, weight));
                }
            }
            let sum: f32 = weights.iter().map(|(_, weight)| *weight).sum();
            if sum == 0.0 {
                vec![(
                    center.round().clamp(0.0, (source_len - 1) as f32) as usize,
                    1.0,
                )]
            } else {
                weights
                    .into_iter()
                    .map(|(index, weight)| (index, weight / sum))
                    .collect()
            }
        })
        .collect()
}

fn lanczos(value: f32) -> f32 {
    if value.abs() >= 3.0 {
        0.0
    } else if value == 0.0 {
        1.0
    } else {
        let pi_value = std::f32::consts::PI * value;
        (pi_value.sin() / pi_value) * ((pi_value / 3.0).sin() / (pi_value / 3.0))
    }
}

#[cfg(not(windows))]
fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Webp => "WebP",
        ImageFormat::Gif => "GIF",
    }
}

#[cfg(windows)]
fn decode_with_wic(bytes: &[u8], wanted: &[u32]) -> Result<Vec<DecodedFrame>, String> {
    let _com = ComGuard::initialize()?;
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|error| format!("cannot create Windows Imaging Component factory: {error}"))?;
    let stream = unsafe { factory.CreateStream() }
        .map_err(|error| format!("cannot create WIC memory stream: {error}"))?;
    unsafe { stream.InitializeFromMemory(bytes) }
        .map_err(|error| format!("cannot initialise WIC memory stream: {error}"))?;
    let decoder = unsafe {
        factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnLoad)
    }
    .map_err(|error| format!("cannot create WIC image decoder: {error}"))?;
    let frame_count = unsafe { decoder.GetFrameCount() }
        .map_err(|error| format!("cannot read WIC frame count: {error}"))?;
    if frame_count == 0 || frame_count > image_policy::MAX_DECODE_FRAMES {
        return Err(format!("WIC reported an unsafe frame count: {frame_count}"));
    }
    if wanted.iter().any(|index| *index >= frame_count) {
        return Err("WIC frame count disagrees with the sampled frame indices".to_owned());
    }
    let mut decoded = Vec::with_capacity(wanted.len());
    for &source_index in wanted {
        let frame = unsafe { decoder.GetFrame(source_index) }
            .map_err(|error| format!("cannot decode WIC frame {source_index}: {error}"))?;
        let converter = unsafe { factory.CreateFormatConverter() }
            .map_err(|error| format!("cannot create WIC RGB converter: {error}"))?;
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat24bppRGB,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .map_err(|error| format!("cannot initialise WIC RGB converter: {error}"))?;
        let mut width = 0;
        let mut height = 0;
        unsafe { converter.GetSize(&mut width, &mut height) }
            .map_err(|error| format!("cannot read WIC frame dimensions: {error}"))?;
        if width == 0 || height == 0 {
            return Err("WIC returned an empty frame".to_owned());
        }
        let stride = width
            .checked_mul(3)
            .ok_or_else(|| "WIC frame stride overflow".to_owned())?;
        let byte_count = usize::try_from(stride)
            .ok()
            .and_then(|stride| usize::try_from(height).ok().map(|height| stride * height))
            .ok_or_else(|| "WIC frame buffer size overflow".to_owned())?;
        let mut rgb = vec![0_u8; byte_count];
        unsafe { converter.CopyPixels(std::ptr::null(), stride, &mut rgb) }
            .map_err(|error| format!("cannot copy WIC RGB pixels: {error}"))?;
        decoded.push(DecodedFrame {
            source_index,
            width,
            height,
            rgb,
        });
    }
    Ok(decoded)
}

#[cfg(windows)]
struct ComGuard {
    owns_initialization: bool,
}

#[cfg(windows)]
impl ComGuard {
    fn initialize() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result.0 >= 0 {
            Ok(Self {
                owns_initialization: true,
            })
        } else if result == RPC_E_CHANGED_MODE {
            // Tauri/WebView2 may already initialize this thread as an STA.
            // WIC can be used in that existing apartment; the failed call
            // did not add a COM reference, so Drop must not uninitialize it.
            Ok(Self {
                owns_initialization: false,
            })
        } else {
            Err(format!(
                "cannot initialise COM for WIC: HRESULT 0x{:08X}",
                result.0 as u32
            ))
        }
    }
}

#[cfg(windows)]
impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns_initialization {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        apply_exif_orientation, decode_image, decode_with_pure_rust_webp, exif_orientation,
        preprocess, sample_indices, DecodedFrame, VISION_FRAME_HEIGHT, VISION_FRAME_WIDTH,
    };

    fn png_bytes() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        let mut encoder = png::Encoder::new(&mut bytes, 8, 6);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        let pixels = [255_u8, 0, 0, 255].repeat(8 * 6);
        writer.write_image_data(&pixels).expect("PNG pixels");
        drop(writer);
        bytes.into_inner()
    }

    #[test]
    fn samples_short_and_long_animations_with_endpoints() {
        assert_eq!(sample_indices(4, 12), vec![0, 1, 2, 3]);
        assert_eq!(
            sample_indices(120, 12),
            vec![0, 11, 22, 32, 43, 54, 65, 76, 87, 97, 108, 119]
        );
    }

    #[test]
    fn decodes_static_png_and_matches_vision_frame_shape() {
        let decoded = decode_image(&png_bytes(), 12).expect("decode PNG");
        assert_eq!(decoded.frames.len(), 1);
        let batch = preprocess(&decoded.frames).expect("preprocess PNG");
        assert_eq!(
            batch.shape,
            [
                1,
                3,
                VISION_FRAME_HEIGHT as usize,
                VISION_FRAME_WIDTH as usize
            ]
        );
        assert_eq!(
            batch.data.len(),
            3 * VISION_FRAME_WIDTH as usize * VISION_FRAME_HEIGHT as usize
        );
        assert!(batch.data.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn rejects_malformed_input_without_inventing_frames() {
        assert!(decode_image(b"not a PNG", 12).is_err());
    }

    #[test]
    fn rejects_empty_frame_batch() {
        assert!(preprocess(&[]).is_err());
    }

    fn jpeg_with_exif_orientation(orientation: u16) -> Vec<u8> {
        let mut payload = b"Exif\0\0II*\0".to_vec();
        payload.extend([8, 0, 0, 0]);
        payload.extend([1, 0]);
        payload.extend([0x12, 0x01, 3, 0, 1, 0, 0, 0]);
        payload.extend([orientation as u8, (orientation >> 8) as u8, 0, 0]);
        payload.extend([0, 0, 0, 0]);
        let segment_length = u16::try_from(payload.len() + 2).expect("APP1 length");
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xe1];
        bytes.extend(segment_length.to_be_bytes());
        bytes.extend(payload);
        bytes.extend([0xff, 0xda]);
        bytes
    }

    #[test]
    fn reads_exif_orientation_and_defaults_without_exif() {
        assert_eq!(exif_orientation(&jpeg_with_exif_orientation(6)), 6);
        assert_eq!(exif_orientation(b"not jpeg"), 1);
    }

    #[test]
    fn applies_exif_rotation_to_rgb_dimensions_and_pixels() {
        let mut frame = DecodedFrame {
            source_index: 0,
            width: 2,
            height: 1,
            rgb: vec![10, 0, 0, 20, 0, 0],
        };
        apply_exif_orientation(&mut frame, 6).expect("rotate clockwise");
        assert_eq!((frame.width, frame.height), (1, 2));
        assert_eq!(frame.rgb, vec![10, 0, 0, 20, 0, 0]);

        let mut counter_clockwise = DecodedFrame {
            source_index: 0,
            width: 2,
            height: 1,
            rgb: vec![10, 0, 0, 20, 0, 0],
        };
        apply_exif_orientation(&mut counter_clockwise, 8).expect("rotate counter-clockwise");
        assert_eq!((counter_clockwise.width, counter_clockwise.height), (1, 2));
        assert_eq!(counter_clockwise.rgb, vec![20, 0, 0, 10, 0, 0]);
    }

    #[test]
    fn pure_rust_webp_decoder_reads_static_and_animated_frames() {
        let webp = decode_base64("UklGRsoAAABXRUJQVlA4WAoAAAACAAAAAQAAAQAAQU5JTQYAAAAAAAAAAABBTk1GSgAAAAAAAAAAAAEAAAEAAAoAAAJWUDggMgAAADABAJ0BKgIAAgABQCYloAADcAD+8ut///mwP/bz/wR6Af//0uD//pcH//S4P/SkAAAAQU5NRkwAAAAAAAAAAAABAAABAAAKAAAAVlA4IDQAAAA0AQCdASoCAAIAAAAmJaAAA3AA/ukiH//3nz//ufP/+58/6M///yn7//I4//8jj/5QIAAA");
        let decoded = decode_with_pure_rust_webp(&webp, &[0, 1]).expect("decode animated WebP");
        assert_eq!(decoded.len(), 2);
        assert_eq!(
            decoded
                .iter()
                .map(|frame| frame.source_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(decoded
            .iter()
            .all(|frame| frame.width == 2 && frame.height == 2));
        assert!(decoded.iter().all(|frame| frame.rgb.len() == 2 * 2 * 3));

        let mut static_webp = Vec::new();
        image_webp::WebPEncoder::new(&mut static_webp)
            .encode(
                &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255],
                2,
                2,
                image_webp::ColorType::Rgb8,
            )
            .expect("encode static WebP fixture");
        assert!(decode_with_pure_rust_webp(&static_webp, &[0]).is_ok());
    }

    #[cfg(any())]
    #[test]
    fn windows_wic_decodes_jpeg_and_animated_gif_and_webp() {
        let jpeg = decode_base64("/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAMCAgMCAgMDAwMEAwMEBQgFBQQEBQoHBwYIDAoMDAsKCwsNDhIQDQ4RDgsLEBYQERMUFRUVDA8XGBYUGBIUFRT/2wBDAQMEBAUEBQkFBQkUDQsNFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBT/wAARCAACAAIDASIAAhEBAxEB/8QAHwAAAQUBAQEBAQEAAAAAAAAAAAECAwQFBgcICQoL/8QAtRAAAgEDAwIEAwUFBAQAAAF9AQIDAAQRBRIhMUEGE1FhByJxFDKBkaEII0KxwRVS0fAkM2JyggkKFhcYGRolJicoKSo0NTY3ODk6Q0RFRkdISUpTVFVWV1hZWmNkZWZnaGlqc3R1dnd4eXqDhIWGh4iJipKTlJWWl5iZmqKjpKWmp6ipqrKztLW2t7i5usLDxMXGx8jJytLT1NXW19jZ2uHi4+Tl5ufo6erx8vP09fb3+Pn6/8QAHwEAAwEBAQEBAQEBAQAAAAAAAAECAwQFBgcICQoL/8QAtREAAgECBAQDBAcFBAQAAQJ3AAECAxEEBSExBhJBUQdhcRMiMoEIFEKRobHBCSMzUvAVYnLRChYkNOEl8RcYGRomJygpKjU2Nzg5OkNERUZHSElKU1RVVldYWVpjZGVmZ2hpanN0dXZ3eHl6goOEhYaHiImKkpOUlZaXmJmaoqOkpaanqKmqsrO0tba3uLm6wsPExcbHyMnK0tPU1dbX2Nna4uPk5ebn6Onq8vP09fb3+Pn6/9oADAMBAAIRAxEAPwD50ooor8MP9Uz/2Q==");
        let jpeg_result = decode_image(&jpeg, 12).expect("WIC JPEG decode");
        assert_eq!(jpeg_result.frames.len(), 1);

        let gif = decode_base64("R0lGODlhAgACAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQAAQAAACwAAAAAAgACAAAIBgABCAQQEAAh+QQBAQABACwAAAAAAgACAIEA/wAAAAAAAAAAAAAIBgABCAQQEAA7");
        let gif_result = decode_image(&gif, 12).expect("WIC GIF decode");
        assert_eq!(gif_result.inspection.frame_count, 2);
        assert_eq!(
            gif_result
                .frames
                .iter()
                .map(|frame| frame.source_index)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );

        let webp = decode_base64("UklGRsoAAABXRUJQVlA4WAoAAAACAAAAAQAAAQAAQU5JTQYAAAAAAAAAAABBTk1GSgAAAAAAAAAAAAEAAAEAAAoAAAJWUDggMgAAADABAJ0BKgIAAgABQCYloAADcAD+8ut///mwP/bz/wR6Af//0uD//pcH//S4P/SkAAAAQU5NRkwAAAAAAAAAAAABAAABAAAKAAAAVlA4IDQAAAA0AQCdASoCAAIAAAAmJaAAA3AA/ukiH//3nz//ufP/+58/6M///yn7//I4//8jj/5QIAAA");
        let webp_result = decode_image(&webp, 12).expect("WIC WebP decode");
        assert_eq!(webp_result.inspection.frame_count, 2);
        assert_eq!(webp_result.frames.len(), 2);
    }

    #[cfg(windows)]
    #[test]
    fn windows_wic_decodes_valid_jpeg_gif_and_webp_fixtures() {
        let jpeg = decode_base64("/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDACgcHiMeGSgjISMtKygwPGRBPDc3PHtYXUlkkYCZlo+AjIqgtObDoKrarYqMyP/L2u71////m8H////6/+b9//j/2wBDASstLTw1PHZBQXb4pYyl+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj4+Pj/wAARCAACAAIDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAT/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAABAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCUAdYf/9k=");
        assert_eq!(decode_image(&jpeg, 12).unwrap().frames.len(), 1);

        let gif = decode_base64("R0lGODlhAgACAIEAAP8AAAAAAAAAAAAAACH/C05FVFNDQVBFMi4wAwEAAAAh+QQAAQAAACwAAAAAAgACAAAIBgABCAQQEAAh+QQBAQABACwAAAAAAgACAIEA/wAAAAAAAAAAAAAIBgABCAQQEAA7");
        let gif_result = decode_image(&gif, 12).expect("WIC GIF decode");
        assert_eq!(gif_result.inspection.frame_count, 2);
        assert_eq!(gif_result.frames.len(), 2);

        let webp = decode_base64("UklGRsoAAABXRUJQVlA4WAoAAAACAAAAAQAAAQAAQU5JTQYAAAAAAAAAAABBTk1GSgAAAAAAAAAAAAEAAAEAAAoAAAJWUDggMgAAADABAJ0BKgIAAgABQCYloAADcAD+8ut///mwP/bz/wR6Af//0uD//pcH//S4P/SkAAAAQU5NRkwAAAAAAAAAAAABAAABAAAKAAAAVlA4IDQAAAA0AQCdASoCAAIAAAAmJaAAA3AA/ukiH//3nz//ufP/+58/6M///yn7//I4//8jj/5QIAAA");
        let webp_result = decode_image(&webp, 12).expect("WIC WebP decode");
        assert_eq!(webp_result.inspection.frame_count, 2);
        assert_eq!(webp_result.frames.len(), 2);
    }

    fn decode_base64(value: &str) -> Vec<u8> {
        let mut output = Vec::new();
        let mut accumulator = 0_u32;
        let mut bits = 0_u8;
        for byte in value.bytes() {
            if byte == b'=' {
                break;
            }
            let digit = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => continue,
            };
            accumulator = (accumulator << 6) | u32::from(digit);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                output.push((accumulator >> bits) as u8);
                accumulator &= (1 << bits) - 1;
            }
        }
        output
    }
}
