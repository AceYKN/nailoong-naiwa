//! Build-time validation evidence used to keep the recall side effect closed.
//!
//! A normal build has no embedded certificate.  A release-feature build must
//! receive the compact JSON certificate produced by the local validation gate;
//! runtime code then checks that certificate against the current Reference
//! Bank before allowing `AUTO_RECALL`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::vision::VisionThresholds;

pub const VALIDATION_CERTIFICATE_SCHEMA_VERSION: u32 = 2;
pub const EXPECTED_NATIVE_RUNTIME_NAME: &str = "opencv_world4130";
pub const RELEASE_MIN_NAILONG: u64 = 100;
pub const RELEASE_MIN_NAIWA_FROG: u64 = 100;
pub const RELEASE_MIN_OTHER: u64 = 1_000;
pub const RELEASE_MIN_GIF: u64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidationCertificate {
    pub schema_version: u32,
    pub git_sha: String,
    pub reference_set_sha256: String,
    pub validation_manifest_sha256: String,
    pub descriptor_fingerprint: String,
    pub engine_fingerprint: String,
    pub vision_pipeline_version: String,
    pub native_runtime_name: String,
    pub native_runtime_sha256: String,
    pub thresholds_sha256: String,
    pub min_recall_threshold: f64,
    pub nailong_rows: u64,
    pub naiwa_frog_rows: u64,
    pub other_rows: u64,
    pub gif_rows: u64,
    pub false_target_label: u64,
    pub false_recall: u64,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Hash a sorted set of `(class, image_sha256)` entries without including
/// machine-specific paths.  The same representation is used by the local
/// validation gate and the runtime SQLite Reference Bank.
pub fn reference_set_hash(entries: impl IntoIterator<Item = (String, String)>) -> String {
    let mut entries = entries
        .into_iter()
        .map(|(class, sha256)| (class, sha256.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    entries.sort();

    let mut hasher = Sha256::new();
    for (class, sha256) in entries {
        hasher.update(class.as_bytes());
        hasher.update([0]);
        hasher.update(sha256.as_bytes());
        hasher.update([0]);
    }
    hex::encode(hasher.finalize())
}

/// Fingerprint every threshold that affects the deterministic decision layer.
/// Group-specific recall thresholds are checked separately against the
/// certificate's `min_recall_threshold`.
pub fn thresholds_sha256(thresholds: VisionThresholds) -> String {
    let payload = format!(
        "match_threshold={:.9}\nother_threshold={:.9}\nrecall_threshold={:.9}\nmin_margin={:.9}\nmin_recall_margin={:.9}\nmin_recall_inliers={}\nmin_recall_ratio={:.9}\nmin_recall_coverage={:.9}\nmax_recall_reprojection_error={:.9}\n",
        thresholds.match_threshold,
        thresholds.other_threshold,
        thresholds.recall_threshold,
        thresholds.min_margin,
        thresholds.min_recall_margin,
        thresholds.min_recall_inliers,
        thresholds.min_recall_ratio,
        thresholds.min_recall_coverage,
        thresholds.max_recall_reprojection_error,
    );
    sha256_hex(payload.as_bytes())
}

pub fn embedded_certificate() -> Option<ValidationCertificate> {
    option_env!("NLNF_VALIDATION_CERTIFICATE_JSON")
        .and_then(|value| serde_json::from_str(value.trim()).ok())
}

pub fn certificate_is_well_formed(certificate: &ValidationCertificate) -> bool {
    let defaults = VisionThresholds::default();
    certificate.schema_version == VALIDATION_CERTIFICATE_SCHEMA_VERSION
        && is_git_revision(&certificate.git_sha)
        && is_sha256(&certificate.reference_set_sha256)
        && is_sha256(&certificate.validation_manifest_sha256)
        && is_sha256(&certificate.descriptor_fingerprint)
        && is_sha256(&certificate.engine_fingerprint)
        && certificate.vision_pipeline_version == crate::vision::VISION_PIPELINE_VERSION
        && certificate.native_runtime_name == EXPECTED_NATIVE_RUNTIME_NAME
        && is_sha256(&certificate.native_runtime_sha256)
        && certificate
            .thresholds_sha256
            .eq_ignore_ascii_case(&thresholds_sha256(defaults))
        && certificate.min_recall_threshold.is_finite()
        && (defaults.recall_threshold as f64..=1.0).contains(&certificate.min_recall_threshold)
        && certificate.nailong_rows >= RELEASE_MIN_NAILONG
        && certificate.naiwa_frog_rows >= RELEASE_MIN_NAIWA_FROG
        && certificate.other_rows >= RELEASE_MIN_OTHER
        && certificate.gif_rows >= RELEASE_MIN_GIF
        && certificate.false_target_label == 0
        && certificate.false_recall == 0
}

pub fn certificate_matches_reference_set(
    current_reference_set_hash: &str,
    current_descriptor_fingerprint: &str,
    current_engine_fingerprint: &str,
    recall_threshold: Option<f64>,
) -> bool {
    let Some(certificate) = embedded_certificate() else {
        return false;
    };
    let Some(build_git_sha) = option_env!("NLNF_BUILD_GIT_SHA") else {
        return false;
    };
    let Some(native_runtime_sha256) = option_env!("NLNF_OPENCV_RUNTIME_SHA256") else {
        return false;
    };
    let Some(native_runtime_name) = option_env!("NLNF_OPENCV_RUNTIME_NAME") else {
        return false;
    };
    certificate_is_well_formed(&certificate)
        && certificate.git_sha.eq_ignore_ascii_case(build_git_sha)
        && certificate
            .reference_set_sha256
            .eq_ignore_ascii_case(current_reference_set_hash)
        && certificate
            .descriptor_fingerprint
            .eq_ignore_ascii_case(current_descriptor_fingerprint)
        && certificate
            .engine_fingerprint
            .eq_ignore_ascii_case(current_engine_fingerprint)
        && certificate
            .native_runtime_name
            .eq_ignore_ascii_case(native_runtime_name)
        && certificate
            .native_runtime_sha256
            .eq_ignore_ascii_case(native_runtime_sha256)
        && recall_threshold.is_none_or(|threshold| {
            threshold.is_finite() && threshold >= certificate.min_recall_threshold
        })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{
        certificate_is_well_formed, reference_set_hash, thresholds_sha256, ValidationCertificate,
        EXPECTED_NATIVE_RUNTIME_NAME, VALIDATION_CERTIFICATE_SCHEMA_VERSION,
    };
    use crate::vision::VisionThresholds;

    fn valid_certificate() -> ValidationCertificate {
        ValidationCertificate {
            schema_version: VALIDATION_CERTIFICATE_SCHEMA_VERSION,
            git_sha: "a".repeat(40),
            reference_set_sha256: reference_set_hash(vec![
                ("NAILONG".to_owned(), "a".repeat(64)),
                ("NAIWA_FROG".to_owned(), "b".repeat(64)),
            ]),
            validation_manifest_sha256: "c".repeat(64),
            descriptor_fingerprint: "d".repeat(64),
            engine_fingerprint: "e".repeat(64),
            vision_pipeline_version: crate::vision::VISION_PIPELINE_VERSION.to_owned(),
            native_runtime_name: EXPECTED_NATIVE_RUNTIME_NAME.to_owned(),
            native_runtime_sha256: "f".repeat(64),
            thresholds_sha256: thresholds_sha256(VisionThresholds::default()),
            min_recall_threshold: f64::from(VisionThresholds::default().recall_threshold),
            nailong_rows: 100,
            naiwa_frog_rows: 100,
            other_rows: 1_000,
            gif_rows: 50,
            false_target_label: 0,
            false_recall: 0,
        }
    }

    #[test]
    fn reference_hash_is_order_independent_but_class_sensitive() {
        let first = reference_set_hash(vec![
            ("NAILONG".to_owned(), "a".repeat(64)),
            ("NAIWA_FROG".to_owned(), "b".repeat(64)),
        ]);
        let reordered = reference_set_hash(vec![
            ("NAIWA_FROG".to_owned(), "b".repeat(64)),
            ("NAILONG".to_owned(), "a".repeat(64)),
        ]);
        let relabeled = reference_set_hash(vec![
            ("NAILONG".to_owned(), "b".repeat(64)),
            ("NAIWA_FROG".to_owned(), "a".repeat(64)),
        ]);
        assert_eq!(first, reordered);
        assert_ne!(first, relabeled);
    }

    #[test]
    fn certificate_requires_zero_negative_recall_evidence() {
        let mut certificate = valid_certificate();
        assert!(certificate_is_well_formed(&certificate));
        certificate.false_recall = 1;
        assert!(!certificate_is_well_formed(&certificate));
    }

    #[test]
    fn certificate_git_sha_is_required_to_be_a_real_revision() {
        let mut certificate = valid_certificate();
        certificate.git_sha.clear();
        assert!(!certificate_is_well_formed(&certificate));
        certificate.git_sha = "not-a-commit".to_owned();
        assert!(!certificate_is_well_formed(&certificate));
        certificate.git_sha = "a".repeat(40);
        assert!(certificate_is_well_formed(&certificate));
    }

    #[test]
    fn certificate_requires_the_pinned_runtime_identity() {
        let mut certificate = valid_certificate();
        certificate.native_runtime_name = "opencv_world9999".to_owned();
        assert!(!certificate_is_well_formed(&certificate));
        certificate.native_runtime_name = EXPECTED_NATIVE_RUNTIME_NAME.to_owned();
        certificate.native_runtime_sha256 = "not-a-sha".to_owned();
        assert!(!certificate_is_well_formed(&certificate));
    }
}
