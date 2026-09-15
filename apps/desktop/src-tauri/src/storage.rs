//! Small Windows SQLite boundary for the desktop app.
//!
//! The target is Windows 10/11, where `winsqlite3.dll` is available as a
//! system component. Keeping this wrapper small avoids a Python dependency in
//! the shipped app and keeps all SQL behind one typed boundary.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use crate::vision::VisionThresholds;

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;
const SQLITE_SCHEMA_VERSION: u32 = 5;

#[repr(C)]
struct sqlite3 {
    _private: [u8; 0],
}

#[repr(C)]
struct sqlite3_stmt {
    _private: [u8; 0],
}

type SqliteCallback =
    unsafe extern "C" fn(*mut c_void, c_int, *mut *mut c_char, *mut *mut c_char) -> c_int;

#[link(name = "winsqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        database: *mut *mut sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(database: *mut sqlite3) -> c_int;
    fn sqlite3_errmsg(database: *mut sqlite3) -> *const c_char;
    fn sqlite3_exec(
        database: *mut sqlite3,
        sql: *const c_char,
        callback: Option<SqliteCallback>,
        argument: *mut c_void,
        error_message: *mut *mut c_char,
    ) -> c_int;
    fn sqlite3_free(value: *mut c_void);
    fn sqlite3_prepare_v2(
        database: *mut sqlite3,
        sql: *const c_char,
        byte_count: c_int,
        statement: *mut *mut sqlite3_stmt,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_step(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_finalize(statement: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_text(statement: *mut sqlite3_stmt, column: c_int) -> *const c_char;
    fn sqlite3_column_double(statement: *mut sqlite3_stmt, column: c_int) -> f64;
    fn sqlite3_bind_text(
        statement: *mut sqlite3_stmt,
        index: c_int,
        value: *const c_char,
        byte_count: c_int,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int;
    fn sqlite3_bind_int64(statement: *mut sqlite3_stmt, index: c_int, value: i64) -> c_int;
    fn sqlite3_bind_double(statement: *mut sqlite3_stmt, index: c_int, value: f64) -> c_int;
    fn sqlite3_bind_null(statement: *mut sqlite3_stmt, index: c_int) -> c_int;
    fn sqlite3_column_int64(statement: *mut sqlite3_stmt, column: c_int) -> i64;
    fn sqlite3_changes(database: *mut sqlite3) -> c_int;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "SQLite error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for StorageError {}

impl From<std::ffi::NulError> for StorageError {
    fn from(error: std::ffi::NulError) -> Self {
        Self {
            code: -1,
            message: format!("SQLite text contains NUL: {error}"),
        }
    }
}

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
    raw: *mut sqlite3,
    path: PathBuf,
}

impl AppDatabase {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().expanduser().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| StorageError {
                code: -1,
                message: format!("cannot create database directory: {error}"),
            })?;
        }
        let filename = CString::new(path.to_string_lossy().as_bytes())?;
        let mut raw = std::ptr::null_mut();
        let code = unsafe {
            sqlite3_open_v2(
                filename.as_ptr(),
                &mut raw,
                SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_FULLMUTEX,
                std::ptr::null(),
            )
        };
        if code != SQLITE_OK {
            let error = Self::error_from_raw(raw, code, "cannot open database");
            if !raw.is_null() {
                unsafe { sqlite3_close(raw) };
            }
            return Err(error);
        }
        let database = Self { raw, path };
        database.initialize_schema()?;
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn schema_version(&self) -> u32 {
        SQLITE_SCHEMA_VERSION
    }

    pub fn reference_set_version(&self) -> Result<u64, StorageError> {
        let value =
            self.query_text("SELECT value FROM settings WHERE key='reference_set_version'")?;
        value.parse::<u64>().map_err(|_| StorageError {
            code: -1,
            message: "reference_set_version is not a valid unsigned integer".to_owned(),
        })
    }

    pub fn builtin_reference_bank_seeded(&self) -> Result<bool, StorageError> {
        Ok(self
            .setting_value("builtin_reference_bank_seeded")?
            .as_deref()
            == Some("1"))
    }

    pub fn mark_builtin_reference_bank_seeded(&self) -> Result<(), StorageError> {
        self.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            |statement| {
                bind_text(statement, 1, "builtin_reference_bank_seeded")?;
                bind_text(statement, 2, "1")
            },
        )
    }

    pub fn app_settings(&self) -> Result<(VisionThresholds, bool), StorageError> {
        let defaults = VisionThresholds::default();
        let thresholds = VisionThresholds {
            match_threshold: self
                .setting_f32("vision.match_threshold", defaults.match_threshold)?,
            other_threshold: self
                .setting_f32("vision.other_threshold", defaults.other_threshold)?,
            recall_threshold: self
                .setting_f32("vision.recall_threshold", defaults.recall_threshold)?,
            min_margin: self.setting_f32("vision.min_margin", defaults.min_margin)?,
            min_recall_margin: self
                .setting_f32("vision.min_recall_margin", defaults.min_recall_margin)?,
            min_recall_inliers: self
                .setting_u32("vision.min_recall_inliers", defaults.min_recall_inliers)?,
            min_recall_ratio: self
                .setting_f32("vision.min_recall_ratio", defaults.min_recall_ratio)?,
            min_recall_coverage: self
                .setting_f32("vision.min_recall_coverage", defaults.min_recall_coverage)?,
            max_recall_reprojection_error: self.setting_f32(
                "vision.max_recall_reprojection_error",
                defaults.max_recall_reprojection_error,
            )?,
        };
        crate::vision::validate_thresholds(thresholds).map_err(|message| StorageError {
            code: -1,
            message: format!("stored vision settings are invalid: {message}"),
        })?;
        let developer_mode = match self.setting_value("developer_mode")? {
            None => false,
            Some(value) if value == "1" => true,
            Some(value) if value == "0" => false,
            Some(value) => {
                return Err(StorageError {
                    code: -1,
                    message: format!("developer_mode must be 0 or 1, got {value}"),
                });
            }
        };
        Ok((thresholds, developer_mode))
    }

    pub fn save_app_settings(
        &self,
        thresholds: VisionThresholds,
        developer_mode: bool,
    ) -> Result<(), StorageError> {
        crate::vision::validate_thresholds(thresholds).map_err(|message| StorageError {
            code: -1,
            message: format!("invalid vision settings: {message}"),
        })?;
        let thresholds_changed = self.app_settings()?.0 != thresholds;
        let values = [
            (
                "vision.match_threshold",
                format!("{:.9}", thresholds.match_threshold),
            ),
            (
                "vision.other_threshold",
                format!("{:.9}", thresholds.other_threshold),
            ),
            (
                "vision.recall_threshold",
                format!("{:.9}", thresholds.recall_threshold),
            ),
            ("vision.min_margin", format!("{:.9}", thresholds.min_margin)),
            (
                "vision.min_recall_margin",
                format!("{:.9}", thresholds.min_recall_margin),
            ),
            (
                "vision.min_recall_inliers",
                thresholds.min_recall_inliers.to_string(),
            ),
            (
                "vision.min_recall_ratio",
                format!("{:.9}", thresholds.min_recall_ratio),
            ),
            (
                "vision.min_recall_coverage",
                format!("{:.9}", thresholds.min_recall_coverage),
            ),
            (
                "vision.max_recall_reprojection_error",
                format!("{:.9}", thresholds.max_recall_reprojection_error),
            ),
            (
                "developer_mode",
                if developer_mode { "1" } else { "0" }.to_owned(),
            ),
        ];
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            for (key, value) in values {
                self.execute(
                    "INSERT INTO settings(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                    |statement| {
                        bind_text(statement, 1, key)?;
                        bind_text(statement, 2, &value)
                    },
                )?;
            }
            if thresholds_changed {
                self.downgrade_auto_recall_groups()?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => self.exec("COMMIT"),
            Err(error) => {
                let _ = self.exec("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn reference_set_hash(&self) -> Result<String, StorageError> {
        let entries = self
            .list_references()?
            .into_iter()
            .map(|record| (record.class, record.sha256))
            .collect::<Vec<_>>();
        Ok(crate::release::reference_set_hash(entries))
    }

    /// Hash the bytes currently present on disk, not only the database's
    /// declared hashes. This is the value that may cross the AUTO_RECALL
    /// safety boundary.
    pub fn verified_reference_set_hash(&self) -> Result<String, StorageError> {
        let mut entries = Vec::new();
        for record in self.list_references()? {
            let bytes = std::fs::read(&record.file_path).map_err(|error| StorageError {
                code: -1,
                message: format!("cannot read reference {}: {error}", record.id),
            })?;
            let actual_sha256 = crate::release::sha256_hex(&bytes);
            if !actual_sha256.eq_ignore_ascii_case(&record.sha256) {
                return Err(StorageError {
                    code: -1,
                    message: format!(
                        "reference {} bytes do not match the stored SHA-256; refusing to use the Reference Bank",
                        record.id
                    ),
                });
            }
            entries.push((record.class, actual_sha256));
        }
        Ok(crate::release::reference_set_hash(entries))
    }

    pub fn count_references(&self, class: &str) -> Result<u64, StorageError> {
        validate_reference_class(class)?;
        let statement =
            Statement::prepare(self, "SELECT COUNT(*) FROM reference_images WHERE class=?1")?;
        bind_text(statement.raw, 1, class)?;
        let code = unsafe { sqlite3_step(statement.raw) };
        if code != SQLITE_ROW {
            return Err(self.error(code, "reference count query failed"));
        }
        let count = unsafe { sqlite3_column_int64(statement.raw, 0) };
        u64::try_from(count).map_err(|_| StorageError {
            code: -1,
            message: "reference count is negative".to_owned(),
        })
    }

    pub fn list_references(&self) -> Result<Vec<ReferenceRecord>, StorageError> {
        let statement = Statement::prepare(
            self,
            "SELECT id, class, file_path, sha256, phash, descriptor_path, width, height, created_at FROM reference_images ORDER BY class, id",
        )?;
        let mut records = Vec::new();
        loop {
            match unsafe { sqlite3_step(statement.raw) } {
                SQLITE_ROW => {
                    let width = u32::try_from(unsafe { sqlite3_column_int64(statement.raw, 6) })
                        .map_err(|_| StorageError {
                            code: -1,
                            message: "reference width is invalid".to_owned(),
                        })?;
                    let height = u32::try_from(unsafe { sqlite3_column_int64(statement.raw, 7) })
                        .map_err(|_| StorageError {
                        code: -1,
                        message: "reference height is invalid".to_owned(),
                    })?;
                    records.push(ReferenceRecord {
                        id: column_text(statement.raw, 0)?,
                        class: column_text(statement.raw, 1)?,
                        file_path: column_text(statement.raw, 2)?,
                        sha256: column_text(statement.raw, 3)?,
                        phash: column_text(statement.raw, 4)?,
                        descriptor_path: column_optional_text(statement.raw, 5),
                        width,
                        height,
                        created_at: column_text(statement.raw, 8)?,
                    });
                }
                SQLITE_DONE => break,
                code => return Err(self.error(code, "reference list query failed")),
            }
        }
        Ok(records)
    }

    pub fn record_reference(&self, record: &ReferenceRecord) -> Result<(), StorageError> {
        validate_reference_class(&record.class)?;
        validate_sha256_text(&record.sha256, "reference sha256")?;
        validate_hex_text(&record.phash, 16, "reference phash")?;
        if record.id.trim().is_empty()
            || record.file_path.trim().is_empty()
            || record.created_at.trim().is_empty()
            || record.width == 0
            || record.height == 0
        {
            return Err(StorageError {
                code: -1,
                message: "reference metadata is incomplete".to_owned(),
            });
        }
        self.execute(
            "INSERT INTO reference_images(id, class, file_path, sha256, phash, descriptor_path, width, height, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(id) DO UPDATE SET class=excluded.class, file_path=excluded.file_path, sha256=excluded.sha256, phash=excluded.phash, descriptor_path=excluded.descriptor_path, width=excluded.width, height=excluded.height",
            |statement| {
                bind_text(statement, 1, &record.id)?;
                bind_text(statement, 2, &record.class)?;
                bind_text(statement, 3, &record.file_path)?;
                bind_text(statement, 4, &record.sha256)?;
                bind_text(statement, 5, &record.phash)?;
                bind_optional_text(statement, 6, record.descriptor_path.as_deref())?;
                bind_int64(statement, 7, i64::from(record.width))?;
                bind_int64(statement, 8, i64::from(record.height))?;
                bind_text(statement, 9, &record.created_at)
            },
        )
    }

    pub fn delete_reference_and_bump_version(&self, id: &str) -> Result<u64, StorageError> {
        if id.trim().is_empty() {
            return Err(StorageError {
                code: -1,
                message: "reference id cannot be empty".to_owned(),
            });
        }
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            self.execute("DELETE FROM reference_images WHERE id=?1", |statement| {
                bind_text(statement, 1, id)
            })?;
            if unsafe { sqlite3_changes(self.raw) } != 1 {
                return Err(StorageError {
                    code: -1,
                    message: "reference was not found".to_owned(),
                });
            }
            self.execute(
                "UPDATE settings SET value=CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key='reference_set_version'",
                |_| Ok(()),
            )?;
            self.downgrade_auto_recall_groups()?;
            self.reference_set_version()
        })();
        match result {
            Ok(version) => match self.exec("COMMIT") {
                Ok(()) => Ok(version),
                Err(error) => {
                    let _ = self.exec("ROLLBACK");
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.exec("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Persist a reference and advance the bank version as one SQLite
    /// transaction. The caller may already have written the image file; a
    /// database failure therefore remains recoverable through its cleanup
    /// path, while the database can never expose a new reference with the old
    /// version.
    pub fn record_reference_and_bump_version(
        &self,
        record: &ReferenceRecord,
    ) -> Result<u64, StorageError> {
        self.exec("BEGIN IMMEDIATE")?;
        let result = (|| {
            self.record_reference(record)?;
            self.execute(
                "UPDATE settings SET value=CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key='reference_set_version'",
                |_| Ok(()),
            )?;
            self.downgrade_auto_recall_groups()?;
            self.reference_set_version()
        })();
        match result {
            Ok(version) => match self.exec("COMMIT") {
                Ok(()) => Ok(version),
                Err(error) => {
                    let _ = self.exec("ROLLBACK");
                    Err(error)
                }
            },
            Err(error) => {
                let _ = self.exec("ROLLBACK");
                Err(error)
            }
        }
    }

    pub fn claim_moderation_message(
        &self,
        group_id: &str,
        message_id: &str,
        created_at: &str,
    ) -> Result<bool, StorageError> {
        if group_id.trim().is_empty() || message_id.trim().is_empty() {
            return Err(StorageError {
                code: -1,
                message: "moderation group_id and message_id cannot be empty".to_owned(),
            });
        }
        self.execute(
            "INSERT INTO moderation_messages(group_id, message_id, state, created_at, updated_at) VALUES(?1, ?2, 'SEEN', ?3, ?3) ON CONFLICT(group_id, message_id) DO NOTHING",
            |statement| {
                bind_text(statement, 1, group_id)?;
                bind_text(statement, 2, message_id)?;
                bind_text(statement, 3, created_at)
            },
        )?;
        Ok(unsafe { sqlite3_changes(self.raw) } == 1)
    }

    pub fn update_moderation_message_state(
        &self,
        group_id: &str,
        message_id: &str,
        state: &str,
        updated_at: &str,
    ) -> Result<(), StorageError> {
        if group_id.trim().is_empty() || message_id.trim().is_empty() {
            return Err(StorageError {
                code: -1,
                message: "moderation group_id and message_id cannot be empty".to_owned(),
            });
        }
        validate_moderation_message_state(state)?;
        self.execute(
            "UPDATE moderation_messages SET state=?3, updated_at=?4 WHERE group_id=?1 AND message_id=?2",
            |statement| {
                bind_text(statement, 1, group_id)?;
                bind_text(statement, 2, message_id)?;
                bind_text(statement, 3, state)?;
                bind_text(statement, 4, updated_at)
            },
        )
    }

    pub fn record_prediction_cache(
        &self,
        record: &PredictionCacheRecord,
    ) -> Result<(), StorageError> {
        validate_sha256_text(&record.image_sha256, "prediction image sha256")?;
        validate_label(&record.label)?;
        validate_confidence(&record.confidence_level)?;
        validate_sha256_text(&record.engine_fingerprint, "prediction engine fingerprint")?;
        if let Some(classification_json) = &record.classification_json {
            if classification_json.trim().is_empty() {
                return Err(StorageError {
                    code: -1,
                    message: "prediction classification JSON cannot be empty".to_owned(),
                });
            }
        }
        validate_score(record.nailong_score, "nailong_score")?;
        validate_score(record.naiwa_frog_score, "naiwa_frog_score")?;
        let version = i64::try_from(record.reference_set_version).map_err(|_| StorageError {
            code: -1,
            message: "reference_set_version exceeds SQLite integer range".to_owned(),
        })?;
        self.execute(
            "INSERT INTO prediction_cache(image_sha256, reference_set_version, engine_fingerprint, label, nailong_score, naiwa_frog_score, confidence_level, classification_json, source, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(image_sha256, reference_set_version, engine_fingerprint) DO UPDATE SET label=excluded.label, nailong_score=excluded.nailong_score, naiwa_frog_score=excluded.naiwa_frog_score, confidence_level=excluded.confidence_level, classification_json=excluded.classification_json, source=excluded.source, created_at=excluded.created_at",
            |statement| {
                bind_text(statement, 1, &record.image_sha256)?;
                bind_int64(statement, 2, version)?;
                bind_text(statement, 3, &record.engine_fingerprint)?;
                bind_text(statement, 4, &record.label)?;
                bind_double(statement, 5, record.nailong_score)?;
                bind_double(statement, 6, record.naiwa_frog_score)?;
                bind_text(statement, 7, &record.confidence_level)?;
                bind_optional_text(statement, 8, record.classification_json.as_deref())?;
                bind_text(statement, 9, &record.source)?;
                bind_text(statement, 10, &record.created_at)
            },
        )
    }

    pub fn find_prediction_cache(
        &self,
        image_sha256: &str,
        reference_set_version: u64,
        engine_fingerprint: &str,
    ) -> Result<Option<PredictionCacheRecord>, StorageError> {
        validate_sha256_text(image_sha256, "prediction image sha256")?;
        validate_sha256_text(engine_fingerprint, "prediction engine fingerprint")?;
        let version = i64::try_from(reference_set_version).map_err(|_| StorageError {
            code: -1,
            message: "reference_set_version exceeds SQLite integer range".to_owned(),
        })?;
        let statement = Statement::prepare(
            self,
            "SELECT image_sha256, reference_set_version, engine_fingerprint, label, nailong_score, naiwa_frog_score, confidence_level, classification_json, source, created_at FROM prediction_cache WHERE image_sha256=?1 AND reference_set_version=?2 AND engine_fingerprint=?3",
        )?;
        bind_text(statement.raw, 1, image_sha256)?;
        bind_int64(statement.raw, 2, version)?;
        bind_text(statement.raw, 3, engine_fingerprint)?;
        match unsafe { sqlite3_step(statement.raw) } {
            SQLITE_ROW => Ok(Some(PredictionCacheRecord {
                image_sha256: column_text(statement.raw, 0)?,
                reference_set_version: u64::try_from(unsafe {
                    sqlite3_column_int64(statement.raw, 1)
                })
                .map_err(|_| StorageError {
                    code: -1,
                    message: "cached reference_set_version is negative".to_owned(),
                })?,
                engine_fingerprint: column_text(statement.raw, 2)?,
                label: column_text(statement.raw, 3)?,
                nailong_score: unsafe { sqlite3_column_double(statement.raw, 4) },
                naiwa_frog_score: unsafe { sqlite3_column_double(statement.raw, 5) },
                confidence_level: column_text(statement.raw, 6)?,
                classification_json: column_optional_text(statement.raw, 7),
                source: column_text(statement.raw, 8)?,
                created_at: column_text(statement.raw, 9)?,
            })),
            SQLITE_DONE => Ok(None),
            code => Err(self.error(code, "prediction cache query failed")),
        }
    }

    pub fn upsert_qq_group(&self, record: &QQGroupRecord) -> Result<(), StorageError> {
        if !matches!(record.mode.as_str(), "OFF" | "OBSERVE" | "AUTO_RECALL") {
            return Err(StorageError {
                code: -1,
                message: format!("invalid QQ group mode: {}", record.mode),
            });
        }
        validate_score(record.recall_threshold, "recall_threshold")?;
        self.execute(
            "INSERT INTO qq_groups(group_id, group_name, mode, recall_threshold, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(group_id) DO UPDATE SET group_name=excluded.group_name, mode=excluded.mode, recall_threshold=excluded.recall_threshold, updated_at=excluded.updated_at",
            |statement| {
                bind_text(statement, 1, &record.group_id)?;
                bind_text(statement, 2, &record.group_name)?;
                bind_text(statement, 3, &record.mode)?;
                bind_double(statement, 4, record.recall_threshold)?;
                bind_text(statement, 5, &record.created_at)?;
                bind_text(statement, 6, &record.updated_at)
            },
        )
    }

    /// Persistently close the recall side effect whenever a value bound into
    /// the release certificate changes. Callers invoke this inside the same
    /// transaction as the reference or threshold mutation.
    fn downgrade_auto_recall_groups(&self) -> Result<(), StorageError> {
        self.execute(
            "UPDATE qq_groups SET mode='OBSERVE', updated_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE mode='AUTO_RECALL'",
            |_| Ok(()),
        )
    }

    pub fn list_qq_groups(&self) -> Result<Vec<QQGroupRecord>, StorageError> {
        let statement = Statement::prepare(
            self,
            "SELECT group_id, group_name, mode, recall_threshold, created_at, updated_at FROM qq_groups ORDER BY group_id",
        )?;
        let mut records = Vec::new();
        loop {
            match unsafe { sqlite3_step(statement.raw) } {
                SQLITE_ROW => records.push(QQGroupRecord {
                    group_id: column_text(statement.raw, 0)?,
                    group_name: column_text(statement.raw, 1)?,
                    mode: column_text(statement.raw, 2)?,
                    recall_threshold: unsafe { sqlite3_column_double(statement.raw, 3) },
                    created_at: column_text(statement.raw, 4)?,
                    updated_at: column_text(statement.raw, 5)?,
                }),
                SQLITE_DONE => break,
                code => return Err(self.error(code, "QQ group list query failed")),
            }
        }
        Ok(records)
    }

    pub fn find_qq_group(&self, group_id: &str) -> Result<Option<QQGroupRecord>, StorageError> {
        if group_id.trim().is_empty() {
            return Err(StorageError {
                code: -1,
                message: "QQ group id cannot be empty".to_owned(),
            });
        }
        let statement = Statement::prepare(
            self,
            "SELECT group_id, group_name, mode, recall_threshold, created_at, updated_at FROM qq_groups WHERE group_id=?1",
        )?;
        bind_text(statement.raw, 1, group_id)?;
        match unsafe { sqlite3_step(statement.raw) } {
            SQLITE_ROW => Ok(Some(QQGroupRecord {
                group_id: column_text(statement.raw, 0)?,
                group_name: column_text(statement.raw, 1)?,
                mode: column_text(statement.raw, 2)?,
                recall_threshold: unsafe { sqlite3_column_double(statement.raw, 3) },
                created_at: column_text(statement.raw, 4)?,
                updated_at: column_text(statement.raw, 5)?,
            })),
            SQLITE_DONE => Ok(None),
            code => Err(self.error(code, "QQ group lookup failed")),
        }
    }

    pub fn append_moderation_log(&self, record: &ModerationLogRecord) -> Result<(), StorageError> {
        if let Some(score) = record.nailong_score {
            validate_score(score, "nailong_score")?;
        }
        if let Some(score) = record.naiwa_frog_score {
            validate_score(score, "naiwa_frog_score")?;
        }
        if let Some(label) = &record.classification_label {
            validate_label(label)?;
        }
        self.execute(
            "INSERT INTO moderation_log(group_id, message_id, user_id, image_sha256, nailong_score, naiwa_frog_score, reference_set_version, classification_label, decision, action_result, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            |statement| {
                bind_optional_text(statement, 1, record.group_id.as_deref())?;
                bind_optional_text(statement, 2, record.message_id.as_deref())?;
                bind_optional_text(statement, 3, record.user_id.as_deref())?;
                bind_optional_text(statement, 4, record.image_sha256.as_deref())?;
                bind_optional_double(statement, 5, record.nailong_score)?;
                bind_optional_double(statement, 6, record.naiwa_frog_score)?;
                bind_optional_int64(statement, 7, record.reference_set_version)?;
                bind_optional_text(statement, 8, record.classification_label.as_deref())?;
                bind_text(statement, 9, &record.decision)?;
                bind_optional_text(statement, 10, record.action_result.as_deref())?;
                bind_text(statement, 11, &record.created_at)
            },
        )
    }

    pub fn recent_moderation_logs(
        &self,
        limit: usize,
    ) -> Result<Vec<ModerationLogRecord>, StorageError> {
        let limit = i64::try_from(limit.clamp(1, 1000)).map_err(|_| StorageError {
            code: -1,
            message: "moderation log limit is out of range".to_owned(),
        })?;
        let statement = Statement::prepare(
            self,
            "SELECT group_id, message_id, user_id, image_sha256, nailong_score, naiwa_frog_score, reference_set_version, classification_label, decision, action_result, created_at FROM moderation_log ORDER BY id DESC LIMIT ?1",
        )?;
        bind_int64(statement.raw, 1, limit)?;
        let mut records = Vec::new();
        loop {
            match unsafe { sqlite3_step(statement.raw) } {
                SQLITE_ROW => records.push(ModerationLogRecord {
                    group_id: column_optional_text(statement.raw, 0),
                    message_id: column_optional_text(statement.raw, 1),
                    user_id: column_optional_text(statement.raw, 2),
                    image_sha256: column_optional_text(statement.raw, 3),
                    nailong_score: column_optional_text(statement.raw, 4)
                        .and_then(|value| value.parse::<f64>().ok()),
                    naiwa_frog_score: column_optional_text(statement.raw, 5)
                        .and_then(|value| value.parse::<f64>().ok()),
                    reference_set_version: column_optional_text(statement.raw, 6)
                        .and_then(|value| value.parse::<u64>().ok()),
                    classification_label: column_optional_text(statement.raw, 7),
                    decision: column_text(statement.raw, 8)?,
                    action_result: column_optional_text(statement.raw, 9),
                    created_at: column_text(statement.raw, 10)?,
                }),
                SQLITE_DONE => break,
                code => return Err(self.error(code, "moderation log query failed")),
            }
        }
        Ok(records)
    }

    fn initialize_schema(&self) -> Result<(), StorageError> {
        self.exec(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS qq_groups (
                 group_id TEXT PRIMARY KEY,
                 group_name TEXT NOT NULL,
                 mode TEXT NOT NULL CHECK(mode IN ('OFF', 'OBSERVE', 'AUTO_RECALL')),
                 recall_threshold REAL NOT NULL,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             );
              CREATE TABLE IF NOT EXISTS moderation_log (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 group_id TEXT,
                 message_id TEXT,
                 user_id TEXT,
                  image_sha256 TEXT,
                  nailong_score REAL,
                  naiwa_frog_score REAL,
                  reference_set_version INTEGER,
                  decision TEXT NOT NULL,
                 action_result TEXT,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             INSERT OR IGNORE INTO settings(key, value)
                 VALUES('reference_set_version', '1');
             CREATE TABLE IF NOT EXISTS reference_images (
                 id TEXT PRIMARY KEY,
                 class TEXT NOT NULL CHECK(class IN ('NAILONG', 'NAIWA_FROG')),
                 file_path TEXT NOT NULL,
                 sha256 TEXT NOT NULL UNIQUE,
                 phash TEXT NOT NULL,
                 descriptor_path TEXT,
                 width INTEGER NOT NULL,
                 height INTEGER NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS prediction_cache (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 image_sha256 TEXT NOT NULL,
                 reference_set_version INTEGER NOT NULL,
                 engine_fingerprint TEXT NOT NULL,
                 label TEXT NOT NULL CHECK(label IN ('NAILONG', 'NAIWA_FROG', 'OTHER', 'UNKNOWN')),
                 nailong_score REAL NOT NULL,
                 naiwa_frog_score REAL NOT NULL,
                 confidence_level TEXT NOT NULL CHECK(confidence_level IN ('NONE', 'LOW', 'MEDIUM', 'HIGH', 'VERY_HIGH')),
                 classification_json TEXT,
                 source TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(image_sha256, reference_set_version, engine_fingerprint)
             );
              INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                  VALUES(1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
              INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                  VALUES(2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
        )?;

        if !self.column_exists("moderation_log", "classification_label")? {
            self.exec("ALTER TABLE moderation_log ADD COLUMN classification_label TEXT")?;
        }
        self.exec(
            "CREATE TABLE IF NOT EXISTS moderation_messages (
                 group_id TEXT NOT NULL,
                 message_id TEXT NOT NULL,
                 state TEXT NOT NULL CHECK(state IN ('SEEN', 'WOULD_RECALL', 'RECALL_ATTEMPTED', 'RECALLED', 'RECALL_FAILED')),
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 PRIMARY KEY(group_id, message_id)
             );",
        )?;
        if !self.migration_applied(3)? {
            self.exec(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                     VALUES(3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            )?;
        }

        if !self.column_exists("prediction_cache", "classification_json")? {
            self.exec("ALTER TABLE prediction_cache ADD COLUMN classification_json TEXT")?;
        }
        if !self.migration_applied(4)? {
            self.exec(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                     VALUES(4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            )?;
        }
        if !self.column_exists("prediction_cache", "engine_fingerprint")? {
            self.exec("BEGIN IMMEDIATE")?;
            let result = (|| {
                self.exec("DROP TABLE IF EXISTS prediction_cache_v5")?;
                self.exec(
                    "CREATE TABLE prediction_cache_v5 (
                         id INTEGER PRIMARY KEY AUTOINCREMENT,
                         image_sha256 TEXT NOT NULL,
                         reference_set_version INTEGER NOT NULL,
                         engine_fingerprint TEXT NOT NULL,
                         label TEXT NOT NULL CHECK(label IN ('NAILONG', 'NAIWA_FROG', 'OTHER', 'UNKNOWN')),
                         nailong_score REAL NOT NULL,
                         naiwa_frog_score REAL NOT NULL,
                         confidence_level TEXT NOT NULL CHECK(confidence_level IN ('NONE', 'LOW', 'MEDIUM', 'HIGH', 'VERY_HIGH')),
                         classification_json TEXT,
                         source TEXT NOT NULL,
                         created_at TEXT NOT NULL,
                         UNIQUE(image_sha256, reference_set_version, engine_fingerprint)
                     )",
                )?;
                self.exec(
                    "INSERT INTO prediction_cache_v5(image_sha256, reference_set_version, engine_fingerprint, label, nailong_score, naiwa_frog_score, confidence_level, classification_json, source, created_at)
                     SELECT image_sha256, reference_set_version, '', label, nailong_score, naiwa_frog_score, confidence_level, classification_json, source, created_at
                     FROM prediction_cache",
                )?;
                self.exec("DROP TABLE prediction_cache")?;
                self.exec("ALTER TABLE prediction_cache_v5 RENAME TO prediction_cache")
            })();
            match result {
                Ok(()) => self.exec("COMMIT")?,
                Err(error) => {
                    let _ = self.exec("ROLLBACK");
                    return Err(error);
                }
            }
        }
        if !self.migration_applied(5)? {
            self.exec(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                     VALUES(5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            )?;
        }
        Ok(())
    }

    fn exec(&self, sql: &str) -> Result<(), StorageError> {
        let sql = CString::new(sql)?;
        let mut error_message = std::ptr::null_mut();
        let code = unsafe {
            sqlite3_exec(
                self.raw,
                sql.as_ptr(),
                None,
                std::ptr::null_mut(),
                &mut error_message,
            )
        };
        if code == SQLITE_OK {
            return Ok(());
        }
        let message = if error_message.is_null() {
            self.error_message()
        } else {
            let message = unsafe { CStr::from_ptr(error_message) }
                .to_string_lossy()
                .into_owned();
            unsafe { sqlite3_free(error_message.cast()) };
            message
        };
        Err(StorageError { code, message })
    }

    fn execute<F>(&self, sql: &str, binder: F) -> Result<(), StorageError>
    where
        F: FnOnce(*mut sqlite3_stmt) -> Result<(), StorageError>,
    {
        let statement = Statement::prepare(self, sql)?;
        binder(statement.raw)?;
        let code = unsafe { sqlite3_step(statement.raw) };
        if code != SQLITE_DONE {
            return Err(self.error(code, "SQLite statement failed"));
        }
        Ok(())
    }

    #[cfg(test)]
    fn query_i64(&self, sql: &str) -> Result<i64, StorageError> {
        let statement = Statement::prepare(self, sql)?;
        let code = unsafe { sqlite3_step(statement.raw) };
        if code != SQLITE_ROW {
            return Err(self.error(code, "SQLite query returned no row"));
        }
        Ok(unsafe { sqlite3_column_int64(statement.raw, 0) })
    }

    fn migration_applied(&self, version: u32) -> Result<bool, StorageError> {
        let statement = Statement::prepare(
            self,
            "SELECT 1 FROM schema_migrations WHERE version=?1 LIMIT 1",
        )?;
        bind_int64(statement.raw, 1, i64::from(version))?;
        match unsafe { sqlite3_step(statement.raw) } {
            SQLITE_ROW => Ok(true),
            SQLITE_DONE => Ok(false),
            code => Err(self.error(code, "schema migration lookup failed")),
        }
    }

    fn column_exists(&self, table: &str, column: &str) -> Result<bool, StorageError> {
        let sql = format!("PRAGMA table_info({table})");
        let statement = Statement::prepare(self, &sql)?;
        loop {
            match unsafe { sqlite3_step(statement.raw) } {
                SQLITE_ROW => {
                    if column_optional_text(statement.raw, 1).as_deref() == Some(column) {
                        return Ok(true);
                    }
                }
                SQLITE_DONE => return Ok(false),
                code => return Err(self.error(code, "SQLite table schema lookup failed")),
            }
        }
    }

    fn query_text(&self, sql: &str) -> Result<String, StorageError> {
        let statement = Statement::prepare(self, sql)?;
        let code = unsafe { sqlite3_step(statement.raw) };
        if code != SQLITE_ROW {
            return Err(self.error(code, "SQLite text query returned no row"));
        }
        column_text(statement.raw, 0)
    }

    fn setting_value(&self, key: &str) -> Result<Option<String>, StorageError> {
        let statement = Statement::prepare(self, "SELECT value FROM settings WHERE key=?1")?;
        bind_text(statement.raw, 1, key)?;
        match unsafe { sqlite3_step(statement.raw) } {
            SQLITE_ROW => Ok(column_optional_text(statement.raw, 0)),
            SQLITE_DONE => Ok(None),
            code => Err(self.error(code, "SQLite setting query failed")),
        }
    }

    fn setting_f32(&self, key: &str, default: f32) -> Result<f32, StorageError> {
        self.setting_value(key)?.map_or(Ok(default), |value| {
            value.parse::<f32>().map_err(|_| StorageError {
                code: -1,
                message: format!("{key} is not a valid number: {value}"),
            })
        })
    }

    fn setting_u32(&self, key: &str, default: u32) -> Result<u32, StorageError> {
        self.setting_value(key)?.map_or(Ok(default), |value| {
            value.parse::<u32>().map_err(|_| StorageError {
                code: -1,
                message: format!("{key} is not a valid unsigned integer: {value}"),
            })
        })
    }

    fn error(&self, code: c_int, fallback: &str) -> StorageError {
        StorageError {
            code,
            message: if self.raw.is_null() {
                fallback.to_owned()
            } else {
                self.error_message()
            },
        }
    }

    fn error_from_raw(raw: *mut sqlite3, code: c_int, fallback: &str) -> StorageError {
        let message = if raw.is_null() {
            fallback.to_owned()
        } else {
            let pointer = unsafe { sqlite3_errmsg(raw) };
            if pointer.is_null() {
                fallback.to_owned()
            } else {
                unsafe { CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        StorageError { code, message }
    }

    fn error_message(&self) -> String {
        let pointer = unsafe { sqlite3_errmsg(self.raw) };
        if pointer.is_null() {
            "unknown SQLite error".to_owned()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for AppDatabase {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_close(self.raw) };
        }
    }
}

struct Statement {
    raw: *mut sqlite3_stmt,
}

impl Statement {
    fn prepare(database: &AppDatabase, sql: &str) -> Result<Self, StorageError> {
        let sql = CString::new(sql)?;
        let mut raw = std::ptr::null_mut();
        let code = unsafe {
            sqlite3_prepare_v2(
                database.raw,
                sql.as_ptr(),
                -1,
                &mut raw,
                std::ptr::null_mut(),
            )
        };
        if code != SQLITE_OK {
            return Err(database.error(code, "cannot prepare SQLite statement"));
        }
        Ok(Self { raw })
    }
}

impl Drop for Statement {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { sqlite3_finalize(self.raw) };
        }
    }
}

fn bind_text(statement: *mut sqlite3_stmt, index: c_int, value: &str) -> Result<(), StorageError> {
    let value = CString::new(value)?;
    // SQLITE_TRANSIENT (-1) tells SQLite to copy the bytes before this local
    // CString is dropped at the end of the bind call.
    let transient: Option<unsafe extern "C" fn(*mut c_void)> =
        unsafe { std::mem::transmute(-1isize) };
    let code = unsafe { sqlite3_bind_text(statement, index, value.as_ptr(), -1, transient) };
    if code == SQLITE_OK {
        Ok(())
    } else {
        Err(StorageError {
            code,
            message: "cannot bind SQLite text".to_owned(),
        })
    }
}

fn column_text(statement: *mut sqlite3_stmt, column: c_int) -> Result<String, StorageError> {
    let pointer = unsafe { sqlite3_column_text(statement, column) };
    if pointer.is_null() {
        return Err(StorageError {
            code: -1,
            message: format!("SQLite column {column} unexpectedly contains NULL"),
        });
    }
    Ok(unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned())
}

fn column_optional_text(statement: *mut sqlite3_stmt, column: c_int) -> Option<String> {
    let pointer = unsafe { sqlite3_column_text(statement, column) };
    if pointer.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

fn bind_optional_text(
    statement: *mut sqlite3_stmt,
    index: c_int,
    value: Option<&str>,
) -> Result<(), StorageError> {
    match value {
        Some(value) => bind_text(statement, index, value),
        None => bind_null(statement, index),
    }
}

fn bind_int64(statement: *mut sqlite3_stmt, index: c_int, value: i64) -> Result<(), StorageError> {
    let code = unsafe { sqlite3_bind_int64(statement, index, value) };
    if code == SQLITE_OK {
        Ok(())
    } else {
        Err(StorageError {
            code,
            message: "cannot bind SQLite integer".to_owned(),
        })
    }
}

fn bind_double(statement: *mut sqlite3_stmt, index: c_int, value: f64) -> Result<(), StorageError> {
    let code = unsafe { sqlite3_bind_double(statement, index, value) };
    if code == SQLITE_OK {
        Ok(())
    } else {
        Err(StorageError {
            code,
            message: "cannot bind SQLite real".to_owned(),
        })
    }
}

fn bind_optional_double(
    statement: *mut sqlite3_stmt,
    index: c_int,
    value: Option<f64>,
) -> Result<(), StorageError> {
    match value {
        Some(value) => bind_double(statement, index, value),
        None => bind_null(statement, index),
    }
}

fn bind_optional_int64(
    statement: *mut sqlite3_stmt,
    index: c_int,
    value: Option<u64>,
) -> Result<(), StorageError> {
    match value {
        Some(value) => {
            let value = i64::try_from(value).map_err(|_| StorageError {
                code: -1,
                message: "reference_set_version exceeds SQLite integer range".to_owned(),
            })?;
            bind_int64(statement, index, value)
        }
        None => bind_null(statement, index),
    }
}

fn bind_null(statement: *mut sqlite3_stmt, index: c_int) -> Result<(), StorageError> {
    let code = unsafe { sqlite3_bind_null(statement, index) };
    if code == SQLITE_OK {
        Ok(())
    } else {
        Err(StorageError {
            code,
            message: "cannot bind SQLite NULL".to_owned(),
        })
    }
}

fn validate_score(value: f64, field: &str) -> Result<(), StorageError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("{field} must be a finite probability between 0 and 1"),
        })
    }
}

fn validate_reference_class(value: &str) -> Result<(), StorageError> {
    if matches!(value, "NAILONG" | "NAIWA_FROG") {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("invalid reference class: {value}"),
        })
    }
}

fn validate_label(value: &str) -> Result<(), StorageError> {
    if matches!(value, "NAILONG" | "NAIWA_FROG" | "OTHER" | "UNKNOWN") {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("invalid classification label: {value}"),
        })
    }
}

fn validate_confidence(value: &str) -> Result<(), StorageError> {
    if matches!(value, "NONE" | "LOW" | "MEDIUM" | "HIGH" | "VERY_HIGH") {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("invalid confidence level: {value}"),
        })
    }
}

fn validate_moderation_message_state(value: &str) -> Result<(), StorageError> {
    if matches!(
        value,
        "SEEN" | "WOULD_RECALL" | "RECALL_ATTEMPTED" | "RECALLED" | "RECALL_FAILED"
    ) {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("invalid moderation message state: {value}"),
        })
    }
}

fn validate_sha256_text(value: &str, field: &str) -> Result<(), StorageError> {
    validate_hex_text(value, 64, field)
}

fn validate_hex_text(value: &str, expected_length: usize, field: &str) -> Result<(), StorageError> {
    if value.len() == expected_length && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StorageError {
            code: -1,
            message: format!("{field} must be {expected_length} hexadecimal characters"),
        })
    }
}

trait ExpandUser {
    fn expanduser(&self) -> PathBuf;
}

impl ExpandUser for Path {
    fn expanduser(&self) -> PathBuf {
        self.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppDatabase, ModerationLogRecord, PredictionCacheRecord, QQGroupRecord, ReferenceRecord,
    };
    use crate::vision::VisionThresholds;

    fn temporary_database_path() -> std::path::PathBuf {
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("test")
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        std::env::temp_dir().join(format!(
            "nlnf-storage-test-{}-{}.db",
            std::process::id(),
            thread_name
        ))
    }

    fn test_engine_fingerprint() -> String {
        "a".repeat(64)
    }

    #[test]
    fn creates_schema_and_persists_active_record_types() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        assert_eq!(database.schema_version(), 5);
        assert!(!database.builtin_reference_bank_seeded().unwrap());
        database.mark_builtin_reference_bank_seeded().unwrap();
        assert!(database.builtin_reference_bank_seeded().unwrap());
        database
            .upsert_qq_group(&QQGroupRecord {
                group_id: "group-1".to_owned(),
                group_name: "test".to_owned(),
                mode: "OFF".to_owned(),
                recall_threshold: 0.98,
                created_at: "2026-09-12T00:00:00Z".to_owned(),
                updated_at: "2026-09-12T00:00:02Z".to_owned(),
            })
            .expect("group persists");
        database
            .append_moderation_log(&ModerationLogRecord {
                group_id: Some("group-1".to_owned()),
                message_id: Some("message-1".to_owned()),
                user_id: None,
                image_sha256: Some("a".repeat(64)),
                nailong_score: Some(0.12),
                naiwa_frog_score: Some(0.98),
                reference_set_version: Some(1),
                classification_label: Some("NAIWA_FROG".to_owned()),
                decision: "PASS".to_owned(),
                action_result: Some("NONE".to_owned()),
                created_at: "2026-09-12T00:00:03Z".to_owned(),
            })
            .expect("moderation log persists");
        database
            .record_reference(&ReferenceRecord {
                id: "NF01".to_owned(),
                class: "NAIWA_FROG".to_owned(),
                file_path: "references/naiwa_frog/NF01.png".to_owned(),
                sha256: "f".repeat(64),
                phash: "0123456789abcdef".to_owned(),
                descriptor_path: Some("cache/NF01.desc".to_owned()),
                width: 640,
                height: 480,
                created_at: "2026-09-12T00:00:04Z".to_owned(),
            })
            .expect("reference persists");
        database
            .record_prediction_cache(&PredictionCacheRecord {
                image_sha256: "a".repeat(64),
                reference_set_version: 1,
                engine_fingerprint: test_engine_fingerprint(),
                label: "NAIWA_FROG".to_owned(),
                nailong_score: 0.12,
                naiwa_frog_score: 0.98,
                confidence_level: "VERY_HIGH".to_owned(),
                classification_json: None,
                source: "opencv-sift".to_owned(),
                created_at: "2026-09-12T00:00:05Z".to_owned(),
            })
            .expect("prediction cache persists");
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM reference_images")
                .unwrap(),
            1
        );
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM prediction_cache")
                .unwrap(),
            1
        );
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM qq_groups")
                .unwrap(),
            1
        );
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM moderation_log")
                .unwrap(),
            1
        );
        let groups = database.list_qq_groups().expect("groups are readable");
        assert_eq!(groups[0].group_id, "group-1");
        let logs = database
            .recent_moderation_logs(10)
            .expect("moderation logs are readable");
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].message_id.as_deref(), Some("message-1"));
        assert_eq!(logs[0].naiwa_frog_score, Some(0.98));
        assert_eq!(logs[0].classification_label.as_deref(), Some("NAIWA_FROG"));
        drop(database);
        assert!(path.is_file());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prediction_cache_is_keyed_by_sha_version_and_engine_fingerprint() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        let record = PredictionCacheRecord {
            nailong_score: 0.10,
            naiwa_frog_score: 0.20,
            image_sha256: "b".repeat(64),
            reference_set_version: 1,
            engine_fingerprint: test_engine_fingerprint(),
            label: "OTHER".to_owned(),
            confidence_level: "LOW".to_owned(),
            classification_json: None,
            source: "first".to_owned(),
            created_at: "2026-09-12T00:00:00Z".to_owned(),
        };
        database
            .record_prediction_cache(&record)
            .expect("first prediction");
        database
            .record_prediction_cache(&PredictionCacheRecord {
                source: "updated".to_owned(),
                nailong_score: 0.30,
                ..record
            })
            .expect("upsert prediction");
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM prediction_cache")
                .unwrap(),
            1
        );
        let cached = database
            .find_prediction_cache(&"b".repeat(64), 1, &test_engine_fingerprint())
            .unwrap()
            .expect("prediction cache entry is readable");
        assert_eq!(cached.nailong_score, 0.30);
        assert_eq!(cached.source, "updated");
        let other_engine_fingerprint = "b".repeat(64);
        assert!(database
            .find_prediction_cache(&"b".repeat(64), 1, &other_engine_fingerprint)
            .unwrap()
            .is_none());
        database
            .record_prediction_cache(&PredictionCacheRecord {
                engine_fingerprint: other_engine_fingerprint.clone(),
                source: "other-engine".to_owned(),
                ..cached.clone()
            })
            .expect("a different engine fingerprint has an independent cache entry");
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM prediction_cache")
                .unwrap(),
            2
        );
        database
            .record_prediction_cache(&PredictionCacheRecord {
                classification_json: Some(r#"{"label":"OTHER"}"#.to_owned()),
                ..cached.clone()
            })
            .expect("full classification cache persists");
        assert_eq!(
            database
                .find_prediction_cache(&"b".repeat(64), 1, &test_engine_fingerprint())
                .unwrap()
                .unwrap()
                .classification_json
                .as_deref(),
            Some(r#"{"label":"OTHER"}"#)
        );
        assert!(database
            .find_prediction_cache(&"c".repeat(64), 1, &test_engine_fingerprint())
            .unwrap()
            .is_none());
        database
            .record_prediction_cache(&PredictionCacheRecord {
                image_sha256: "b".repeat(64),
                reference_set_version: 2,
                engine_fingerprint: test_engine_fingerprint(),
                label: "NAILONG".to_owned(),
                nailong_score: 0.9,
                naiwa_frog_score: 0.1,
                confidence_level: "HIGH".to_owned(),
                classification_json: None,
                source: "updated-reference-bank".to_owned(),
                created_at: "2026-09-12T00:00:01Z".to_owned(),
            })
            .expect("new reference version has a separate cache key");
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM prediction_cache")
                .unwrap(),
            3
        );
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn invalid_probabilities_and_group_modes_fail_closed() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        let prediction = database.record_prediction_cache(&PredictionCacheRecord {
            image_sha256: "c".repeat(64),
            reference_set_version: 1,
            engine_fingerprint: test_engine_fingerprint(),
            label: "NAIWA_FROG".to_owned(),
            nailong_score: f64::NAN,
            naiwa_frog_score: 0.2,
            confidence_level: "LOW".to_owned(),
            classification_json: None,
            source: "test".to_owned(),
            created_at: "now".to_owned(),
        });
        assert!(prediction.is_err());
        let group = database.upsert_qq_group(&QQGroupRecord {
            group_id: "group".to_owned(),
            group_name: "group".to_owned(),
            mode: "RECALL_EVERYTHING".to_owned(),
            recall_threshold: 0.98,
            created_at: "now".to_owned(),
            updated_at: "now".to_owned(),
        });
        assert!(group.is_err());
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reference_bank_and_versioned_prediction_cache_are_persisted() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        assert_eq!(database.reference_set_version().unwrap(), 1);
        assert_eq!(
            database
                .record_reference_and_bump_version(&ReferenceRecord {
                    id: "NF01".to_owned(),
                    class: "NAIWA_FROG".to_owned(),
                    file_path: "references/naiwa_frog/NF01.png".to_owned(),
                    sha256: "f".repeat(64),
                    phash: "0123456789abcdef".to_owned(),
                    descriptor_path: Some("cache/NF01.desc".to_owned()),
                    width: 640,
                    height: 480,
                    created_at: "2026-09-12T00:00:00Z".to_owned(),
                })
                .expect("reference and version persist atomically"),
            2
        );
        assert_eq!(database.count_references("NAIWA_FROG").unwrap(), 1);
        assert_eq!(database.list_references().unwrap()[0].id, "NF01");
        let reference_hash = database.reference_set_hash().unwrap();
        assert_eq!(reference_hash.len(), 64);
        let cache = PredictionCacheRecord {
            image_sha256: "a".repeat(64),
            reference_set_version: 2,
            engine_fingerprint: test_engine_fingerprint(),
            label: "NAIWA_FROG".to_owned(),
            nailong_score: 0.12,
            naiwa_frog_score: 0.91,
            confidence_level: "VERY_HIGH".to_owned(),
            classification_json: None,
            source: "opencv-sift".to_owned(),
            created_at: "2026-09-12T00:00:01Z".to_owned(),
        };
        database
            .record_prediction_cache(&cache)
            .expect("versioned cache persists");
        assert_eq!(
            database
                .find_prediction_cache(&"a".repeat(64), 2, &test_engine_fingerprint())
                .unwrap(),
            Some(cache)
        );
        assert!(database
            .find_prediction_cache(&"a".repeat(64), 1, &test_engine_fingerprint())
            .unwrap()
            .is_none());
        assert_eq!(
            database
                .delete_reference_and_bump_version("NF01")
                .expect("reference deletes and bumps version"),
            3
        );
        assert_eq!(database.count_references("NAIWA_FROG").unwrap(), 0);
        assert_ne!(reference_hash, database.reference_set_hash().unwrap());
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn verified_reference_set_hash_rejects_changed_reference_bytes() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let reference_path = std::env::temp_dir().join(format!(
            "nlnf-verified-reference-{}-verify.png",
            std::process::id()
        ));
        let original = b"reference bytes";
        std::fs::write(&reference_path, original).unwrap();
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        database
            .record_reference(&ReferenceRecord {
                id: "NF-VERIFY".to_owned(),
                class: "NAIWA_FROG".to_owned(),
                file_path: reference_path.to_string_lossy().into_owned(),
                sha256: crate::release::sha256_hex(original),
                phash: "0123456789abcdef".to_owned(),
                descriptor_path: None,
                width: 1,
                height: 1,
                created_at: "now".to_owned(),
            })
            .unwrap();
        assert_eq!(database.verified_reference_set_hash().unwrap().len(), 64);
        std::fs::write(&reference_path, b"changed bytes").unwrap();
        let error = database.verified_reference_set_hash().unwrap_err();
        assert!(error.message.contains("do not match"));
        drop(database);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(reference_path);
    }

    #[test]
    fn failed_atomic_reference_mutation_does_not_advance_version() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        let record = ReferenceRecord {
            id: "NF01".to_owned(),
            class: "NAIWA_FROG".to_owned(),
            file_path: "references/naiwa_frog/NF01.png".to_owned(),
            sha256: "f".repeat(64),
            phash: "0123456789abcdef".to_owned(),
            descriptor_path: None,
            width: 640,
            height: 480,
            created_at: "now".to_owned(),
        };
        assert_eq!(
            database.record_reference_and_bump_version(&record).unwrap(),
            2
        );
        let duplicate = ReferenceRecord {
            id: "NF02".to_owned(),
            ..record
        };
        assert!(database
            .record_reference_and_bump_version(&duplicate)
            .is_err());
        assert_eq!(database.reference_set_version().unwrap(), 2);
        assert_eq!(database.count_references("NAIWA_FROG").unwrap(), 1);
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reopening_database_is_idempotent_and_preserves_the_schema_migration() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        {
            let database = AppDatabase::open(&path).expect("initial schema migration");
            database
                .record_prediction_cache(&PredictionCacheRecord {
                    image_sha256: "e".repeat(64),
                    reference_set_version: 1,
                    engine_fingerprint: test_engine_fingerprint(),
                    label: "UNKNOWN".to_owned(),
                    nailong_score: 0.4,
                    naiwa_frog_score: 0.6,
                    confidence_level: "MEDIUM".to_owned(),
                    classification_json: None,
                    source: "test".to_owned(),
                    created_at: "2026-09-12T00:00:00Z".to_owned(),
                })
                .expect("prediction persists before reopen");
        }
        let reopened = AppDatabase::open(&path).expect("reopen applies no duplicate migration");
        assert_eq!(reopened.schema_version(), 5);
        assert!(reopened
            .find_prediction_cache(&"e".repeat(64), 1, &test_engine_fingerprint())
            .unwrap()
            .is_some());
        assert_eq!(
            reopened
                .query_i64("SELECT COUNT(*) FROM schema_migrations")
                .unwrap(),
            5
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn moderation_message_claim_is_group_scoped_and_persistent() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        assert!(database
            .claim_moderation_message("group-a", "message-1", "now")
            .unwrap());
        assert!(!database
            .claim_moderation_message("group-a", "message-1", "later")
            .unwrap());
        assert!(database
            .claim_moderation_message("group-b", "message-1", "now")
            .unwrap());
        database
            .update_moderation_message_state("group-a", "message-1", "RECALLED", "later")
            .unwrap();
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM moderation_messages")
                .unwrap(),
            2
        );
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reference_mutations_persistently_downgrade_auto_recall_groups() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        database
            .upsert_qq_group(&QQGroupRecord {
                group_id: "group-1".to_owned(),
                group_name: "test".to_owned(),
                mode: "AUTO_RECALL".to_owned(),
                recall_threshold: 0.98,
                created_at: "now".to_owned(),
                updated_at: "now".to_owned(),
            })
            .unwrap();
        let record = ReferenceRecord {
            id: "NF01".to_owned(),
            class: "NAIWA_FROG".to_owned(),
            file_path: "references/naiwa_frog/NF01.png".to_owned(),
            sha256: "f".repeat(64),
            phash: "0123456789abcdef".to_owned(),
            descriptor_path: None,
            width: 640,
            height: 480,
            created_at: "now".to_owned(),
        };
        database
            .record_reference_and_bump_version(&record)
            .expect("reference insert commits");
        assert_eq!(database.list_qq_groups().unwrap()[0].mode, "OBSERVE");

        database
            .upsert_qq_group(&QQGroupRecord {
                mode: "AUTO_RECALL".to_owned(),
                updated_at: "later".to_owned(),
                ..database.list_qq_groups().unwrap()[0].clone()
            })
            .unwrap();
        database
            .delete_reference_and_bump_version("NF01")
            .expect("reference delete commits");
        assert_eq!(database.list_qq_groups().unwrap()[0].mode, "OBSERVE");
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn threshold_mutations_persistently_downgrade_auto_recall_groups() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        database
            .upsert_qq_group(&QQGroupRecord {
                group_id: "group-threshold".to_owned(),
                group_name: "test".to_owned(),
                mode: "AUTO_RECALL".to_owned(),
                recall_threshold: 0.98,
                created_at: "now".to_owned(),
                updated_at: "now".to_owned(),
            })
            .unwrap();
        let thresholds = VisionThresholds {
            match_threshold: 0.64,
            ..VisionThresholds::default()
        };
        database
            .save_app_settings(thresholds, false)
            .expect("threshold update commits");
        assert_eq!(
            database
                .find_qq_group("group-threshold")
                .unwrap()
                .unwrap()
                .mode,
            "OBSERVE"
        );
        drop(database);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn app_settings_round_trip_and_reject_unsafe_threshold_ordering() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        let thresholds = VisionThresholds {
            match_threshold: 0.64,
            other_threshold: 0.22,
            min_recall_inliers: 18,
            ..VisionThresholds::default()
        };
        database
            .save_app_settings(thresholds, true)
            .expect("settings persist");
        let (loaded, developer_mode) = database.app_settings().expect("settings load");
        assert_eq!(loaded, thresholds);
        assert!(developer_mode);

        let mut unsafe_thresholds = thresholds;
        unsafe_thresholds.recall_threshold = 0.50;
        assert!(database
            .save_app_settings(unsafe_thresholds, false)
            .is_err());
        let mut zero_inlier_thresholds = thresholds;
        zero_inlier_thresholds.min_recall_inliers = 0;
        assert!(database
            .save_app_settings(zero_inlier_thresholds, false)
            .is_err());
        let (unchanged, still_developer_mode) = database
            .app_settings()
            .expect("invalid settings are not committed");
        assert_eq!(unchanged, thresholds);
        assert!(still_developer_mode);
        drop(database);
        let _ = std::fs::remove_file(path);
    }
}
