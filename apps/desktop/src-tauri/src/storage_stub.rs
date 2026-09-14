//! Non-Windows compile-time boundary.
//!
//! The shipped target is Windows and uses the real `winsqlite3.dll` wrapper
//! in `storage.rs`. Other targets compile the Tauri shell and fail closed if
//! the app tries to initialize persistence; they must not silently substitute
//! a different storage format.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "SQLite storage unavailable on this target: {}",
            self.message
        )
    }
}

impl std::error::Error for StorageError {}

#[derive(Debug, Clone)]
pub struct QQGroupRecord {
    pub group_id: String,
    pub group_name: String,
    pub mode: String,
    pub recall_threshold: f64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ModerationLogRecord {
    pub group_id: Option<String>,
    pub message_id: Option<String>,
    pub user_id: Option<String>,
    pub image_sha256: Option<String>,
    pub nailong_score: Option<f64>,
    pub naiwa_frog_score: Option<f64>,
    pub reference_set_version: Option<u64>,
    pub classification_label: Option<String>,
    pub decision: String,
    pub action_result: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceRecord {
    pub id: String,
    pub class: String,
    pub file_path: String,
    pub sha256: String,
    pub phash: String,
    pub descriptor_path: Option<String>,
    pub width: u32,
    pub height: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PredictionCacheRecord {
    pub image_sha256: String,
    pub reference_set_version: u64,
    pub engine_fingerprint: String,
    pub label: String,
    pub nailong_score: f64,
    pub naiwa_frog_score: f64,
    pub confidence_level: String,
    pub classification_json: Option<String>,
    pub source: String,
    pub created_at: String,
}

pub struct AppDatabase {
    path: PathBuf,
}

impl AppDatabase {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Err(Self::unsupported(path.as_ref()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> u32 {
        4
    }

    pub fn reference_set_version(&self) -> Result<u64, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn reference_set_hash(&self) -> Result<String, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn verified_reference_set_hash(&self) -> Result<String, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn count_references(&self, _: &str) -> Result<u64, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn list_references(&self) -> Result<Vec<ReferenceRecord>, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn record_reference(&self, _: &ReferenceRecord) -> Result<(), StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn delete_reference_and_bump_version(&self, _: &str) -> Result<u64, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn record_reference_and_bump_version(
        &self,
        _: &ReferenceRecord,
    ) -> Result<u64, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn claim_moderation_message(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<bool, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn update_moderation_message_state(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<(), StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn record_prediction_cache(&self, _: &PredictionCacheRecord) -> Result<(), StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn find_prediction_cache(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<Option<PredictionCacheRecord>, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn upsert_qq_group(&self, _: &QQGroupRecord) -> Result<(), StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn list_qq_groups(&self) -> Result<Vec<QQGroupRecord>, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn find_qq_group(&self, _: &str) -> Result<Option<QQGroupRecord>, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn append_moderation_log(&self, _: &ModerationLogRecord) -> Result<(), StorageError> {
        Err(Self::unsupported(&self.path))
    }

    pub fn recent_moderation_logs(
        &self,
        _: usize,
    ) -> Result<Vec<ModerationLogRecord>, StorageError> {
        Err(Self::unsupported(&self.path))
    }

    fn unsupported(path: &Path) -> StorageError {
        StorageError {
            code: -1,
            message: format!(
                "winsqlite3 is required for local database: {}",
                path.display()
            ),
        }
    }
}
