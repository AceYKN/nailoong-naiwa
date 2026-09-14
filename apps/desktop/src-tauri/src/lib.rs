use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod decoder;
pub mod image_policy;
pub mod moderation;
pub mod onebot;
pub mod phash;
pub mod qq;
pub mod qq_service;
pub mod references;
pub mod release;
#[cfg(windows)]
pub mod storage;
#[cfg(not(windows))]
#[path = "storage_stub.rs"]
pub mod storage;
pub mod vision;
#[cfg(feature = "opencv-backend")]
pub mod vision_opencv;

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    product_name: &'static str,
    app_version: &'static str,
    phase: &'static str,
    vision_backend: &'static str,
    vision_available: bool,
    vision_message: &'static str,
    reference_set_version: u64,
    nailong_reference_count: u64,
    naiwa_frog_reference_count: u64,
    network_required_for_classification: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageInfo {
    path: String,
    schema_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppSettings {
    thresholds: vision::VisionThresholds,
    developer_mode: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisionInfo {
    backend: &'static str,
    available: bool,
    message: &'static str,
    reference_set_version: u64,
    nailong_reference_count: u64,
    naiwa_frog_reference_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReferenceInfo {
    id: String,
    class: String,
    file_path: String,
    sha256: String,
    phash: String,
    descriptor_path: Option<String>,
    width: u32,
    height: u32,
    created_at: String,
}

#[tauri::command]
fn app_info(app: tauri::AppHandle) -> AppInfo {
    let (vision_backend, vision_available, vision_message) = vision_backend_status();
    let (reference_set_version, nailong_reference_count, naiwa_frog_reference_count) =
        match open_database(&app) {
            Ok(database) => (
                database.reference_set_version().unwrap_or(1),
                database.count_references("NAILONG").unwrap_or(0),
                database.count_references("NAIWA_FROG").unwrap_or(0),
            ),
            Err(_) => (1, 0, 0),
        };
    AppInfo {
        product_name: "NLNF Classifier",
        app_version: env!("CARGO_PKG_VERSION"),
        phase: "Phase 4 — Reference matching",
        vision_backend,
        vision_available,
        vision_message,
        reference_set_version,
        nailong_reference_count,
        naiwa_frog_reference_count,
        network_required_for_classification: false,
    }
}

#[tauri::command]
fn inspect_image(bytes: Vec<u8>) -> Result<image_policy::ImageInspection, String> {
    image_policy::inspect_image(&bytes).map_err(|error| error.to_string())
}

#[tauri::command]
fn decode_image_summary(
    bytes: Vec<u8>,
    max_sample_frames: Option<u32>,
) -> Result<decoder::DecodeSummary, String> {
    decoder::summarize(
        &bytes,
        max_sample_frames.unwrap_or(crate::image_policy::MAX_SAMPLE_FRAMES),
    )
}

#[tauri::command]
fn classify_image(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    max_sample_frames: Option<u32>,
) -> Result<vision::ClassificationResult, String> {
    classify_image_bytes(&app, &bytes, max_sample_frames)
}

pub(crate) fn classify_image_bytes(
    app: &tauri::AppHandle,
    bytes: &[u8],
    max_sample_frames: Option<u32>,
) -> Result<vision::ClassificationResult, String> {
    let sample_frame_limit = max_sample_frames
        .unwrap_or(crate::image_policy::MAX_SAMPLE_FRAMES)
        .min(crate::image_policy::MAX_SAMPLE_FRAMES);
    let decoded = decoder::decode_image(bytes, sample_frame_limit)?;

    #[cfg(feature = "opencv-backend")]
    {
        let database = open_database(app)?;
        let reference_set_version = database
            .reference_set_version()
            .map_err(|error| error.to_string())?;
        let image_sha256 = decoded.inspection.sha256.clone();
        let records = database
            .list_references()
            .map_err(|error| error.to_string())?;
        if records
            .iter()
            .filter(|record| record.class == "NAILONG")
            .count()
            == 0
            || records
                .iter()
                .filter(|record| record.class == "NAIWA_FROG")
                .count()
                == 0
        {
            return Err("请先为奶龙和奶蛙各添加至少 1 张参考图".to_owned());
        }
        let (vision_thresholds, _) = database.app_settings().map_err(|error| error.to_string())?;
        let config = vision_opencv::OpenCvConfig {
            vision_thresholds,
            ..vision_opencv::OpenCvConfig::default()
        };
        let descriptor_fingerprint = vision_opencv::descriptor_fingerprint(config);
        let engine_fingerprint = vision_opencv::engine_fingerprint(config, sample_frame_limit);
        // Validate every source file before consulting the prediction cache.
        // A cached classification must never hide a modified or replaced
        // Reference Bank from the recall safety checks.
        let mut verified_records = Vec::with_capacity(records.len());
        for record in records {
            let reference_bytes = read_verified_reference_bytes(&record)?;
            verified_records.push((record, reference_bytes));
        }
        if let Ok(Some(cached)) = database.find_prediction_cache(
            &image_sha256,
            reference_set_version,
            &engine_fingerprint,
        ) {
            if let Some(serialized) = cached.classification_json {
                if let Ok(result) =
                    serde_json::from_str::<vision::ClassificationResult>(&serialized)
                {
                    return Ok(result);
                }
            }
        }
        let mut engine = vision_opencv::OpenCvVisionEngine::new(config)?;
        let mut references = Vec::with_capacity(verified_records.len());
        for (record, reference_bytes) in verified_records {
            let class = parse_reference_class(&record.class)?;
            let cached = record
                .descriptor_path
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.is_file())
                .and_then(|path| {
                    vision_opencv::read_reference_features(
                        &path,
                        record.id.clone(),
                        class,
                        &record.sha256,
                        &descriptor_fingerprint,
                    )
                    .ok()
                });
            if let Some(features) = cached {
                references.push(features);
                continue;
            }
            let reference_image =
                decoder::decode_image(&reference_bytes, image_policy::MAX_SAMPLE_FRAMES)?;
            let frame = reference_image
                .frames
                .first()
                .ok_or_else(|| format!("reference {} has no decoded frame", record.id))?;
            let features = engine.extract_reference_with_source_sha256(
                record.id.clone(),
                class,
                record.sha256.clone(),
                frame,
            )?;
            if let Some(path) = record.descriptor_path.as_deref().map(PathBuf::from) {
                if let Err(error) = vision_opencv::write_reference_features(&path, &features) {
                    eprintln!("descriptor cache refresh failed for {}: {error}", record.id);
                }
            }
            references.push(features);
        }
        let result = engine.classify_frames(&decoded.frames, &references)?;
        let classification_json = serde_json::to_string(&result).ok();
        if let Err(error) = database.record_prediction_cache(&storage::PredictionCacheRecord {
            image_sha256,
            reference_set_version,
            engine_fingerprint,
            label: classification_label_name(result.label).to_owned(),
            nailong_score: f64::from(result.nailong_score),
            naiwa_frog_score: f64::from(result.naiwa_frog_score),
            confidence_level: confidence_level_name(result.confidence_level).to_owned(),
            classification_json,
            source: "opencv-sift-akaze".to_owned(),
            created_at: current_timestamp(),
        }) {
            eprintln!("prediction cache write skipped: {error}");
        }
        Ok(result)
    }

    #[cfg(not(feature = "opencv-backend"))]
    {
        let _ = (app, decoded);
        Err(
            "OpenCV SIFT/AKAZE backend is not compiled; refusing to generate a classification result"
                .to_owned(),
        )
    }
}

#[tauri::command]
fn debug_match_image(
    app: tauri::AppHandle,
    bytes: Vec<u8>,
    reference_id: String,
) -> Result<Vec<u8>, String> {
    #[cfg(feature = "opencv-backend")]
    {
        if reference_id.trim().is_empty() {
            return Err("reference id cannot be empty".to_owned());
        }
        let query = decoder::decode_image(&bytes, 1)?;
        let database = open_database(&app)?;
        let record = database
            .list_references()
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|record| record.id == reference_id)
            .ok_or_else(|| "reference not found".to_owned())?;
        let class = parse_reference_class(&record.class)?;
        let reference_bytes = read_verified_reference_bytes(&record)?;
        let reference = decoder::decode_image(&reference_bytes, 1)?;
        let query_frame = query
            .frames
            .first()
            .ok_or_else(|| "query image did not produce a decoded frame".to_owned())?;
        let reference_frame = reference
            .frames
            .first()
            .ok_or_else(|| "reference image did not produce a decoded frame".to_owned())?;
        let config = vision_opencv::OpenCvConfig::default();
        let descriptor_fingerprint = vision_opencv::descriptor_fingerprint(config);
        let mut engine = vision_opencv::OpenCvVisionEngine::new(config)?;
        let features = record
            .descriptor_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .and_then(|path| {
                vision_opencv::read_reference_features(
                    &path,
                    record.id.clone(),
                    class,
                    &record.sha256,
                    &descriptor_fingerprint,
                )
                .ok()
            })
            .unwrap_or(engine.extract_reference_with_source_sha256(
                record.id,
                class,
                record.sha256,
                reference_frame,
            )?);
        engine.render_match_debug(query_frame, reference_frame, &features)
    }

    #[cfg(not(feature = "opencv-backend"))]
    {
        let _ = (app, bytes, reference_id);
        Err("OpenCV SIFT/AKAZE backend is not compiled; debug matching is unavailable".to_owned())
    }
}

#[cfg(feature = "opencv-backend")]
fn classification_label_name(label: vision::ClassificationLabel) -> &'static str {
    match label {
        vision::ClassificationLabel::Nailong => "NAILONG",
        vision::ClassificationLabel::NaiwaFrog => "NAIWA_FROG",
        vision::ClassificationLabel::Other => "OTHER",
        vision::ClassificationLabel::Unknown => "UNKNOWN",
    }
}

#[cfg(feature = "opencv-backend")]
fn confidence_level_name(level: vision::ConfidenceLevel) -> &'static str {
    match level {
        vision::ConfidenceLevel::None => "NONE",
        vision::ConfidenceLevel::Low => "LOW",
        vision::ConfidenceLevel::Medium => "MEDIUM",
        vision::ConfidenceLevel::High => "HIGH",
        vision::ConfidenceLevel::VeryHigh => "VERY_HIGH",
    }
}

#[cfg(feature = "opencv-backend")]
fn read_verified_reference_bytes(record: &storage::ReferenceRecord) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(&record.file_path)
        .map_err(|error| format!("cannot read reference {}: {error}", record.id))?;
    let actual_sha256 = release::sha256_hex(&bytes);
    if !actual_sha256.eq_ignore_ascii_case(&record.sha256) {
        return Err(format!(
            "reference {} bytes do not match the stored SHA-256; re-add this reference before classification",
            record.id
        ));
    }
    Ok(bytes)
}

fn current_timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix:{}", duration.as_secs()),
        Err(_) => "unix:0".to_owned(),
    }
}

#[tauri::command]
fn initialize_storage(app: tauri::AppHandle) -> Result<StorageInfo, String> {
    let database = open_database(&app)?;
    Ok(StorageInfo {
        path: database.path().to_string_lossy().into_owned(),
        schema_version: database.schema_version(),
    })
}

fn app_settings_from_database(database: &storage::AppDatabase) -> Result<AppSettings, String> {
    let (thresholds, developer_mode) =
        database.app_settings().map_err(|error| error.to_string())?;
    Ok(AppSettings {
        thresholds,
        developer_mode,
    })
}

#[tauri::command]
fn get_app_settings(app: tauri::AppHandle) -> Result<AppSettings, String> {
    let database = open_database(&app)?;
    app_settings_from_database(&database)
}

#[tauri::command]
fn set_app_settings(app: tauri::AppHandle, settings: AppSettings) -> Result<AppSettings, String> {
    let database = open_database(&app)?;
    database
        .save_app_settings(settings.thresholds, settings.developer_mode)
        .map_err(|error| error.to_string())?;
    app_settings_from_database(&database)
}

#[tauri::command]
fn vision_info(app: tauri::AppHandle) -> Result<VisionInfo, String> {
    let database = open_database(&app)?;
    let (backend, available, message) = vision_backend_status();
    Ok(VisionInfo {
        backend,
        available,
        message,
        reference_set_version: database
            .reference_set_version()
            .map_err(|error| error.to_string())?,
        nailong_reference_count: database
            .count_references("NAILONG")
            .map_err(|error| error.to_string())?,
        naiwa_frog_reference_count: database
            .count_references("NAIWA_FROG")
            .map_err(|error| error.to_string())?,
    })
}

#[tauri::command]
fn list_references(app: tauri::AppHandle) -> Result<Vec<ReferenceInfo>, String> {
    let database = open_database(&app)?;
    database
        .list_references()
        .map_err(|error| error.to_string())
        .map(|records| records.into_iter().map(reference_info).collect())
}

#[tauri::command]
fn read_reference(app: tauri::AppHandle, id: String) -> Result<Vec<u8>, String> {
    if id.trim().is_empty() {
        return Err("reference id cannot be empty".to_owned());
    }
    let database = open_database(&app)?;
    let record = database
        .list_references()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| "reference not found".to_owned())?;
    std::fs::read(&record.file_path)
        .map_err(|error| format!("cannot read reference {}: {error}", record.id))
}

#[tauri::command]
fn add_reference(
    app: tauri::AppHandle,
    class: String,
    bytes: Vec<u8>,
) -> Result<ReferenceInfo, String> {
    let class = parse_reference_class(&class)?;
    let manager = references::ReferenceManager::new(reference_root(&app)?);
    let asset = manager.add(class, &bytes)?;
    #[cfg(feature = "opencv-backend")]
    let descriptor_path = match write_descriptor_cache(&app, &asset, class, &bytes) {
        Ok(path) => Some(path),
        Err(error) => {
            let _ = std::fs::remove_file(&asset.file_path);
            return Err(error);
        }
    };
    #[cfg(not(feature = "opencv-backend"))]
    let descriptor_path: Option<PathBuf> = None;
    let database = open_database(&app)?;
    let record = storage::ReferenceRecord {
        id: asset.id.clone(),
        class: reference_class_name(class).to_owned(),
        file_path: asset.file_path.to_string_lossy().into_owned(),
        sha256: asset.sha256.clone(),
        phash: asset.phash.clone(),
        descriptor_path: descriptor_path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        width: asset.width,
        height: asset.height,
        created_at: current_timestamp(),
    };
    if let Err(error) = database.record_reference_and_bump_version(&record) {
        if let Some(path) = &descriptor_path {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(&asset.file_path);
        let message = if error.code == 19 {
            "这张图片已经属于另一类参考库，请为每个类别选择不同的参考图".to_owned()
        } else {
            error.to_string()
        };
        return Err(message);
    }
    Ok(reference_info(record))
}

#[tauri::command]
fn remove_reference(app: tauri::AppHandle, id: String) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("reference id cannot be empty".to_owned());
    }
    let database = open_database(&app)?;
    let record = database
        .list_references()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| "reference not found".to_owned())?;
    database
        .delete_reference_and_bump_version(&id)
        .map_err(|error| error.to_string())?;
    let mut cleanup_failures = Vec::new();
    if let Err(error) = std::fs::remove_file(&record.file_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            cleanup_failures.push(format!("image: {error}"));
        }
    }
    if let Some(path) = &record.descriptor_path {
        if let Err(error) = std::fs::remove_file(path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                cleanup_failures.push(format!("descriptor: {error}"));
            }
        }
    }
    if !cleanup_failures.is_empty() {
        eprintln!(
            "reference {} removed from the database; orphan cleanup deferred: {}",
            id,
            cleanup_failures.join("; ")
        );
    }
    Ok(())
}

fn reference_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("cannot resolve reference directory: {error}"))?
        .join("references");
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("cannot create reference directory: {error}"))?;
    Ok(root)
}

#[cfg(feature = "opencv-backend")]
fn write_descriptor_cache(
    app: &tauri::AppHandle,
    asset: &references::ReferenceAsset,
    class: vision::ReferenceClass,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let decoded = decoder::decode_image(bytes, image_policy::MAX_SAMPLE_FRAMES)?;
    let frame = decoded
        .frames
        .first()
        .ok_or_else(|| "reference image did not produce a decoded frame".to_owned())?;
    let mut engine =
        vision_opencv::OpenCvVisionEngine::new(vision_opencv::OpenCvConfig::default())?;
    let features = engine.extract_reference_with_source_sha256(
        asset.id.clone(),
        class,
        asset.sha256.clone(),
        frame,
    )?;
    let cache_root = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("cannot resolve descriptor cache directory: {error}"))?
        .join("reference-cache");
    let path = cache_root.join(format!("{}.desc", asset.id));
    vision_opencv::write_reference_features(&path, &features)?;
    Ok(path)
}

fn reference_info(record: storage::ReferenceRecord) -> ReferenceInfo {
    ReferenceInfo {
        id: record.id,
        class: record.class,
        file_path: record.file_path,
        sha256: record.sha256,
        phash: record.phash,
        descriptor_path: record.descriptor_path,
        width: record.width,
        height: record.height,
        created_at: record.created_at,
    }
}

fn parse_reference_class(value: &str) -> Result<vision::ReferenceClass, String> {
    match value {
        "NAILONG" => Ok(vision::ReferenceClass::Nailong),
        "NAIWA_FROG" => Ok(vision::ReferenceClass::NaiwaFrog),
        _ => Err("class must be NAILONG or NAIWA_FROG".to_owned()),
    }
}

fn reference_class_name(class: vision::ReferenceClass) -> &'static str {
    match class {
        vision::ReferenceClass::Nailong => "NAILONG",
        vision::ReferenceClass::NaiwaFrog => "NAIWA_FROG",
    }
}

#[cfg(feature = "opencv-backend")]
fn vision_backend_status() -> (&'static str, bool, &'static str) {
    ("opencv-sift-akaze", true, "OpenCV SIFT/AKAZE 后端已编译")
}

#[cfg(not(feature = "opencv-backend"))]
fn vision_backend_status() -> (&'static str, bool, &'static str) {
    (
        "opencv-sift-akaze-unavailable",
        false,
        "未编译 OpenCV 后端；当前不会生成识别结果",
    )
}

pub(crate) fn open_database(app: &tauri::AppHandle) -> Result<storage::AppDatabase, String> {
    let app_data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("cannot resolve app data directory: {error}"))?;
    std::fs::create_dir_all(&app_data_dir)
        .map_err(|error| format!("cannot create app data directory: {error}"))?;
    storage::AppDatabase::open(app_data_dir.join("app.db")).map_err(|error| error.to_string())
}

pub fn run() {
    let qq_state = qq_service::QqServiceState::default();
    tauri::Builder::default()
        .manage(qq_state)
        .invoke_handler(tauri::generate_handler![
            app_info,
            inspect_image,
            decode_image_summary,
            initialize_storage,
            get_app_settings,
            set_app_settings,
            vision_info,
            list_references,
            read_reference,
            add_reference,
            remove_reference,
            classify_image,
            debug_match_image,
            qq_service::qq_status,
            qq_service::connect_qq,
            qq_service::disconnect_qq,
            qq_service::set_qq_group_mode
        ])
        .run(tauri::generate_context!())
        .expect("error while running NLNF Classifier");
}
