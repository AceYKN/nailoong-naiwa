//! OpenCV-backed reference matching.
//!
//! This module is feature-gated because OpenCV is a system dependency on the
//! Windows target. The deterministic policy and recall gates live in
//! [`crate::vision`] so they remain testable without a native CV runtime.

use std::{
    io::{Cursor, Read},
    path::Path,
};

use opencv::{
    calib3d::{find_homography, RANSAC},
    core::{
        self, no_array, DMatch, KeyPoint, Mat, Point, Point2f, Rect, Scalar, Size, Vector, CV_8UC3,
        NORM_L2,
    },
    features2d::{BFMatcher, SIFT},
    imgcodecs::imencode_def,
    imgproc::{self, COLOR_GRAY2BGR, COLOR_RGB2GRAY, INTER_AREA, LINE_8},
    prelude::*,
};

use crate::{
    decoder::DecodedFrame,
    phash,
    vision::{self, MatchResult, ReferenceClass, VisionThresholds},
};

pub const DEFAULT_MAX_WORKING_DIMENSION: i32 = 1280;
pub const DEFAULT_MAX_KEYPOINTS: i32 = 1_000;
pub const DEFAULT_LOWE_RATIO: f32 = 0.65;
pub const DEFAULT_RANSAC_REPROJECTION_THRESHOLD: f64 = 3.0;
pub const DEFAULT_PHASH_SHORTCUT_DISTANCE: u32 = 4;
const DESCRIPTOR_MAGIC: &[u8] = b"NLNF-DESC\0";
const DESCRIPTOR_VERSION: u32 = 2;
const MAX_DESCRIPTOR_BYTES: usize = 64 * 1024 * 1024;
const MAX_SERIALIZED_KEYPOINTS: u32 = 100_000;
const COLOR_SIGNATURE_HUE_BINS: usize = 12;
const COLOR_SIGNATURE_SAMPLE_TARGET: usize = 4_096;
const COLOR_SIGNATURE_LOW_SATURATION: f32 = 45.0 / 255.0;
const COLOR_SIGNATURE_FULL_SIMILARITY: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq)]
struct ColorSignature {
    hue_histogram: [f32; COLOR_SIGNATURE_HUE_BINS],
    low_saturation_fraction: f32,
}

impl ColorSignature {
    fn from_rgb(rgb: &[u8], width: u32, height: u32) -> Result<Self, String> {
        let width_usize = usize::try_from(width)
            .map_err(|_| "color signature width overflows usize".to_owned())?;
        let height_usize = usize::try_from(height)
            .map_err(|_| "color signature height overflows usize".to_owned())?;
        let expected = width_usize
            .checked_mul(height_usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or_else(|| "color signature buffer size overflows usize".to_owned())?;
        if width == 0 || height == 0 || rgb.len() != expected {
            return Err("color signature RGB buffer has inconsistent dimensions".to_owned());
        }

        let pixel_count = width_usize
            .checked_mul(height_usize)
            .ok_or_else(|| "color signature pixel count overflows usize".to_owned())?;
        let step = ((pixel_count as f64 / COLOR_SIGNATURE_SAMPLE_TARGET as f64)
            .sqrt()
            .ceil() as usize)
            .max(1);
        let mut hue_histogram = [0.0_f32; COLOR_SIGNATURE_HUE_BINS];
        let mut sampled = 0_u32;
        let mut low_saturation = 0_u32;
        for y in (0..height_usize).step_by(step) {
            for x in (0..width_usize).step_by(step) {
                let index = (y * width_usize + x) * 3;
                let red = rgb[index] as f32 / 255.0;
                let green = rgb[index + 1] as f32 / 255.0;
                let blue = rgb[index + 2] as f32 / 255.0;
                let max = red.max(green).max(blue);
                let min = red.min(green).min(blue);
                let delta = max - min;
                let saturation = if max <= f32::EPSILON {
                    0.0
                } else {
                    delta / max
                };
                sampled += 1;
                if saturation < COLOR_SIGNATURE_LOW_SATURATION {
                    low_saturation += 1;
                    continue;
                }
                let mut hue = if delta <= f32::EPSILON {
                    0.0
                } else if (max - red).abs() <= f32::EPSILON {
                    ((green - blue) / delta).rem_euclid(6.0)
                } else if (max - green).abs() <= f32::EPSILON {
                    (blue - red) / delta + 2.0
                } else {
                    (red - green) / delta + 4.0
                };
                hue /= 6.0;
                let bin = ((hue * COLOR_SIGNATURE_HUE_BINS as f32).floor() as usize)
                    .min(COLOR_SIGNATURE_HUE_BINS - 1);
                hue_histogram[bin] += 1.0;
            }
        }
        if sampled == 0 {
            return Err("color signature sampled no pixels".to_owned());
        }
        let saturated = hue_histogram.iter().sum::<f32>();
        if saturated > 0.0 {
            for value in &mut hue_histogram {
                *value /= saturated;
            }
        }
        Ok(Self {
            hue_histogram,
            low_saturation_fraction: low_saturation as f32 / sampled as f32,
        })
    }

    fn similarity(self, other: Self) -> f32 {
        let self_saturated = self.hue_histogram.iter().sum::<f32>() > 0.0;
        let other_saturated = other.hue_histogram.iter().sum::<f32>() > 0.0;
        let hue_similarity = if self_saturated && other_saturated {
            self.hue_histogram
                .iter()
                .zip(other.hue_histogram.iter())
                .map(|(left, right)| (left * right).sqrt())
                .sum::<f32>()
        } else if !self_saturated && !other_saturated {
            1.0
        } else {
            0.0
        };
        let saturation_similarity =
            1.0 - (self.low_saturation_fraction - other.low_saturation_fraction).abs();
        (0.85 * hue_similarity + 0.15 * saturation_similarity).clamp(0.0, 1.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OpenCvConfig {
    pub max_working_dimension: i32,
    pub max_keypoints: i32,
    pub lowe_ratio: f32,
    pub ransac_reprojection_threshold: f64,
    pub phash_shortcut_distance: u32,
    pub vision_thresholds: VisionThresholds,
}

impl Default for OpenCvConfig {
    fn default() -> Self {
        Self {
            max_working_dimension: DEFAULT_MAX_WORKING_DIMENSION,
            max_keypoints: DEFAULT_MAX_KEYPOINTS,
            lowe_ratio: DEFAULT_LOWE_RATIO,
            ransac_reprojection_threshold: DEFAULT_RANSAC_REPROJECTION_THRESHOLD,
            phash_shortcut_distance: DEFAULT_PHASH_SHORTCUT_DISTANCE,
            vision_thresholds: VisionThresholds::default(),
        }
    }
}

pub struct ReferenceFeatures {
    pub reference_id: String,
    pub class: ReferenceClass,
    pub phash: u64,
    pub width: i32,
    pub height: i32,
    color_signature: ColorSignature,
    pub keypoints: Vector<KeyPoint>,
    pub descriptors: Mat,
}

struct QueryFeatures {
    phash: u64,
    width: i32,
    height: i32,
    color_signature: ColorSignature,
    keypoints: Vector<KeyPoint>,
    descriptors: Mat,
}

#[derive(Debug, Clone, Copy)]
struct DebugMatchPoint {
    query: Point2f,
    reference: Point2f,
    inlier: bool,
}

pub struct OpenCvVisionEngine {
    config: OpenCvConfig,
    sift: opencv::core::Ptr<SIFT>,
}

impl OpenCvVisionEngine {
    pub fn new(config: OpenCvConfig) -> Result<Self, String> {
        if !(0.0..1.0).contains(&config.lowe_ratio)
            || config.max_working_dimension < 64
            || config.max_keypoints < 100
            || config.ransac_reprojection_threshold <= 0.0
            || config.phash_shortcut_distance > 64
        {
            return Err("invalid OpenCV matcher configuration".to_owned());
        }
        let sift =
            SIFT::create(config.max_keypoints, 3, 0.04, 10.0, 1.6, false).map_err(cv_error)?;
        Ok(Self { config, sift })
    }

    pub fn extract_reference(
        &mut self,
        reference_id: impl Into<String>,
        class: ReferenceClass,
        frame: &DecodedFrame,
    ) -> Result<ReferenceFeatures, String> {
        let phash = phash::compute_rgb(&frame.rgb, frame.width, frame.height)?;
        let (gray, width, height) = self.working_gray(frame)?;
        let (keypoints, descriptors) = self.extract_descriptors(&gray)?;
        Ok(ReferenceFeatures {
            reference_id: reference_id.into(),
            class,
            phash,
            width,
            height,
            color_signature: ColorSignature::from_rgb(&frame.rgb, frame.width, frame.height)?,
            keypoints,
            descriptors,
        })
    }

    pub fn classify_frames(
        &mut self,
        frames: &[DecodedFrame],
        references: &[ReferenceFeatures],
    ) -> Result<vision::ClassificationResult, String> {
        if frames.is_empty() {
            return Err("at least one decoded frame is required".to_owned());
        }
        if references.is_empty() {
            return Err("at least one reference image is required".to_owned());
        }
        let mut frame_matches = Vec::with_capacity(frames.len());
        for frame in frames {
            let query_phash = phash::compute_rgb(&frame.rgb, frame.width, frame.height)?;
            let has_shortcut = references.iter().any(|reference| {
                phash::hamming_distance(query_phash, reference.phash)
                    <= self.config.phash_shortcut_distance
            });
            if has_shortcut {
                let mut matches = Vec::with_capacity(references.len());
                for reference in references {
                    let distance = phash::hamming_distance(query_phash, reference.phash);
                    matches.push(if distance <= self.config.phash_shortcut_distance {
                        phash_shortcut_match(reference, distance)
                    } else {
                        empty_match_without_query(reference, distance)
                    });
                }
                frame_matches.push(matches);
                continue;
            }

            let query = self.extract_query(frame, query_phash)?;
            let mut matches = Vec::with_capacity(references.len());
            for reference in references {
                matches.push(self.match_reference(&query, reference)?);
            }
            frame_matches.push(matches);
        }
        vision::classify_frames(&frame_matches, self.config.vision_thresholds)
    }

    fn extract_query(&mut self, frame: &DecodedFrame, phash: u64) -> Result<QueryFeatures, String> {
        let (gray, width, height) = self.working_gray(frame)?;
        let (keypoints, descriptors) = self.extract_descriptors(&gray)?;
        Ok(QueryFeatures {
            phash,
            width,
            height,
            color_signature: ColorSignature::from_rgb(&frame.rgb, frame.width, frame.height)?,
            keypoints,
            descriptors,
        })
    }

    fn working_gray(&self, frame: &DecodedFrame) -> Result<(Mat, i32, i32), String> {
        let width =
            i32::try_from(frame.width).map_err(|_| "frame width overflows i32".to_owned())?;
        let height =
            i32::try_from(frame.height).map_err(|_| "frame height overflows i32".to_owned())?;
        if width <= 0 || height <= 0 {
            return Err("decoded frame dimensions must be positive".to_owned());
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .map(|height| width * height * 3)
            })
            .ok_or_else(|| "decoded frame buffer size overflows usize".to_owned())?;
        if frame.rgb.len() != expected {
            return Err("decoded frame RGB buffer has inconsistent dimensions".to_owned());
        }
        let mut rgb =
            Mat::new_rows_cols_with_default(height, width, core::CV_8UC3, Scalar::all(0.0))
                .map_err(cv_error)?;
        rgb.data_bytes_mut()
            .map_err(cv_error)?
            .copy_from_slice(&frame.rgb);
        let mut gray = Mat::default();
        imgproc::cvt_color_def(&rgb, &mut gray, COLOR_RGB2GRAY).map_err(cv_error)?;

        let max_dimension = width.max(height);
        if max_dimension <= self.config.max_working_dimension {
            return Ok((gray, width, height));
        }
        let scale = f64::from(self.config.max_working_dimension) / f64::from(max_dimension);
        let resized_width = (f64::from(width) * scale).round().max(1.0) as i32;
        let resized_height = (f64::from(height) * scale).round().max(1.0) as i32;
        let mut resized = Mat::default();
        imgproc::resize(
            &gray,
            &mut resized,
            Size::new(resized_width, resized_height),
            0.0,
            0.0,
            INTER_AREA,
        )
        .map_err(cv_error)?;
        Ok((resized, resized_width, resized_height))
    }

    fn extract_descriptors(&mut self, gray: &Mat) -> Result<(Vector<KeyPoint>, Mat), String> {
        let mut keypoints = Vector::new();
        let mut descriptors = Mat::default();
        self.sift
            .detect_and_compute(gray, &no_array(), &mut keypoints, &mut descriptors, false)
            .map_err(cv_error)?;
        Ok((keypoints, descriptors))
    }

    fn match_reference(
        &self,
        query: &QueryFeatures,
        reference: &ReferenceFeatures,
    ) -> Result<MatchResult, String> {
        self.match_reference_with_points(query, reference)
            .map(|(result, _)| result)
    }

    fn match_reference_with_points(
        &self,
        query: &QueryFeatures,
        reference: &ReferenceFeatures,
    ) -> Result<(MatchResult, Vec<DebugMatchPoint>), String> {
        let phash_distance = Some(phash::hamming_distance(query.phash, reference.phash));
        if query.descriptors.empty() || reference.descriptors.empty() {
            return Ok((empty_match(reference, query, phash_distance), Vec::new()));
        }
        let matcher = BFMatcher::new(NORM_L2, false).map_err(cv_error)?;
        let mut knn_matches = Vector::<Vector<DMatch>>::new();
        matcher
            .knn_train_match_def(
                &query.descriptors,
                &reference.descriptors,
                &mut knn_matches,
                2,
            )
            .map_err(cv_error)?;
        let mut good_matches = Vector::<DMatch>::new();
        for pair in knn_matches.iter() {
            if pair.len() < 2 {
                continue;
            }
            let best = pair.get(0).map_err(cv_error)?;
            let second = pair.get(1).map_err(cv_error)?;
            if best.distance < self.config.lowe_ratio * second.distance {
                good_matches.push(best);
            }
        }
        let good_match_count = good_matches.len() as u32;
        if good_match_count < 4 {
            return Ok((
                empty_match(reference, query, phash_distance).with_good_matches(good_match_count),
                Vec::new(),
            ));
        }

        let mut query_points = Vector::<Point2f>::new();
        let mut reference_points = Vector::<Point2f>::new();
        for item in good_matches.iter() {
            query_points.push(
                query
                    .keypoints
                    .get(item.query_idx as usize)
                    .map_err(cv_error)?
                    .pt(),
            );
            reference_points.push(
                reference
                    .keypoints
                    .get(item.train_idx as usize)
                    .map_err(cv_error)?
                    .pt(),
            );
        }

        let mut mask = Mat::default();
        let homography = find_homography(
            &query_points,
            &reference_points,
            &mut mask,
            RANSAC,
            self.config.ransac_reprojection_threshold,
        )
        .map_err(cv_error)?;
        if homography.empty() {
            return Ok((
                empty_match(reference, query, phash_distance).with_good_matches(good_match_count),
                query_points
                    .iter()
                    .zip(reference_points.iter())
                    .map(|(query, reference)| DebugMatchPoint {
                        query,
                        reference,
                        inlier: false,
                    })
                    .collect(),
            ));
        }
        let mask_values = mask.data_typed::<u8>().map_err(cv_error)?;
        let inlier_count = mask_values
            .iter()
            .take(good_matches.len())
            .filter(|value| **value != 0)
            .count() as u32;
        let inlier_ratio = inlier_count as f32 / good_match_count as f32;
        let coverage = spatial_coverage(&query_points, mask_values, query.width, query.height);
        let reprojection_error =
            reprojection_error(&query_points, &reference_points, mask_values, &homography)?;
        let base_score = vision::score_match(
            good_match_count,
            inlier_count,
            inlier_ratio,
            coverage,
            reprojection_error,
            phash_distance,
        )?;
        let color_similarity = query.color_signature.similarity(reference.color_signature);
        let score = apply_color_compatibility(base_score, color_similarity);
        let debug_points = query_points
            .iter()
            .zip(reference_points.iter())
            .enumerate()
            .map(|(index, (query, reference))| DebugMatchPoint {
                query,
                reference,
                inlier: mask_values.get(index).copied().unwrap_or(0) != 0,
            })
            .collect();
        Ok((
            MatchResult {
                reference_id: reference.reference_id.clone(),
                class: reference.class,
                keypoint_count: query.keypoints.len() as u32,
                good_match_count,
                inlier_count,
                inlier_ratio,
                coverage,
                reprojection_error,
                phash_distance,
                score,
            },
            debug_points,
        ))
    }

    /// Render a side-by-side developer diagnostic. Green points/lines are
    /// RANSAC inliers; red points are Lowe-filtered matches rejected by the
    /// geometric model. The output is a PNG owned by the caller.
    pub fn render_match_debug(
        &mut self,
        query_frame: &DecodedFrame,
        reference_frame: &DecodedFrame,
        reference: &ReferenceFeatures,
    ) -> Result<Vec<u8>, String> {
        let query_phash =
            phash::compute_rgb(&query_frame.rgb, query_frame.width, query_frame.height)?;
        let query = self.extract_query(query_frame, query_phash)?;
        let (_, reference_width, reference_height) = self.working_gray(reference_frame)?;
        let (_, query_width, query_height) = self.working_gray(query_frame)?;
        let (result, points) = self.match_reference_with_points(&query, reference)?;
        let _ = result;

        let (reference_gray, _, _) = self.working_gray(reference_frame)?;
        let (query_gray, _, _) = self.working_gray(query_frame)?;
        let mut reference_color = Mat::default();
        let mut query_color = Mat::default();
        imgproc::cvt_color_def(&reference_gray, &mut reference_color, COLOR_GRAY2BGR)
            .map_err(cv_error)?;
        imgproc::cvt_color_def(&query_gray, &mut query_color, COLOR_GRAY2BGR).map_err(cv_error)?;
        let canvas_height = reference_height.max(query_height);
        let canvas_width = reference_width
            .checked_add(query_width)
            .ok_or_else(|| "debug canvas width overflow".to_owned())?;
        let mut canvas = Mat::new_rows_cols_with_default(
            canvas_height,
            canvas_width,
            CV_8UC3,
            Scalar::all(32.0),
        )
        .map_err(cv_error)?;
        {
            let mut roi = Mat::roi_mut(
                &mut canvas,
                Rect::new(0, 0, reference_width, reference_height),
            )
            .map_err(cv_error)?;
            reference_color.copy_to(&mut roi).map_err(cv_error)?;
        }
        {
            let mut roi = Mat::roi_mut(
                &mut canvas,
                Rect::new(reference_width, 0, query_width, query_height),
            )
            .map_err(cv_error)?;
            query_color.copy_to(&mut roi).map_err(cv_error)?;
        }
        for point in points {
            let reference_point = Point::new(
                point.reference.x.round() as i32,
                point.reference.y.round() as i32,
            );
            let query_point = Point::new(
                reference_width + point.query.x.round() as i32,
                point.query.y.round() as i32,
            );
            let color = if point.inlier {
                Scalar::new(80.0, 220.0, 120.0, 0.0)
            } else {
                Scalar::new(40.0, 80.0, 230.0, 0.0)
            };
            imgproc::circle(&mut canvas, reference_point, 4, color, -1, LINE_8, 0)
                .map_err(cv_error)?;
            imgproc::circle(&mut canvas, query_point, 4, color, -1, LINE_8, 0).map_err(cv_error)?;
            if point.inlier {
                imgproc::line(
                    &mut canvas,
                    reference_point,
                    query_point,
                    color,
                    1,
                    LINE_8,
                    0,
                )
                .map_err(cv_error)?;
            }
        }
        let mut encoded = Vector::<u8>::new();
        if !imencode_def(".png", &canvas, &mut encoded).map_err(cv_error)? {
            return Err("OpenCV failed to encode the debug overlay".to_owned());
        }
        Ok(encoded.to_vec())
    }
}

/// Persist extracted keypoints and descriptors in a small versioned binary
/// cache. The cache is derived data; the original reference image remains the
/// source of truth and can be re-extracted if this format changes.
pub fn write_reference_features(path: &Path, features: &ReferenceFeatures) -> Result<(), String> {
    let descriptor_bytes = features.descriptors.data_bytes().map_err(cv_error)?;
    if descriptor_bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err("descriptor cache exceeds the 64 MiB limit".to_owned());
    }
    let keypoint_count = u32::try_from(features.keypoints.len())
        .map_err(|_| "keypoint count overflows u32".to_owned())?;
    let mut bytes = Vec::with_capacity(
        DESCRIPTOR_MAGIC.len()
            + 4
            + 1
            + 4
            + 4
            + 8
            + 4
            + features.keypoints.len() * 28
            + COLOR_SIGNATURE_HUE_BINS * 4
            + 4
            + 12
            + 8
            + descriptor_bytes.len(),
    );
    bytes.extend_from_slice(DESCRIPTOR_MAGIC);
    push_u32(&mut bytes, DESCRIPTOR_VERSION);
    bytes.push(reference_class_code(features.class));
    push_i32(&mut bytes, features.width);
    push_i32(&mut bytes, features.height);
    push_u64(&mut bytes, features.phash);
    for value in features.color_signature.hue_histogram {
        push_f32(&mut bytes, value);
    }
    push_f32(&mut bytes, features.color_signature.low_saturation_fraction);
    push_u32(&mut bytes, keypoint_count);
    for keypoint in features.keypoints.iter() {
        let point = keypoint.pt();
        push_f32(&mut bytes, point.x);
        push_f32(&mut bytes, point.y);
        push_f32(&mut bytes, keypoint.size());
        push_f32(&mut bytes, keypoint.angle());
        push_f32(&mut bytes, keypoint.response());
        push_i32(&mut bytes, keypoint.octave());
        push_i32(&mut bytes, keypoint.class_id());
    }
    push_i32(&mut bytes, features.descriptors.rows());
    push_i32(&mut bytes, features.descriptors.cols());
    push_i32(&mut bytes, features.descriptors.typ());
    push_u64(
        &mut bytes,
        u64::try_from(descriptor_bytes.len())
            .map_err(|_| "descriptor byte length overflows u64".to_owned())?,
    );
    bytes.extend_from_slice(descriptor_bytes);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create descriptor directory: {error}"))?;
    }
    write_cache_bytes(path, &bytes)
}

/// Write derived descriptor bytes without depending on the source image's
/// volume. The normal path is a same-directory atomic rename. Some Windows
/// filesystem providers still report `ERROR_NOT_SAME_DEVICE`; copying and
/// checking the byte count is a safe fallback because the cache is derived
/// data and can always be rebuilt from the reference image.
fn write_cache_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| "descriptor cache path has no parent directory".to_owned())?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "descriptor cache path has no valid filename".to_owned())?;
    let mut temporary = parent.join(format!(".{filename}.{}.tmp", std::process::id()));
    for attempt in 0..100u32 {
        if !temporary.exists() {
            break;
        }
        temporary = parent.join(format!(".{filename}.{}-{attempt}.tmp", std::process::id()));
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("cannot write descriptor cache: {error}"))?;
    if let Err(error) = file.write_all(bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot write descriptor cache: {error}"));
    }
    if let Err(error) = file.sync_all() {
        let _ = std::fs::remove_file(&temporary);
        return Err(format!("cannot flush descriptor cache: {error}"));
    }
    drop(file);

    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if matches!(error.raw_os_error(), Some(17 | 18)) => {
            match std::fs::copy(&temporary, path) {
                Ok(copied) if copied == bytes.len() as u64 => {
                    let _ = std::fs::remove_file(&temporary);
                    Ok(())
                }
                Ok(copied) => {
                    let _ = std::fs::remove_file(&temporary);
                    let _ = std::fs::remove_file(path);
                    Err(format!(
                        "cannot verify copied descriptor cache: expected {} bytes, got {copied}",
                        bytes.len()
                    ))
                }
                Err(copy_error) => {
                    let _ = std::fs::remove_file(&temporary);
                    Err(format!(
                        "cannot finalize descriptor cache after cross-device rename ({error}): {copy_error}"
                    ))
                }
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(format!("cannot finalize descriptor cache: {error}"))
        }
    }
}

pub fn read_reference_features(
    path: &Path,
    expected_id: impl Into<String>,
    expected_class: ReferenceClass,
) -> Result<ReferenceFeatures, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot read descriptor cache {}: {error}", path.display()))?;
    let mut cursor = Cursor::new(bytes.as_slice());
    let mut magic = vec![0_u8; DESCRIPTOR_MAGIC.len()];
    cursor
        .read_exact(&mut magic)
        .map_err(|_| "descriptor cache has a truncated magic header".to_owned())?;
    if magic != DESCRIPTOR_MAGIC {
        return Err("descriptor cache magic does not match".to_owned());
    }
    if read_u32(&mut cursor)? != DESCRIPTOR_VERSION {
        return Err("descriptor cache version is unsupported".to_owned());
    }
    let class = reference_class_from_code(read_u8(&mut cursor)?)?;
    if class != expected_class {
        return Err("descriptor cache class does not match the reference metadata".to_owned());
    }
    let width = read_i32(&mut cursor)?;
    let height = read_i32(&mut cursor)?;
    if width <= 0 || height <= 0 {
        return Err("descriptor cache dimensions are invalid".to_owned());
    }
    let phash = read_u64(&mut cursor)?;
    let mut hue_histogram = [0.0_f32; COLOR_SIGNATURE_HUE_BINS];
    for value in &mut hue_histogram {
        *value = read_f32(&mut cursor)?;
    }
    let low_saturation_fraction = read_f32(&mut cursor)?;
    if hue_histogram
        .iter()
        .chain(std::iter::once(&low_saturation_fraction))
        .any(|value| !value.is_finite() || *value < 0.0)
        || hue_histogram.iter().sum::<f32>() > 1.001
        || low_saturation_fraction > 1.0
    {
        return Err("descriptor cache color signature is invalid".to_owned());
    }
    let color_signature = ColorSignature {
        hue_histogram,
        low_saturation_fraction,
    };
    let keypoint_count = read_u32(&mut cursor)?;
    if keypoint_count > MAX_SERIALIZED_KEYPOINTS {
        return Err("descriptor cache has too many keypoints".to_owned());
    }
    let mut keypoints = Vector::<KeyPoint>::new();
    for _ in 0..keypoint_count {
        let point = Point2f::new(read_f32(&mut cursor)?, read_f32(&mut cursor)?);
        let keypoint = KeyPoint::new_point(
            point,
            read_f32(&mut cursor)?,
            read_f32(&mut cursor)?,
            read_f32(&mut cursor)?,
            read_i32(&mut cursor)?,
            read_i32(&mut cursor)?,
        )
        .map_err(cv_error)?;
        keypoints.push(keypoint);
    }
    let rows = read_i32(&mut cursor)?;
    let cols = read_i32(&mut cursor)?;
    let typ = read_i32(&mut cursor)?;
    let descriptor_len = usize::try_from(read_u64(&mut cursor)?)
        .map_err(|_| "descriptor cache byte length overflows usize".to_owned())?;
    if rows < 0 || cols < 0 || descriptor_len > MAX_DESCRIPTOR_BYTES {
        return Err("descriptor cache matrix metadata is invalid".to_owned());
    }
    let mut descriptor_bytes = vec![0_u8; descriptor_len];
    cursor
        .read_exact(&mut descriptor_bytes)
        .map_err(|_| "descriptor cache has truncated matrix bytes".to_owned())?;
    if cursor.position() != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err("descriptor cache has trailing bytes".to_owned());
    }
    let mut descriptors =
        Mat::new_rows_cols_with_default(rows, cols, typ, Scalar::all(0.0)).map_err(cv_error)?;
    let destination = descriptors.data_bytes_mut().map_err(cv_error)?;
    if destination.len() != descriptor_bytes.len() {
        return Err("descriptor cache matrix byte length does not match its shape".to_owned());
    }
    destination.copy_from_slice(&descriptor_bytes);
    Ok(ReferenceFeatures {
        reference_id: expected_id.into(),
        class,
        phash,
        width,
        height,
        color_signature,
        keypoints,
        descriptors,
    })
}

fn apply_color_compatibility(base_score: f32, similarity: f32) -> f32 {
    // Color is only a negative auxiliary signal. Palette-compatible matches
    // are unchanged; a strongly different palette can suppress a textured
    // false match, but can never create a positive class score by itself.
    if similarity >= COLOR_SIGNATURE_FULL_SIMILARITY {
        return base_score;
    }
    let normalized = (similarity / COLOR_SIGNATURE_FULL_SIMILARITY).clamp(0.0, 1.0);
    let factor = 0.35 + 0.65 * normalized;
    (base_score * factor).clamp(0.0, 1.0)
}

fn reference_class_code(class: ReferenceClass) -> u8 {
    match class {
        ReferenceClass::Nailong => 1,
        ReferenceClass::NaiwaFrog => 2,
    }
}

fn reference_class_from_code(code: u8) -> Result<ReferenceClass, String> {
    match code {
        1 => Ok(ReferenceClass::Nailong),
        2 => Ok(ReferenceClass::NaiwaFrog),
        _ => Err("descriptor cache contains an unknown reference class".to_owned()),
    }
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(bytes: &mut Vec<u8>, value: f32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, String> {
    let mut bytes = [0_u8; 1];
    cursor
        .read_exact(&mut bytes)
        .map_err(|_| "descriptor cache is truncated".to_owned())?;
    Ok(bytes[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, String> {
    let mut bytes = [0_u8; 4];
    cursor
        .read_exact(&mut bytes)
        .map_err(|_| "descriptor cache is truncated".to_owned())?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_i32(cursor: &mut Cursor<&[u8]>) -> Result<i32, String> {
    Ok(i32::from_le_bytes(read_array(cursor)?))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, String> {
    Ok(u64::from_le_bytes(read_array(cursor)?))
}

fn read_f32(cursor: &mut Cursor<&[u8]>) -> Result<f32, String> {
    Ok(f32::from_le_bytes(read_array(cursor)?))
}

fn read_array<const N: usize>(cursor: &mut Cursor<&[u8]>) -> Result<[u8; N], String> {
    let mut bytes = [0_u8; N];
    cursor
        .read_exact(&mut bytes)
        .map_err(|_| "descriptor cache is truncated".to_owned())?;
    Ok(bytes)
}

fn empty_match(
    reference: &ReferenceFeatures,
    query: &QueryFeatures,
    phash_distance: Option<u32>,
) -> MatchResult {
    MatchResult {
        reference_id: reference.reference_id.clone(),
        class: reference.class,
        keypoint_count: query.keypoints.len() as u32,
        good_match_count: 0,
        inlier_count: 0,
        inlier_ratio: 0.0,
        coverage: 0.0,
        reprojection_error: 20.0,
        phash_distance,
        score: 0.0,
    }
}

fn empty_match_without_query(reference: &ReferenceFeatures, phash_distance: u32) -> MatchResult {
    MatchResult {
        reference_id: reference.reference_id.clone(),
        class: reference.class,
        keypoint_count: 0,
        good_match_count: 0,
        inlier_count: 0,
        inlier_ratio: 0.0,
        coverage: 0.0,
        reprojection_error: 20.0,
        phash_distance: Some(phash_distance),
        score: 0.0,
    }
}

fn phash_shortcut_match(reference: &ReferenceFeatures, phash_distance: u32) -> MatchResult {
    // A near-identical pHash is useful for ordinary recognition and avoids a
    // full SIFT pass. It deliberately has no geometric evidence, so the QQ
    // recall gate can never accept this shortcut by itself.
    MatchResult {
        reference_id: reference.reference_id.clone(),
        class: reference.class,
        keypoint_count: 0,
        good_match_count: 0,
        inlier_count: 0,
        inlier_ratio: 0.0,
        coverage: 0.0,
        reprojection_error: 20.0,
        phash_distance: Some(phash_distance),
        score: (0.98 - phash_distance as f32 * 0.01).clamp(0.0, 1.0),
    }
}

trait MatchResultExt {
    fn with_good_matches(self, count: u32) -> Self;
}

impl MatchResultExt for MatchResult {
    fn with_good_matches(mut self, count: u32) -> Self {
        self.good_match_count = count;
        self
    }
}

fn spatial_coverage(points: &Vector<Point2f>, mask: &[u8], width: i32, height: i32) -> f32 {
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    let mut count = 0_u32;
    for (index, point) in points.iter().enumerate() {
        if mask.get(index).copied().unwrap_or(0) == 0 {
            continue;
        }
        min_x = min_x.min(point.x);
        max_x = max_x.max(point.x);
        min_y = min_y.min(point.y);
        max_y = max_y.max(point.y);
        count += 1;
    }
    if count < 2 || width <= 0 || height <= 0 {
        return 0.0;
    }
    let area = (max_x - min_x).max(0.0) * (max_y - min_y).max(0.0);
    (area / (width as f32 * height as f32)).clamp(0.0, 1.0)
}

fn reprojection_error(
    query_points: &Vector<Point2f>,
    reference_points: &Vector<Point2f>,
    mask: &[u8],
    homography: &Mat,
) -> Result<f32, String> {
    let mut transformed = Vector::<Point2f>::new();
    core::perspective_transform(query_points, &mut transformed, homography).map_err(cv_error)?;
    let mut total = 0.0_f32;
    let mut count = 0_u32;
    for index in 0..query_points.len() {
        if mask.get(index).copied().unwrap_or(0) == 0 {
            continue;
        }
        let projected = transformed.get(index).map_err(cv_error)?;
        let expected = reference_points.get(index).map_err(cv_error)?;
        total += (projected.x - expected.x).hypot(projected.y - expected.y);
        count += 1;
    }
    if count == 0 {
        return Ok(20.0);
    }
    Ok(total / count as f32)
}

fn cv_error(error: opencv::Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;
    use std::{collections::HashSet, path::Path};

    use super::{
        apply_color_compatibility, read_reference_features, write_reference_features,
        ColorSignature, OpenCvVisionEngine,
    };
    use crate::{
        decoder::DecodedFrame,
        vision::{ClassificationLabel, ReferenceClass},
    };
    use opencv::prelude::{MatTraitConst, MatTraitConstManual, MatTraitManual};
    use opencv::{
        core::{Mat, Point, Point2f, Rect, Scalar, Size, Vector, CV_8UC3},
        imgcodecs::{imdecode, imencode, IMREAD_COLOR, IMWRITE_JPEG_QUALITY},
        imgproc::{
            gaussian_blur_def, get_rotation_matrix_2d, put_text, resize, warp_affine_def,
            FONT_HERSHEY_SIMPLEX, INTER_LINEAR, LINE_8,
        },
    };
    use serde::Deserialize;

    fn textured_frame() -> DecodedFrame {
        let width = 320_u32;
        let height = 240_u32;
        let mut rgb = vec![24_u8; (width * height * 3) as usize];
        for y in 0..height {
            for x in 0..width {
                let index = ((y * width + x) * 3) as usize;
                let tile = ((x / 24) + (y / 24)) % 2 == 0;
                if tile {
                    rgb[index] = 220;
                    rgb[index + 1] = 190;
                    rgb[index + 2] = 70;
                }
                if (x > 38 && x < 112 && y > 31 && y < 95)
                    || (x > 179 && x < 282 && y > 122 && y < 204)
                {
                    rgb[index] = 242;
                    rgb[index + 1] = 242;
                    rgb[index + 2] = 242;
                }
            }
        }
        DecodedFrame {
            source_index: 0,
            width,
            height,
            rgb,
        }
    }

    fn resize_frame(frame: &DecodedFrame, width: u32, height: u32) -> DecodedFrame {
        let source = frame_to_mat(frame);
        let mut resized = Mat::default();
        resize(
            &source,
            &mut resized,
            Size::new(width as i32, height as i32),
            0.0,
            0.0,
            INTER_LINEAR,
        )
        .unwrap();
        mat_to_frame(&resized)
    }

    fn frame_to_mat(frame: &DecodedFrame) -> Mat {
        let mut source = Mat::new_rows_cols_with_default(
            frame.height as i32,
            frame.width as i32,
            CV_8UC3,
            Scalar::all(0.0),
        )
        .unwrap();
        source.data_bytes_mut().unwrap().copy_from_slice(&frame.rgb);
        source
    }

    fn mat_to_frame(mat: &Mat) -> DecodedFrame {
        DecodedFrame {
            source_index: 0,
            width: mat.cols() as u32,
            height: mat.rows() as u32,
            rgb: mat.data_bytes().unwrap().to_vec(),
        }
    }

    fn rotate_frame(frame: &DecodedFrame, angle: f64) -> DecodedFrame {
        let source = frame_to_mat(frame);
        let center = Point2f::new(frame.width as f32 / 2.0, frame.height as f32 / 2.0);
        let matrix = get_rotation_matrix_2d(center, angle, 1.0).unwrap();
        let mut rotated = Mat::default();
        warp_affine_def(
            &source,
            &mut rotated,
            &matrix,
            Size::new(frame.width as i32, frame.height as i32),
        )
        .unwrap();
        mat_to_frame(&rotated)
    }

    fn crop_frame(frame: &DecodedFrame, rect: Rect) -> DecodedFrame {
        let source = frame_to_mat(frame);
        let cropped = Mat::roi(&source, rect).unwrap().try_clone().unwrap();
        mat_to_frame(&cropped)
    }

    fn jpeg_frame(frame: &DecodedFrame, quality: i32) -> DecodedFrame {
        let source = frame_to_mat(frame);
        let mut params = Vector::<i32>::new();
        params.push(IMWRITE_JPEG_QUALITY);
        params.push(quality);
        let mut encoded = Vector::<u8>::new();
        assert!(imencode(".jpg", &source, &mut encoded, &params).unwrap());
        let encoded_mat = Mat::from_slice(encoded.as_slice()).unwrap();
        let decoded = imdecode(&encoded_mat, IMREAD_COLOR).unwrap();
        mat_to_frame(&decoded)
    }

    fn text_overlay_frame(frame: &DecodedFrame) -> DecodedFrame {
        let mut overlay = frame_to_mat(frame);
        put_text(
            &mut overlay,
            "CHECK",
            Point::new(18, 224),
            FONT_HERSHEY_SIMPLEX,
            0.8,
            Scalar::new(255.0, 255.0, 255.0, 0.0),
            2,
            LINE_8,
            false,
        )
        .unwrap();
        mat_to_frame(&overlay)
    }

    fn blurred_frame(frame: &DecodedFrame) -> DecodedFrame {
        let source = frame_to_mat(frame);
        let mut blurred = Mat::default();
        gaussian_blur_def(&source, &mut blurred, Size::new(3, 3), 0.0).unwrap();
        mat_to_frame(&blurred)
    }

    fn solid_frame(width: u32, height: u32, rgb: [u8; 3]) -> DecodedFrame {
        let mut pixels = vec![0_u8; (width * height * 3) as usize];
        for chunk in pixels.as_chunks_mut::<3>().0 {
            chunk.copy_from_slice(&rgb);
        }
        DecodedFrame {
            source_index: 0,
            width,
            height,
            rgb: pixels,
        }
    }

    #[test]
    fn identical_reference_uses_phash_shortcut_for_ordinary_recognition() {
        let frame = textured_frame();
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-SMOKE", ReferenceClass::Nailong, &frame)
            .unwrap();
        let result = engine.classify_frames(&[frame], &[reference]).unwrap();
        assert_eq!(result.label, ClassificationLabel::Nailong);
        assert!(result.nailong_score >= 0.60);
        // The identical-image path is intentionally allowed to use the pHash
        // shortcut. It is still ineligible for QQ recall without geometry.
        assert!(!result.geometry_valid);
        assert_eq!(result.inlier_count, 0);
    }

    #[test]
    fn sift_and_ransac_run_when_phash_does_not_shortcut() {
        let frame = textured_frame();
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-SMOKE-GEOMETRY", ReferenceClass::Nailong, &frame)
            .unwrap();
        let query = engine
            .extract_query(&frame, reference.phash ^ 0xff)
            .unwrap();
        let result = engine.match_reference(&query, &reference).unwrap();
        assert!(result.inlier_count >= 4);
        assert!(result.inlier_ratio >= 0.55);
        assert!(result.coverage >= 0.20);
    }

    #[test]
    fn sift_and_ransac_survive_a_scaled_query() {
        let reference_frame = textured_frame();
        let query_frame = resize_frame(&reference_frame, 480, 360);
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-SCALED", ReferenceClass::Nailong, &reference_frame)
            .unwrap();
        let query = engine
            .extract_query(&query_frame, reference.phash ^ 0xff)
            .unwrap();
        let result = engine.match_reference(&query, &reference).unwrap();
        assert!(result.good_match_count >= 12);
        assert!(result.inlier_count >= 8);
        assert!(result.inlier_ratio >= 0.55);
        assert!(result.coverage >= 0.15);
        assert!(result.reprojection_error <= 8.0);
    }

    #[test]
    fn sift_and_ransac_survive_common_image_transformations() {
        let reference_frame = textured_frame();
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-TRANSFORMS", ReferenceClass::Nailong, &reference_frame)
            .unwrap();
        let transformed = [
            ("rotate", rotate_frame(&reference_frame, 8.0)),
            (
                "crop",
                crop_frame(&reference_frame, Rect::new(24, 18, 272, 204)),
            ),
            ("jpeg", jpeg_frame(&reference_frame, 35)),
            ("text", text_overlay_frame(&reference_frame)),
            ("blur", blurred_frame(&reference_frame)),
        ];
        for (name, frame) in transformed {
            let query = engine
                .extract_query(&frame, reference.phash ^ 0xff)
                .unwrap();
            let result = engine.match_reference(&query, &reference).unwrap();
            eprintln!(
                "transform={name} good={} inliers={} ratio={:.3} coverage={:.3} score={:.3}",
                result.good_match_count,
                result.inlier_count,
                result.inlier_ratio,
                result.coverage,
                result.score
            );
            assert!(
                result.inlier_count >= 6,
                "{name} lost too much geometric evidence: {result:?}"
            );
            assert!(
                result.score >= 0.45,
                "{name} score became too weak: {result:?}"
            );
        }
    }

    #[test]
    fn unrelated_texture_does_not_create_a_recall_grade_match() {
        let reference_frame = textured_frame();
        let mut unrelated = reference_frame.clone();
        for (index, value) in unrelated.rgb.iter_mut().enumerate() {
            let mixed = (index as u32)
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            *value = (mixed >> 24) as u8;
        }
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-NEGATIVE", ReferenceClass::Nailong, &reference_frame)
            .unwrap();
        let query = engine
            .extract_query(&unrelated, reference.phash ^ 0xff)
            .unwrap();
        let result = engine.match_reference(&query, &reference).unwrap();
        assert!(result.inlier_count < 12);
        assert!(result.score < 0.60);
    }

    #[test]
    fn color_signature_separates_yellow_and_blue_palettes() {
        let yellow_frame = solid_frame(4, 4, [250, 210, 40]);
        let blue_frame = solid_frame(4, 4, [40, 120, 245]);
        let yellow =
            ColorSignature::from_rgb(&yellow_frame.rgb, yellow_frame.width, yellow_frame.height)
                .unwrap();
        let blue =
            ColorSignature::from_rgb(&blue_frame.rgb, blue_frame.width, blue_frame.height).unwrap();
        assert!((yellow.similarity(yellow) - 1.0).abs() < f32::EPSILON);
        assert!(yellow.similarity(blue) < 0.2);
    }

    #[test]
    fn color_compatibility_only_penalizes_strongly_different_palettes() {
        assert!((apply_color_compatibility(0.84, 1.0) - 0.84).abs() < f32::EPSILON);
        assert!(apply_color_compatibility(0.84, 0.0) < 0.60);
    }

    #[cfg(windows)]
    #[test]
    fn animated_reference_uses_the_query_sampling_policy_for_phash() {
        let Some(path) = std::env::var_os("NLNF_SMOKE_NAILONG") else {
            eprintln!("set NLNF_SMOKE_NAILONG to run the animated reference pHash test");
            return;
        };
        let bytes = std::fs::read(path).expect("read animated reference");
        let root = std::env::temp_dir().join(format!(
            "nlnf-animated-reference-phash-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let manager = crate::references::ReferenceManager::new(&root);
        let asset = manager
            .add(ReferenceClass::Nailong, &bytes)
            .expect("add animated reference");
        let decoded = crate::decoder::decode_image(&bytes, crate::image_policy::MAX_SAMPLE_FRAMES)
            .expect("decode animated reference with query policy");
        let frame = decoded.frames.first().expect("animated reference frame");
        let expected = crate::phash::compute_rgb(&frame.rgb, frame.width, frame.height)
            .expect("compute animated reference pHash");
        assert_eq!(asset.phash, format!("{expected:016x}"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn real_local_reference_smoke_uses_explicit_environment_paths() {
        let Some(nailong_path) = std::env::var_os("NLNF_SMOKE_NAILONG") else {
            eprintln!(
                "set NLNF_SMOKE_NAILONG and NLNF_SMOKE_NAIWA_FROG to run the real-image smoke test"
            );
            return;
        };
        let Some(frog_path) = std::env::var_os("NLNF_SMOKE_NAIWA_FROG") else {
            eprintln!(
                "set NLNF_SMOKE_NAILONG and NLNF_SMOKE_NAIWA_FROG to run the real-image smoke test"
            );
            return;
        };
        let nailong_bytes = std::fs::read(nailong_path).expect("read nailong smoke image");
        let frog_bytes = std::fs::read(frog_path).expect("read frog smoke image");
        let nailong = crate::decoder::decode_image(&nailong_bytes, 12).expect("decode nailong");
        let frog = crate::decoder::decode_image(&frog_bytes, 12).expect("decode frog");
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let nailong_reference = engine
            .extract_reference("NL-REAL-SMOKE", ReferenceClass::Nailong, &nailong.frames[0])
            .unwrap();
        let frog_reference = engine
            .extract_reference("NF-REAL-SMOKE", ReferenceClass::NaiwaFrog, &frog.frames[0])
            .unwrap();
        let references = vec![nailong_reference, frog_reference];
        let nailong_result = engine
            .classify_frames(&nailong.frames, &references)
            .expect("classify nailong smoke image");
        let frog_result = engine
            .classify_frames(&frog.frames, &references)
            .expect("classify frog smoke image");
        eprintln!(
            "real smoke: nailong={:?} ({:.3}) frog={:?} ({:.3})",
            nailong_result.label,
            nailong_result.nailong_score,
            frog_result.label,
            frog_result.naiwa_frog_score
        );
        assert_eq!(nailong_result.label, ClassificationLabel::Nailong);
        assert_eq!(frog_result.label, ClassificationLabel::NaiwaFrog);
        assert!(nailong_result.nailong_score >= 0.60);
        assert!(frog_result.naiwa_frog_score >= 0.60);

        let mut static_durations = Vec::with_capacity(5);
        for _ in 0..5 {
            let started = Instant::now();
            engine
                .classify_frames(&[nailong.frames[0].clone()], &references)
                .expect("benchmark static real smoke image");
            static_durations.push(started.elapsed());
        }
        static_durations.sort_unstable();
        let static_p95_index = ((static_durations.len() * 95).div_ceil(100)).saturating_sub(1);
        let static_p95 = static_durations[static_p95_index];
        eprintln!(
            "real static SIFT p95: {} ms",
            static_p95.as_secs_f64() * 1000.0
        );
        assert!(
            static_p95.as_millis() < 200,
            "real static p95 exceeded 200 ms: {static_p95:?}"
        );

        if let Some(other_path) = std::env::var_os("NLNF_SMOKE_OTHER") {
            let other_bytes = std::fs::read(other_path).expect("read other smoke image");
            let other = crate::decoder::decode_image(&other_bytes, 12).expect("decode other");
            let other_result = engine
                .classify_frames(&other.frames, &references)
                .expect("classify other smoke image");
            eprintln!("real smoke: other={:?}", other_result.label);
            assert!(matches!(
                other_result.label,
                ClassificationLabel::Other | ClassificationLabel::Unknown
            ));
        }

        if let Some(animated_path) = std::env::var_os("NLNF_SMOKE_ANIMATED") {
            let animated_bytes = std::fs::read(animated_path).expect("read animated smoke image");
            let animated =
                crate::decoder::decode_image(&animated_bytes, 12).expect("decode animation");
            assert!(animated.inspection.animated);
            assert!(animated.frames.len() >= 2);
            let animated_result = engine
                .classify_frames(&animated.frames, &references)
                .expect("classify animated smoke image");
            eprintln!(
                "animated smoke: frames={} sampled={} frog_qualifying={} label={:?}",
                animated.inspection.frame_count,
                animated_result.sampled_frame_count,
                animated_result.qualifying_naiwa_frame_count,
                animated_result.label
            );
            assert!(animated_result.sampled_frame_count <= 12);
            let mut durations = Vec::with_capacity(5);
            for _ in 0..5 {
                let started = Instant::now();
                engine
                    .classify_frames(&animated.frames, &references)
                    .expect("benchmark animated smoke image");
                durations.push(started.elapsed());
            }
            durations.sort_unstable();
            let p95_index = ((durations.len() * 95).div_ceil(100)).saturating_sub(1);
            let p95 = durations[p95_index];
            eprintln!("animated SIFT p95: {} ms", p95.as_secs_f64() * 1000.0);
            assert!(
                p95.as_millis() < 1_000,
                "animated p95 exceeded 1000 ms: {p95:?}"
            );
        }
    }

    #[test]
    fn sift_static_pipeline_stays_below_p95_budget() {
        let reference_frame = textured_frame();
        let query_frame = resize_frame(&reference_frame, 480, 360);
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-BENCHMARK", ReferenceClass::Nailong, &reference_frame)
            .unwrap();
        let mut durations = Vec::with_capacity(20);
        for _ in 0..20 {
            let started = Instant::now();
            let query = engine
                .extract_query(&query_frame, reference.phash ^ 0xff)
                .unwrap();
            let result = engine.match_reference(&query, &reference).unwrap();
            assert!(result.inlier_count >= 8);
            durations.push(started.elapsed());
        }
        durations.sort_unstable();
        let p95_index = ((durations.len() * 95).div_ceil(100)).saturating_sub(1);
        let p95 = durations[p95_index];
        eprintln!(
            "synthetic SIFT static p95: {} ms",
            p95.as_secs_f64() * 1000.0
        );
        assert!(
            p95.as_millis() < 200,
            "static SIFT p95 exceeded 200 ms: {p95:?}"
        );
    }

    #[test]
    fn descriptor_cache_round_trips_keypoints_and_matrix() {
        let frame = textured_frame();
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-CACHE", ReferenceClass::Nailong, &frame)
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "nlnf-descriptor-test-{}-{}.desc",
            std::process::id(),
            reference.reference_id
        ));
        let _ = std::fs::remove_file(&path);
        write_reference_features(&path, &reference).unwrap();
        let loaded = read_reference_features(&path, "NL-CACHE", ReferenceClass::Nailong).unwrap();
        assert_eq!(loaded.phash, reference.phash);
        assert_eq!(loaded.keypoints.len(), reference.keypoints.len());
        assert_eq!(loaded.descriptors.rows(), reference.descriptors.rows());
        assert_eq!(loaded.descriptors.cols(), reference.descriptors.cols());
        assert_eq!(
            loaded.descriptors.data_bytes().unwrap(),
            reference.descriptors.data_bytes().unwrap()
        );
        assert_eq!(loaded.color_signature, reference.color_signature);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn debug_overlay_contains_a_png_with_geometric_points() {
        let frame = textured_frame();
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let reference = engine
            .extract_reference("NL-DEBUG", ReferenceClass::Nailong, &frame)
            .unwrap();
        let overlay = engine
            .render_match_debug(&frame, &frame, &reference)
            .unwrap();
        assert!(overlay.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(overlay.len() > 1_000);
    }

    #[cfg(windows)]
    #[derive(Debug, Deserialize)]
    struct ValidationRow {
        source_relative_path: String,
        label: String,
    }

    #[cfg(windows)]
    const RELEASE_MIN_NAILONG: usize = 100;
    #[cfg(windows)]
    const RELEASE_MIN_NAIWA_FROG: usize = 100;
    #[cfg(windows)]
    const RELEASE_MIN_OTHER: usize = 1_000;
    #[cfg(windows)]
    const RELEASE_MIN_GIF: usize = 50;

    #[cfg(windows)]
    fn release_gate_requested() -> bool {
        matches!(
            std::env::var("NLNF_REQUIRE_RELEASE_GATE").as_deref(),
            Ok("1") | Ok("true") | Ok("TRUE")
        )
    }

    #[cfg(windows)]
    fn validation_path(root: &Path, relative_path: &str) -> std::path::PathBuf {
        let relative = Path::new(relative_path);
        assert!(
            !relative.is_absolute()
                && !relative
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir)),
            "validation manifest paths must stay below the validation root: {relative_path}"
        );
        root.join(relative)
    }

    #[cfg(windows)]
    #[test]
    fn local_manifest_validation_reports_false_recall_safety() {
        let (Some(root), Some(manifest), Some(nailong_path), Some(frog_path)) = (
            std::env::var_os("NLNF_VALIDATION_ROOT"),
            std::env::var_os("NLNF_VALIDATION_MANIFEST"),
            std::env::var_os("NLNF_SMOKE_NAILONG"),
            std::env::var_os("NLNF_SMOKE_NAIWA_FROG"),
        ) else {
            eprintln!(
                "set NLNF_VALIDATION_ROOT, NLNF_VALIDATION_MANIFEST and both smoke references to run the local validation"
            );
            return;
        };
        let rows = std::fs::read_to_string(manifest)
            .expect("read validation manifest")
            .lines()
            .map(|line| serde_json::from_str::<ValidationRow>(line).expect("parse validation row"))
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        let rows = rows
            .into_iter()
            .filter(|row| seen.insert(row.source_relative_path.clone()))
            .collect::<Vec<_>>();
        let expected_nailong = rows.iter().filter(|row| row.label == "NAILONG").count();
        let expected_frog = rows.iter().filter(|row| row.label == "NAIWA_FROG").count();
        let expected_other = rows.iter().filter(|row| row.label == "OTHER").count();
        assert!(
            rows.iter()
                .all(|row| matches!(row.label.as_str(), "NAILONG" | "NAIWA_FROG" | "OTHER")),
            "validation manifest contains an unsupported label"
        );
        let unique_row_count = rows.len();
        if release_gate_requested() {
            assert!(
                expected_nailong >= RELEASE_MIN_NAILONG,
                "release validation needs at least {RELEASE_MIN_NAILONG} NAILONG rows, got {expected_nailong}"
            );
            assert!(
                expected_frog >= RELEASE_MIN_NAIWA_FROG,
                "release validation needs at least {RELEASE_MIN_NAIWA_FROG} NAIWA_FROG rows, got {expected_frog}"
            );
            assert!(
                expected_other >= RELEASE_MIN_OTHER,
                "release validation needs at least {RELEASE_MIN_OTHER} OTHER rows, got {expected_other}"
            );
        }
        let nailong_bytes = std::fs::read(nailong_path).expect("read nailong reference");
        let frog_bytes = std::fs::read(frog_path).expect("read frog reference");
        let nailong = crate::decoder::decode_image(&nailong_bytes, 12).expect("decode nailong");
        let frog = crate::decoder::decode_image(&frog_bytes, 12).expect("decode frog");
        let mut engine = OpenCvVisionEngine::new(Default::default()).unwrap();
        let references = vec![
            engine
                .extract_reference("NL-VALIDATION", ReferenceClass::Nailong, &nailong.frames[0])
                .unwrap(),
            engine
                .extract_reference("NF-VALIDATION", ReferenceClass::NaiwaFrog, &frog.frames[0])
                .unwrap(),
        ];
        let mut processed = 0_u32;
        let mut skipped = 0_u32;
        let mut false_target_label = 0_u32;
        let mut false_recall = 0_u32;
        let mut correct_nailong = 0_u32;
        let mut correct_frog = 0_u32;
        let mut incorrect_target = 0_u32;
        let mut other_or_unknown = 0_u32;
        let mut gif_count = 0_u32;
        let mut false_positive_paths = Vec::new();
        let started = Instant::now();
        for row in rows {
            let path = validation_path(Path::new(&root), &row.source_relative_path);
            if !path.is_file() {
                skipped += 1;
                continue;
            }
            let bytes = std::fs::read(&path).expect("read validation image");
            let decoded =
                crate::decoder::decode_image(&bytes, 12).expect("decode validation image");
            if decoded.inspection.format == crate::image_policy::ImageFormat::Gif {
                gif_count += 1;
            }
            let result = engine
                .classify_frames(&decoded.frames, &references)
                .expect("classify validation image");
            processed += 1;
            match row.label.as_str() {
                "NAILONG" if result.label == ClassificationLabel::Nailong => correct_nailong += 1,
                "NAIWA_FROG" if result.label == ClassificationLabel::NaiwaFrog => correct_frog += 1,
                "NAILONG" | "NAIWA_FROG" => incorrect_target += 1,
                "OTHER"
                    if !matches!(
                        result.label,
                        ClassificationLabel::Nailong | ClassificationLabel::NaiwaFrog
                    ) =>
                {
                    other_or_unknown += 1
                }
                "OTHER" => {
                    false_target_label += 1;
                    if crate::vision::recall_eligible(&result, Default::default()) {
                        false_recall += 1;
                    }
                    if false_positive_paths.len() < 20 {
                        false_positive_paths.push(format!(
                            "{} => {:?} nailong={:.3} frog={:.3} inliers={} ratio={:.3} coverage={:.3} reproj={:.2}",
                            row.source_relative_path,
                            result.label,
                            result.nailong_score,
                            result.naiwa_frog_score,
                            result.inlier_count,
                            result.inlier_ratio,
                            result.coverage,
                            result.reprojection_error
                        ));
                    }
                }
                _ => {}
            }
        }
        let elapsed = started.elapsed();
        eprintln!(
            "local validation: processed={} skipped={} nailong_correct={}/{} frog_correct={}/{} other_or_unknown={} gif_count={} incorrect_target={} false_target_label={} false_recall={} elapsed_ms={:.1}",
            processed,
            skipped,
            correct_nailong,
            expected_nailong,
            correct_frog,
            expected_frog,
            other_or_unknown,
            gif_count,
            incorrect_target,
            false_target_label,
            false_recall,
            elapsed.as_secs_f64() * 1000.0
        );
        for path in false_positive_paths {
            eprintln!("false positive: {path}");
        }
        assert!(
            processed >= 100,
            "validation processed too few images: {processed}"
        );
        assert_eq!(
            false_recall, 0,
            "negative samples met the strict recall gate"
        );
        if release_gate_requested() {
            assert_eq!(
                skipped, 0,
                "release validation cannot contain missing files: {skipped} skipped"
            );
            assert_eq!(
                processed, unique_row_count as u32,
                "release validation did not process every unique manifest row"
            );
            assert_eq!(
                correct_nailong, expected_nailong as u32,
                "release validation contains a NAILONG false negative"
            );
            assert_eq!(
                correct_frog, expected_frog as u32,
                "release validation contains a NAIWA_FROG false negative"
            );
            assert_eq!(
                incorrect_target, 0,
                "release validation classified a target row as the wrong target"
            );
            assert_eq!(
                false_target_label, 0,
                "release validation classified an OTHER row as a target"
            );
            assert!(
                gif_count as usize >= RELEASE_MIN_GIF,
                "release validation needs at least {RELEASE_MIN_GIF} decoded GIF files, got {gif_count}"
            );
        }
    }
}
