//! Training-free reference matching decision rules.
//!
//! OpenCV is responsible for producing `MatchResult` values. This module is
//! intentionally independent from the OpenCV binding so the safety-critical
//! scoring, ambiguity handling, and QQ recall gates remain deterministic and
//! unit-testable on every platform.

use serde::Serialize;

pub const MAX_REFERENCES_PER_CLASS: usize = 10;
pub const MIN_ORDINARY_GEOMETRIC_INLIERS: u32 = 6;
pub const MAX_ORDINARY_PHASH_SHORTCUT_DISTANCE: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReferenceClass {
    Nailong,
    NaiwaFrog,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClassificationLabel {
    Nailong,
    NaiwaFrog,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConfidenceLevel {
    None,
    Low,
    Medium,
    High,
    VeryHigh,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchResult {
    pub reference_id: String,
    pub class: ReferenceClass,
    pub keypoint_count: u32,
    pub good_match_count: u32,
    pub inlier_count: u32,
    pub inlier_ratio: f32,
    pub coverage: f32,
    pub reprojection_error: f32,
    pub phash_distance: Option<u32>,
    pub score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisionThresholds {
    pub match_threshold: f32,
    pub other_threshold: f32,
    pub recall_threshold: f32,
    pub min_margin: f32,
    pub min_recall_margin: f32,
    pub min_recall_inliers: u32,
    pub min_recall_ratio: f32,
    pub min_recall_coverage: f32,
    pub max_recall_reprojection_error: f32,
}

impl Default for VisionThresholds {
    fn default() -> Self {
        Self {
            match_threshold: 0.60,
            other_threshold: 0.25,
            recall_threshold: 0.85,
            min_margin: 0.15,
            min_recall_margin: 0.20,
            min_recall_inliers: 12,
            min_recall_ratio: 0.55,
            min_recall_coverage: 0.20,
            max_recall_reprojection_error: 8.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassificationResult {
    pub label: ClassificationLabel,
    pub nailong_score: f32,
    pub naiwa_frog_score: f32,
    pub best_nailong_reference: Option<String>,
    pub best_naiwa_reference: Option<String>,
    pub best_match: Option<MatchResult>,
    pub inlier_count: u32,
    pub inlier_ratio: f32,
    pub coverage: f32,
    pub reprojection_error: f32,
    pub confidence_level: ConfidenceLevel,
    pub geometry_valid: bool,
    pub sampled_frame_count: u32,
    pub qualifying_naiwa_frame_count: u32,
}

pub fn validate_reference_count(count: usize) -> Result<(), String> {
    if (1..=MAX_REFERENCES_PER_CLASS).contains(&count) {
        Ok(())
    } else {
        Err(format!(
            "每类参考图数量必须在 1 到 {} 张之间",
            MAX_REFERENCES_PER_CLASS
        ))
    }
}

pub fn score_match(
    good_match_count: u32,
    inlier_count: u32,
    inlier_ratio: f32,
    coverage: f32,
    reprojection_error: f32,
    phash_distance: Option<u32>,
) -> Result<f32, String> {
    if good_match_count == 0 || inlier_count > good_match_count {
        return Err("good matches must be positive and contain all inliers".to_owned());
    }
    if !inlier_ratio.is_finite()
        || !(0.0..=1.0).contains(&inlier_ratio)
        || !coverage.is_finite()
        || !(0.0..=1.0).contains(&coverage)
        || !reprojection_error.is_finite()
        || reprojection_error < 0.0
    {
        return Err("match geometry metrics are invalid".to_owned());
    }
    let match_strength = (good_match_count as f32 / 30.0).min(1.0);
    let reprojection_quality = (1.0 - reprojection_error / 20.0).clamp(0.0, 1.0);
    let phash_quality = phash_distance
        .map(|distance| (1.0 - distance as f32 / 64.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    Ok((0.25 * match_strength
        + 0.35 * inlier_ratio
        + 0.20 * coverage
        + 0.10 * reprojection_quality
        + 0.10 * phash_quality)
        .clamp(0.0, 1.0))
}

pub fn classify(
    matches: &[MatchResult],
    thresholds: VisionThresholds,
    sampled_frame_count: u32,
) -> Result<ClassificationResult, String> {
    if sampled_frame_count == 0 {
        return Err("classification requires at least one sampled frame".to_owned());
    }
    validate_thresholds(thresholds)?;
    let best_nailong = best_for_class(matches, ReferenceClass::Nailong);
    let best_naiwa = best_for_class(matches, ReferenceClass::NaiwaFrog);
    let nailong_score = best_nailong.map_or(0.0, |value| value.score);
    let naiwa_score = best_naiwa.map_or(0.0, |value| value.score);
    let (winner, loser, best_match) = if nailong_score >= naiwa_score {
        (ReferenceClass::Nailong, naiwa_score, best_nailong)
    } else {
        (ReferenceClass::NaiwaFrog, nailong_score, best_naiwa)
    };
    let winner_score = best_match.map_or(0.0, |value| value.score);
    let margin = winner_score - loser;
    let label = if winner_score >= thresholds.match_threshold
        && margin >= thresholds.min_margin
        && best_match.is_some_and(ordinary_evidence)
    {
        match winner {
            ReferenceClass::Nailong => ClassificationLabel::Nailong,
            ReferenceClass::NaiwaFrog => ClassificationLabel::NaiwaFrog,
        }
    } else if nailong_score < thresholds.other_threshold
        && naiwa_score < thresholds.other_threshold
        && best_nailong.is_none_or(|value| value.inlier_count == 0)
        && best_naiwa.is_none_or(|value| value.inlier_count == 0)
    {
        ClassificationLabel::Other
    } else {
        ClassificationLabel::Unknown
    };
    let geometry_valid = best_match.is_some_and(|value| {
        value.inlier_count > 0
            && value.inlier_ratio >= thresholds.min_recall_ratio
            && value.coverage >= thresholds.min_recall_coverage
            && value.reprojection_error <= thresholds.max_recall_reprojection_error
    });
    let confidence_level = confidence_level(winner_score, margin, geometry_valid, thresholds);
    Ok(ClassificationResult {
        label,
        nailong_score,
        naiwa_frog_score: naiwa_score,
        best_nailong_reference: best_nailong.map(|value| value.reference_id.clone()),
        best_naiwa_reference: best_naiwa.map(|value| value.reference_id.clone()),
        inlier_count: best_match.map_or(0, |value| value.inlier_count),
        inlier_ratio: best_match.map_or(0.0, |value| value.inlier_ratio),
        coverage: best_match.map_or(0.0, |value| value.coverage),
        reprojection_error: best_match.map_or(f32::INFINITY, |value| value.reprojection_error),
        best_match: best_match.cloned(),
        confidence_level,
        geometry_valid,
        sampled_frame_count,
        qualifying_naiwa_frame_count: if sampled_frame_count == 1
            && label == ClassificationLabel::NaiwaFrog
        {
            1
        } else {
            0
        },
    })
}

fn ordinary_evidence(value: &MatchResult) -> bool {
    value.inlier_count >= MIN_ORDINARY_GEOMETRIC_INLIERS
        || (value.good_match_count == 0
            && value
                .phash_distance
                .is_some_and(|distance| distance <= MAX_ORDINARY_PHASH_SHORTCUT_DISTANCE))
}

/// Classify a sampled animation while retaining frame-level evidence for the
/// stricter QQ gate. The aggregate label uses the strongest reference match,
/// but an animated frog recall requires two independently qualifying frames.
pub fn classify_frames(
    frame_matches: &[Vec<MatchResult>],
    thresholds: VisionThresholds,
) -> Result<ClassificationResult, String> {
    if frame_matches.is_empty() {
        return Err("classification requires at least one sampled frame".to_owned());
    }
    let flattened = frame_matches
        .iter()
        .flat_map(|matches| matches.iter().cloned())
        .collect::<Vec<_>>();
    let sampled_frame_count = u32::try_from(frame_matches.len())
        .map_err(|_| "sampled frame count overflows u32".to_owned())?;
    let mut result = classify(&flattened, thresholds, sampled_frame_count)?;
    result.qualifying_naiwa_frame_count = frame_matches
        .iter()
        .filter(|matches| {
            classify(matches, thresholds, 1)
                .map(|frame| strict_naiwa_frame_eligible(&frame, thresholds))
                .unwrap_or(false)
        })
        .count() as u32;
    Ok(result)
}

fn strict_naiwa_frame_eligible(
    result: &ClassificationResult,
    thresholds: VisionThresholds,
) -> bool {
    result.label == ClassificationLabel::NaiwaFrog
        && result.naiwa_frog_score >= thresholds.recall_threshold
        && result.naiwa_frog_score - result.nailong_score >= thresholds.min_recall_margin
        && result.inlier_count >= thresholds.min_recall_inliers
        && result.inlier_ratio >= thresholds.min_recall_ratio
        && result.coverage >= thresholds.min_recall_coverage
        && result.reprojection_error <= thresholds.max_recall_reprojection_error
        && result.confidence_level == ConfidenceLevel::VeryHigh
}

pub fn recall_eligible(result: &ClassificationResult, thresholds: VisionThresholds) -> bool {
    let required_frames = if result.sampled_frame_count > 1 { 2 } else { 1 };
    strict_naiwa_frame_eligible(result, thresholds)
        && result.qualifying_naiwa_frame_count >= required_frames
}

fn best_for_class(matches: &[MatchResult], class: ReferenceClass) -> Option<&MatchResult> {
    matches
        .iter()
        .filter(|value| value.class == class)
        .max_by(|left, right| left.score.total_cmp(&right.score))
}

fn confidence_level(
    score: f32,
    margin: f32,
    geometry_valid: bool,
    thresholds: VisionThresholds,
) -> ConfidenceLevel {
    if score < thresholds.match_threshold || margin < thresholds.min_margin {
        return ConfidenceLevel::None;
    }
    if score >= thresholds.recall_threshold
        && margin >= thresholds.min_recall_margin
        && geometry_valid
    {
        ConfidenceLevel::VeryHigh
    } else if score >= 0.75 {
        ConfidenceLevel::High
    } else if score >= 0.65 {
        ConfidenceLevel::Medium
    } else {
        ConfidenceLevel::Low
    }
}

fn validate_thresholds(thresholds: VisionThresholds) -> Result<(), String> {
    for (name, value) in [
        ("match_threshold", thresholds.match_threshold),
        ("other_threshold", thresholds.other_threshold),
        ("recall_threshold", thresholds.recall_threshold),
        ("min_margin", thresholds.min_margin),
        ("min_recall_margin", thresholds.min_recall_margin),
        ("min_recall_ratio", thresholds.min_recall_ratio),
        ("min_recall_coverage", thresholds.min_recall_coverage),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(format!("{name} must be finite and within [0, 1]"));
        }
    }
    if !thresholds.max_recall_reprojection_error.is_finite()
        || thresholds.max_recall_reprojection_error < 0.0
        || thresholds.other_threshold >= thresholds.match_threshold
        || thresholds.recall_threshold <= thresholds.match_threshold
        || thresholds.min_recall_margin < thresholds.min_margin
    {
        return Err("unsafe vision threshold ordering".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        classify, classify_frames, recall_eligible, score_match, validate_reference_count,
        ClassificationLabel, ConfidenceLevel, MatchResult, ReferenceClass, VisionThresholds,
    };

    fn match_result(class: ReferenceClass, score: f32) -> MatchResult {
        MatchResult {
            reference_id: format!("{class:?}"),
            class,
            keypoint_count: 100,
            good_match_count: 30,
            inlier_count: 20,
            inlier_ratio: 0.67,
            coverage: 0.35,
            reprojection_error: 2.0,
            phash_distance: Some(2),
            score,
        }
    }

    #[test]
    fn reference_count_accepts_one_to_ten_only() {
        assert!(validate_reference_count(1).is_ok());
        assert!(validate_reference_count(10).is_ok());
        assert!(validate_reference_count(0).is_err());
        assert!(validate_reference_count(11).is_err());
    }

    #[test]
    fn score_combines_match_and_geometry_metrics() {
        let score = score_match(30, 20, 0.67, 0.35, 2.0, Some(2)).unwrap();
        assert!(score > 0.5 && score < 1.0);
        assert!(score_match(0, 0, 0.0, 0.0, 0.0, None).is_err());
    }

    #[test]
    fn ambiguous_winner_is_unknown() {
        let thresholds = VisionThresholds::default();
        let result = classify(
            &[
                match_result(ReferenceClass::Nailong, 0.72),
                match_result(ReferenceClass::NaiwaFrog, 0.70),
            ],
            thresholds,
            1,
        )
        .unwrap();
        assert_eq!(result.label, ClassificationLabel::Unknown);
        assert_eq!(result.confidence_level, ConfidenceLevel::None);
        assert!(!recall_eligible(&result, thresholds));
    }

    #[test]
    fn ordinary_classification_rejects_a_high_score_with_too_few_inliers() {
        let mut weak = match_result(ReferenceClass::Nailong, 0.82);
        weak.good_match_count = 4;
        weak.inlier_count = 4;
        weak.phash_distance = Some(32);
        let result = classify(&[weak], VisionThresholds::default(), 1).unwrap();
        assert_eq!(result.label, ClassificationLabel::Unknown);
    }

    #[test]
    fn ordinary_classification_allows_a_near_identical_phash_shortcut() {
        let mut shortcut = match_result(ReferenceClass::Nailong, 0.98);
        shortcut.good_match_count = 0;
        shortcut.inlier_count = 0;
        shortcut.phash_distance = Some(2);
        let result = classify(&[shortcut], VisionThresholds::default(), 1).unwrap();
        assert_eq!(result.label, ClassificationLabel::Nailong);
    }

    #[test]
    fn weak_nonmatching_evidence_is_other() {
        let thresholds = VisionThresholds::default();
        let mut nailong = match_result(ReferenceClass::Nailong, 0.10);
        nailong.good_match_count = 1;
        nailong.inlier_count = 0;
        nailong.inlier_ratio = 0.0;
        nailong.coverage = 0.0;
        nailong.reprojection_error = 20.0;
        let mut frog = nailong.clone();
        frog.class = ReferenceClass::NaiwaFrog;
        let result = classify(&[nailong, frog], thresholds, 1).unwrap();
        assert_eq!(result.label, ClassificationLabel::Other);
    }

    #[test]
    fn very_high_frog_match_passes_recall_gates() {
        let thresholds = VisionThresholds::default();
        let result = classify(
            &[
                match_result(ReferenceClass::Nailong, 0.20),
                match_result(ReferenceClass::NaiwaFrog, 0.96),
            ],
            thresholds,
            1,
        )
        .unwrap();
        assert_eq!(result.label, ClassificationLabel::NaiwaFrog);
        assert_eq!(result.confidence_level, ConfidenceLevel::VeryHigh);
        assert!(recall_eligible(&result, thresholds));
    }

    #[test]
    fn animated_frog_requires_two_strictly_qualifying_frames() {
        let thresholds = VisionThresholds::default();
        let strong = vec![
            match_result(ReferenceClass::Nailong, 0.20),
            match_result(ReferenceClass::NaiwaFrog, 0.96),
        ];
        let weak = vec![match_result(ReferenceClass::Nailong, 0.20), {
            let mut result = match_result(ReferenceClass::NaiwaFrog, 0.96);
            result.inlier_count = thresholds.min_recall_inliers - 1;
            result
        }];
        let two_strong = classify_frames(&[strong.clone(), strong.clone()], thresholds).unwrap();
        assert_eq!(two_strong.qualifying_naiwa_frame_count, 2);
        assert!(recall_eligible(&two_strong, thresholds));

        let one_strong = classify_frames(&[strong, weak], thresholds).unwrap();
        assert_eq!(one_strong.qualifying_naiwa_frame_count, 1);
        assert!(!recall_eligible(&one_strong, thresholds));
    }
}
