//! Small Windows SQLite boundary for the desktop app.
//!
//! The target is Windows 10/11, where `winsqlite3.dll` is available as a
//! system component. Keeping this wrapper small avoids a Python dependency in
//! the shipped app and keeps all SQL behind one typed boundary.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_OPEN_FULLMUTEX: c_int = 0x0001_0000;
const SQLITE_SCHEMA_VERSION: u32 = 2;

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
    pub label: String,
    pub nailong_score: f64,
    pub naiwa_frog_score: f64,
    pub confidence_level: String,
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

    pub fn bump_reference_set_version(&self) -> Result<u64, StorageError> {
        self.execute(
            "UPDATE settings SET value=CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key='reference_set_version'",
            |_| Ok(()),
        )?;
        self.reference_set_version()
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

    pub fn delete_reference(&self, id: &str) -> Result<(), StorageError> {
        if id.trim().is_empty() {
            return Err(StorageError {
                code: -1,
                message: "reference id cannot be empty".to_owned(),
            });
        }
        self.execute("DELETE FROM reference_images WHERE id=?1", |statement| {
            bind_text(statement, 1, id)
        })
    }

    pub fn record_prediction_cache(
        &self,
        record: &PredictionCacheRecord,
    ) -> Result<(), StorageError> {
        validate_sha256_text(&record.image_sha256, "prediction image sha256")?;
        validate_label(&record.label)?;
        validate_confidence(&record.confidence_level)?;
        validate_score(record.nailong_score, "nailong_score")?;
        validate_score(record.naiwa_frog_score, "naiwa_frog_score")?;
        let version = i64::try_from(record.reference_set_version).map_err(|_| StorageError {
            code: -1,
            message: "reference_set_version exceeds SQLite integer range".to_owned(),
        })?;
        self.execute(
            "INSERT INTO prediction_cache(image_sha256, reference_set_version, label, nailong_score, naiwa_frog_score, confidence_level, source, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(image_sha256, reference_set_version) DO UPDATE SET label=excluded.label, nailong_score=excluded.nailong_score, naiwa_frog_score=excluded.naiwa_frog_score, confidence_level=excluded.confidence_level, source=excluded.source, created_at=excluded.created_at",
            |statement| {
                bind_text(statement, 1, &record.image_sha256)?;
                bind_int64(statement, 2, version)?;
                bind_text(statement, 3, &record.label)?;
                bind_double(statement, 4, record.nailong_score)?;
                bind_double(statement, 5, record.naiwa_frog_score)?;
                bind_text(statement, 6, &record.confidence_level)?;
                bind_text(statement, 7, &record.source)?;
                bind_text(statement, 8, &record.created_at)
            },
        )
    }

    pub fn find_prediction_cache(
        &self,
        image_sha256: &str,
        reference_set_version: u64,
    ) -> Result<Option<PredictionCacheRecord>, StorageError> {
        validate_sha256_text(image_sha256, "prediction image sha256")?;
        let version = i64::try_from(reference_set_version).map_err(|_| StorageError {
            code: -1,
            message: "reference_set_version exceeds SQLite integer range".to_owned(),
        })?;
        let statement = Statement::prepare(
            self,
            "SELECT image_sha256, reference_set_version, label, nailong_score, naiwa_frog_score, confidence_level, source, created_at FROM prediction_cache WHERE image_sha256=?1 AND reference_set_version=?2",
        )?;
        bind_text(statement.raw, 1, image_sha256)?;
        bind_int64(statement.raw, 2, version)?;
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
                label: column_text(statement.raw, 2)?,
                nailong_score: unsafe { sqlite3_column_double(statement.raw, 3) },
                naiwa_frog_score: unsafe { sqlite3_column_double(statement.raw, 4) },
                confidence_level: column_text(statement.raw, 5)?,
                source: column_text(statement.raw, 6)?,
                created_at: column_text(statement.raw, 7)?,
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
        self.execute(
            "INSERT INTO moderation_log(group_id, message_id, user_id, image_sha256, nailong_score, naiwa_frog_score, reference_set_version, decision, action_result, created_at) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            |statement| {
                bind_optional_text(statement, 1, record.group_id.as_deref())?;
                bind_optional_text(statement, 2, record.message_id.as_deref())?;
                bind_optional_text(statement, 3, record.user_id.as_deref())?;
                bind_optional_text(statement, 4, record.image_sha256.as_deref())?;
                bind_optional_double(statement, 5, record.nailong_score)?;
                bind_optional_double(statement, 6, record.naiwa_frog_score)?;
                bind_optional_int64(statement, 7, record.reference_set_version)?;
                bind_text(statement, 8, &record.decision)?;
                bind_optional_text(statement, 9, record.action_result.as_deref())?;
                bind_text(statement, 10, &record.created_at)
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
            "SELECT group_id, message_id, user_id, image_sha256, nailong_score, naiwa_frog_score, reference_set_version, decision, action_result, created_at FROM moderation_log ORDER BY id DESC LIMIT ?1",
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
                    decision: column_text(statement.raw, 7)?,
                    action_result: column_optional_text(statement.raw, 8),
                    created_at: column_text(statement.raw, 9)?,
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
                 label TEXT NOT NULL CHECK(label IN ('NAILONG', 'NAIWA_FROG', 'OTHER', 'UNKNOWN')),
                 nailong_score REAL NOT NULL,
                 naiwa_frog_score REAL NOT NULL,
                 confidence_level TEXT NOT NULL CHECK(confidence_level IN ('NONE', 'LOW', 'MEDIUM', 'HIGH', 'VERY_HIGH')),
                 source TEXT NOT NULL,
                 created_at TEXT NOT NULL,
                 UNIQUE(image_sha256, reference_set_version)
             );
             INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                 VALUES(1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
             INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                 VALUES(2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));",
        )
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

    fn query_text(&self, sql: &str) -> Result<String, StorageError> {
        let statement = Statement::prepare(self, sql)?;
        let code = unsafe { sqlite3_step(statement.raw) };
        if code != SQLITE_ROW {
            return Err(self.error(code, "SQLite text query returned no row"));
        }
        column_text(statement.raw, 0)
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

    #[test]
    fn creates_schema_and_persists_active_record_types() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        assert_eq!(database.schema_version(), 2);
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
                label: "NAIWA_FROG".to_owned(),
                nailong_score: 0.12,
                naiwa_frog_score: 0.98,
                confidence_level: "VERY_HIGH".to_owned(),
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
        drop(database);
        assert!(path.is_file());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prediction_cache_is_unique_per_sha_and_reference_version() {
        let path = temporary_database_path();
        let _ = std::fs::remove_file(&path);
        let database = AppDatabase::open(&path).expect("Windows SQLite opens");
        let record = PredictionCacheRecord {
            nailong_score: 0.10,
            naiwa_frog_score: 0.20,
            image_sha256: "b".repeat(64),
            reference_set_version: 1,
            label: "OTHER".to_owned(),
            confidence_level: "LOW".to_owned(),
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
            .find_prediction_cache(&"b".repeat(64), 1)
            .unwrap()
            .expect("prediction cache entry is readable");
        assert_eq!(cached.nailong_score, 0.30);
        assert_eq!(cached.source, "updated");
        assert!(database
            .find_prediction_cache(&"c".repeat(64), 1)
            .unwrap()
            .is_none());
        database
            .record_prediction_cache(&PredictionCacheRecord {
                image_sha256: "b".repeat(64),
                reference_set_version: 2,
                label: "NAILONG".to_owned(),
                nailong_score: 0.9,
                naiwa_frog_score: 0.1,
                confidence_level: "HIGH".to_owned(),
                source: "updated-reference-bank".to_owned(),
                created_at: "2026-09-12T00:00:01Z".to_owned(),
            })
            .expect("new reference version has a separate cache key");
        assert_eq!(
            database
                .query_i64("SELECT COUNT(*) FROM prediction_cache")
                .unwrap(),
            2
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
            label: "NAIWA_FROG".to_owned(),
            nailong_score: f64::NAN,
            naiwa_frog_score: 0.2,
            confidence_level: "LOW".to_owned(),
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
                created_at: "2026-09-12T00:00:00Z".to_owned(),
            })
            .expect("reference persists");
        assert_eq!(database.count_references("NAIWA_FROG").unwrap(), 1);
        assert_eq!(database.list_references().unwrap()[0].id, "NF01");
        assert_eq!(database.bump_reference_set_version().unwrap(), 2);
        let cache = PredictionCacheRecord {
            image_sha256: "a".repeat(64),
            reference_set_version: 2,
            label: "NAIWA_FROG".to_owned(),
            nailong_score: 0.12,
            naiwa_frog_score: 0.91,
            confidence_level: "VERY_HIGH".to_owned(),
            source: "opencv-sift".to_owned(),
            created_at: "2026-09-12T00:00:01Z".to_owned(),
        };
        database
            .record_prediction_cache(&cache)
            .expect("versioned cache persists");
        assert_eq!(
            database.find_prediction_cache(&"a".repeat(64), 2).unwrap(),
            Some(cache)
        );
        assert!(database
            .find_prediction_cache(&"a".repeat(64), 1)
            .unwrap()
            .is_none());
        database
            .delete_reference("NF01")
            .expect("reference deletes");
        assert_eq!(database.count_references("NAIWA_FROG").unwrap(), 0);
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
                    label: "UNKNOWN".to_owned(),
                    nailong_score: 0.4,
                    naiwa_frog_score: 0.6,
                    confidence_level: "MEDIUM".to_owned(),
                    source: "test".to_owned(),
                    created_at: "2026-09-12T00:00:00Z".to_owned(),
                })
                .expect("prediction persists before reopen");
        }
        let reopened = AppDatabase::open(&path).expect("reopen applies no duplicate migration");
        assert_eq!(reopened.schema_version(), 2);
        assert!(reopened
            .find_prediction_cache(&"e".repeat(64), 1)
            .unwrap()
            .is_some());
        assert_eq!(
            reopened
                .query_i64("SELECT COUNT(*) FROM schema_migrations")
                .unwrap(),
            2
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }
}
