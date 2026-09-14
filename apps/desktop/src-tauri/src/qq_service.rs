//! Desktop-owned OneBot connection and moderation worker.
//!
//! The low-level adapter remains independently testable in `onebot.rs`. This
//! module is the Tauri integration boundary: it connects only to loopback,
//! keeps the token in memory, subscribes configured groups, classifies image
//! bytes locally, and persists an Observe/Recall audit row. The service is
//! stopped by default and never starts from `run()` without an explicit UI
//! command.

use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::{
    image_policy,
    moderation::{GroupMode, ModerationDecision},
    onebot::{LoopbackEndpoint, OneBotHttpAdapter, OneBotReverseListener},
    qq::{MessageModerationEngine, ModerationAction, QQAdapter, QQError},
    release,
    storage::{AppDatabase, ModerationLogRecord, QQGroupRecord},
    vision::{ClassificationLabel, ClassificationResult, VisionThresholds},
};

const DEFAULT_GROUP_RECALL_THRESHOLD: f64 = 0.98;
const MAX_RECENT_EVENTS: usize = 100;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(50);

fn auto_recall_available() -> bool {
    cfg!(feature = "auto-recall-release")
        && release::embedded_certificate()
            .is_some_and(|certificate| release::certificate_is_well_formed(&certificate))
}

fn auto_recall_available_for_database(
    database: &AppDatabase,
    token_configured: bool,
    recall_threshold: Option<f64>,
) -> bool {
    if !token_configured || !auto_recall_available() {
        return false;
    }
    #[cfg(feature = "opencv-backend")]
    {
        let Ok((vision_thresholds, _)) = database.app_settings() else {
            return false;
        };
        let config = crate::vision_opencv::OpenCvConfig {
            vision_thresholds,
            ..crate::vision_opencv::OpenCvConfig::default()
        };
        let descriptor_fingerprint = crate::vision_opencv::descriptor_fingerprint(config);
        let engine_fingerprint =
            crate::vision_opencv::engine_fingerprint(config, image_policy::MAX_SAMPLE_FRAMES);
        database
            .verified_reference_set_hash()
            .ok()
            .is_some_and(|hash| {
                release::certificate_matches_reference_set(
                    &hash,
                    &descriptor_fingerprint,
                    &engine_fingerprint,
                    recall_threshold,
                )
            })
    }
    #[cfg(not(feature = "opencv-backend"))]
    {
        let _ = (database, recall_threshold);
        false
    }
}

fn validate_requested_mode(mode: GroupMode) -> Result<(), String> {
    if mode == GroupMode::AutoRecall && !auto_recall_available() {
        return Err(
            "AUTO_RECALL 尚未开放：请先通过冻结验证门禁、嵌入验证凭证，并使用 auto-recall-release 特性构建"
                .to_owned(),
        );
    }
    Ok(())
}

fn effective_group_mode(mode: GroupMode) -> GroupMode {
    if mode == GroupMode::AutoRecall && !auto_recall_available() {
        // A stale database must not turn a normal build into an active recall
        // worker. Keep observing so the event remains visible and no delete
        // action can occur until the release feature is explicitly built.
        GroupMode::Observe
    } else {
        mode
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QqGroupView {
    pub group_id: String,
    pub group_name: String,
    pub mode: String,
    pub recall_threshold: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QqEventView {
    pub group_id: String,
    pub message_id: String,
    pub label: Option<String>,
    pub nailong_score: f32,
    pub naiwa_frog_score: f32,
    pub decision: String,
    pub action: String,
    pub classified_images: u32,
    pub failed_images: u32,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QqStatus {
    pub connected: bool,
    pub action_endpoint: Option<String>,
    pub event_endpoint: Option<String>,
    pub token_configured: bool,
    pub auto_recall_available: bool,
    pub groups: Vec<QqGroupView>,
    pub recent_events: Vec<QqEventView>,
    pub last_error: Option<String>,
}

#[derive(Default)]
struct Runtime {
    connected: bool,
    action_endpoint: Option<String>,
    event_endpoint: Option<String>,
    token_configured: bool,
    auto_recall_available: bool,
    groups: Vec<QqGroupView>,
    recent_events: VecDeque<QqEventView>,
    last_error: Option<String>,
    stop_sender: Option<mpsc::Sender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

/// Cloneable Tauri-managed state. The OneBot token is intentionally absent
/// from this status object and is retained only by the worker's adapter.
#[derive(Clone, Default)]
pub struct QqServiceState {
    inner: Arc<Mutex<Runtime>>,
}

impl QqServiceState {
    fn snapshot(&self) -> QqStatus {
        let runtime = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        QqStatus {
            connected: runtime.connected,
            action_endpoint: runtime.action_endpoint.clone(),
            event_endpoint: runtime.event_endpoint.clone(),
            token_configured: runtime.token_configured,
            auto_recall_available: runtime.auto_recall_available,
            groups: runtime
                .groups
                .iter()
                .map(|group| normalize_group_view(group, runtime.auto_recall_available))
                .collect(),
            recent_events: runtime.recent_events.iter().cloned().collect(),
            last_error: runtime.last_error.clone(),
        }
    }

    fn set_error(&self, error: impl Into<String>) {
        let mut runtime = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        runtime.last_error = Some(error.into());
    }

    fn push_event(&self, event: QqEventView) {
        let mut runtime = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if runtime.recent_events.len() >= MAX_RECENT_EVENTS {
            runtime.recent_events.pop_front();
        }
        runtime.recent_events.push_back(event);
        runtime.last_error = None;
    }

    fn worker_stopped(&self) {
        let mut runtime = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        runtime.connected = false;
        runtime.stop_sender = None;
        runtime.action_endpoint = None;
        runtime.event_endpoint = None;
        runtime.token_configured = false;
        runtime.auto_recall_available = false;
    }
}

#[tauri::command]
pub fn qq_status(app: AppHandle, state: State<'_, QqServiceState>) -> Result<QqStatus, String> {
    let mut status = state.snapshot();
    if let Ok(database) = crate::open_database(&app) {
        status.auto_recall_available =
            auto_recall_available_for_database(&database, status.token_configured, None);
        if status.connected {
            status.groups = status
                .groups
                .iter()
                .map(|group| normalize_group_view(group, status.auto_recall_available))
                .collect();
        } else {
            if let Ok(groups) = database.list_qq_groups() {
                status.groups = groups
                    .iter()
                    .map(|group| {
                        let available = auto_recall_available_for_database(
                            &database,
                            status.token_configured,
                            Some(group.recall_threshold),
                        );
                        group_view(group, available)
                    })
                    .collect();
            }
            if status.recent_events.is_empty() {
                if let Ok(logs) = database.recent_moderation_logs(MAX_RECENT_EVENTS) {
                    status.recent_events = logs.iter().map(log_event_view).collect();
                }
            }
        }
    }
    Ok(status)
}

#[tauri::command]
pub fn connect_qq(
    app: AppHandle,
    state: State<'_, QqServiceState>,
    action_endpoint: String,
    event_endpoint: String,
    token: Option<String>,
) -> Result<QqStatus, String> {
    let service = state.inner.clone();
    let stale_worker = {
        let mut runtime = service.lock().unwrap_or_else(|poison| poison.into_inner());
        if runtime.connected {
            return Err("QQ Adapter 已连接，请先断开当前连接".to_owned());
        }
        if runtime
            .worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            runtime.worker.take()
        } else {
            None
        }
    };
    if let Some(worker) = stale_worker {
        worker
            .join()
            .map_err(|_| "上一次 QQ worker 未能正常结束".to_owned())?;
    }

    let action = LoopbackEndpoint::parse(action_endpoint.trim()).map_err(qq_error)?;
    let event = LoopbackEndpoint::parse(event_endpoint.trim()).map_err(qq_error)?;
    if action.display_url() == event.display_url() {
        return Err("QQ API 地址与事件监听地址不能完全相同，请使用两个本地端口".to_owned());
    }
    let token = token.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    });

    let mut adapter = OneBotHttpAdapter::new(action.clone(), token.clone()).map_err(qq_error)?;
    adapter.connect().map_err(qq_error)?;
    let remote_groups = adapter.get_group_list().map_err(qq_error)?;
    let database = crate::open_database(&app)?;
    let auto_recall_ready = auto_recall_available_for_database(&database, token.is_some(), None);
    let mut groups = Vec::with_capacity(remote_groups.len());
    for group in &remote_groups {
        adapter
            .subscribe_group_messages(&group.group_id)
            .map_err(qq_error)?;
        let record = match database
            .find_qq_group(&group.group_id)
            .map_err(|error| error.to_string())?
        {
            Some(existing) => existing,
            None => {
                let now = timestamp();
                let created = QQGroupRecord {
                    group_id: group.group_id.clone(),
                    group_name: group.group_name.clone(),
                    mode: "OFF".to_owned(),
                    recall_threshold: DEFAULT_GROUP_RECALL_THRESHOLD,
                    created_at: now.clone(),
                    updated_at: now,
                };
                database
                    .upsert_qq_group(&created)
                    .map_err(|error| error.to_string())?;
                created
            }
        };
        let group_ready = auto_recall_available_for_database(
            &database,
            token.is_some(),
            Some(record.recall_threshold),
        );
        groups.push(group_view(&record, group_ready));
    }

    let listener = OneBotReverseListener::bind(event.clone(), token.clone()).map_err(qq_error)?;
    listener.set_nonblocking(true).map_err(qq_error)?;
    let (stop_sender, stop_receiver) = mpsc::channel();
    let worker_state = QqServiceState {
        inner: service.clone(),
    };
    let worker_app = app.clone();
    let worker = thread::Builder::new()
        .name("nlnf-qq-worker".to_owned())
        .spawn(move || run_worker(worker_app, worker_state, adapter, listener, stop_receiver))
        .map_err(|error| format!("cannot start QQ worker: {error}"))?;

    let mut runtime = service.lock().unwrap_or_else(|poison| poison.into_inner());
    runtime.connected = true;
    runtime.action_endpoint = Some(action.display_url());
    runtime.event_endpoint = Some(event.display_url());
    runtime.token_configured = token.is_some();
    runtime.auto_recall_available = auto_recall_ready;
    runtime.groups = groups;
    runtime.last_error = None;
    runtime.stop_sender = Some(stop_sender);
    runtime.worker = Some(worker);
    Ok(QqStatus {
        connected: runtime.connected,
        action_endpoint: runtime.action_endpoint.clone(),
        event_endpoint: runtime.event_endpoint.clone(),
        token_configured: runtime.token_configured,
        auto_recall_available: runtime.auto_recall_available,
        groups: runtime
            .groups
            .iter()
            .map(|group| normalize_group_view(group, runtime.auto_recall_available))
            .collect(),
        recent_events: runtime.recent_events.iter().cloned().collect(),
        last_error: runtime.last_error.clone(),
    })
}

#[tauri::command]
pub fn disconnect_qq(state: State<'_, QqServiceState>) -> Result<QqStatus, String> {
    let (stop_sender, worker) = {
        let mut runtime = state
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        runtime.connected = false;
        runtime.action_endpoint = None;
        runtime.event_endpoint = None;
        runtime.token_configured = false;
        (runtime.stop_sender.take(), runtime.worker.take())
    };
    if let Some(sender) = stop_sender {
        let _ = sender.send(());
    }
    if let Some(worker) = worker {
        worker
            .join()
            .map_err(|_| "QQ worker thread did not stop cleanly".to_owned())?;
    }
    Ok(state.snapshot())
}

#[tauri::command]
pub fn set_qq_group_mode(
    app: AppHandle,
    state: State<'_, QqServiceState>,
    group_id: String,
    group_name: String,
    mode: String,
    recall_threshold: Option<f64>,
) -> Result<QqStatus, String> {
    let group_id = group_id.trim().to_owned();
    if group_id.is_empty() {
        return Err("QQ 群号不能为空".to_owned());
    }
    let mode = parse_group_mode(&mode)?;
    validate_requested_mode(mode)?;
    let token_configured = state
        .inner
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .token_configured;
    let database = crate::open_database(&app)?;
    let previous = database
        .find_qq_group(&group_id)
        .map_err(|error| error.to_string())?;
    let now = timestamp();
    let record = QQGroupRecord {
        group_id: group_id.clone(),
        group_name: if group_name.trim().is_empty() {
            previous
                .as_ref()
                .map(|value| value.group_name.clone())
                .unwrap_or_else(|| group_id.clone())
        } else {
            group_name.trim().to_owned()
        },
        mode: mode_name(mode).to_owned(),
        recall_threshold: recall_threshold
            .or_else(|| previous.as_ref().map(|value| value.recall_threshold))
            .unwrap_or(DEFAULT_GROUP_RECALL_THRESHOLD),
        created_at: previous
            .as_ref()
            .map(|value| value.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
    };
    let group_ready = auto_recall_available_for_database(
        &database,
        token_configured,
        Some(record.recall_threshold),
    );
    if mode == GroupMode::AutoRecall && !group_ready {
        return Err(
            "AUTO_RECALL 当前不可用：需要 Token、验证凭证、未变更的参考库和不低于验证门槛的阈值"
                .to_owned(),
        );
    }
    database
        .upsert_qq_group(&record)
        .map_err(|error| error.to_string())?;

    let mut runtime = state
        .inner
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(group) = runtime
        .groups
        .iter_mut()
        .find(|value| value.group_id == group_id)
    {
        *group = group_view(&record, group_ready);
    } else {
        runtime.groups.push(group_view(&record, group_ready));
    }
    runtime
        .groups
        .sort_by(|left, right| left.group_id.cmp(&right.group_id));
    runtime.last_error = None;
    drop(runtime);
    Ok(state.snapshot())
}

fn run_worker(
    app: AppHandle,
    state: QqServiceState,
    mut adapter: OneBotHttpAdapter,
    listener: OneBotReverseListener,
    stop_receiver: mpsc::Receiver<()>,
) {
    let mut moderation_engine = MessageModerationEngine::default();
    loop {
        match stop_receiver.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break,
            Err(mpsc::TryRecvError::Empty) => {}
        }

        match listener.try_accept_event() {
            Ok(Some(payload)) => {
                if let Err(error) = adapter.ingest_event(&payload).map(|_| ()) {
                    state.set_error(format!("OneBot 事件未加入队列：{error}"));
                    continue;
                }
                while let Some(message) = adapter.next_group_message() {
                    handle_group_message(
                        &app,
                        &state,
                        &mut adapter,
                        &mut moderation_engine,
                        message,
                    );
                }
            }
            Ok(None) => thread::sleep(WORKER_POLL_INTERVAL),
            Err(error) => {
                state.set_error(error.to_string());
                thread::sleep(WORKER_POLL_INTERVAL);
            }
        }
    }
    adapter.disconnect();
    state.worker_stopped();
}

fn handle_group_message(
    app: &AppHandle,
    state: &QqServiceState,
    adapter: &mut OneBotHttpAdapter,
    moderation_engine: &mut MessageModerationEngine,
    message: crate::onebot::OneBotGroupMessage,
) {
    let database = match crate::open_database(app) {
        Ok(database) => database,
        Err(error) => {
            state.set_error(error);
            return;
        }
    };
    let Some(group) = (match database.find_qq_group(&message.group_id) {
        Ok(group) => group,
        Err(error) => {
            state.set_error(error.to_string());
            return;
        }
    }) else {
        return;
    };
    let vision_thresholds = match database.app_settings() {
        Ok((thresholds, _)) => thresholds,
        Err(error) => {
            state.set_error(format!("视觉设置无效，已跳过 QQ 图片处理：{error}"));
            return;
        }
    };
    let configured_mode = match parse_group_mode(&group.mode) {
        Ok(mode) => mode,
        Err(error) => {
            state.set_error(error);
            return;
        }
    };
    let mode = if configured_mode == GroupMode::AutoRecall
        && !auto_recall_available_for_database(
            &database,
            adapter.token_configured(),
            Some(group.recall_threshold),
        ) {
        // A reference-bank or threshold change invalidates the release
        // certificate immediately. Continue in OBSERVE so the event remains
        // visible, but never call delete_msg with stale evidence.
        GroupMode::Observe
    } else {
        effective_group_mode(configured_mode)
    };
    if mode == GroupMode::Off {
        return;
    }

    let persistent_claim = if mode == GroupMode::AutoRecall {
        match database.claim_moderation_message(
            &message.group_id,
            &message.message_id,
            &timestamp(),
        ) {
            Ok(true) => true,
            Ok(false) => {
                state.push_event(QqEventView {
                    group_id: message.group_id,
                    message_id: message.message_id,
                    label: None,
                    nailong_score: 0.0,
                    naiwa_frog_score: 0.0,
                    decision: "SKIP".to_owned(),
                    action: "SKIPPED_ALREADY_PROCESSED".to_owned(),
                    classified_images: 0,
                    failed_images: 0,
                    created_at: timestamp(),
                });
                return;
            }
            Err(error) => {
                state.set_error(format!("QQ 消息幂等记录失败，已跳过撤回：{error}"));
                return;
            }
        }
    } else {
        false
    };

    let mut classified = Vec::new();
    let mut failed_images = 0_u32;
    for image in &message.images {
        let bytes = match adapter.download_image(image) {
            Ok(bytes) => bytes,
            Err(error) => {
                failed_images += 1;
                state.set_error(format!("QQ 图片 {} 获取失败：{error}", image.image_id));
                continue;
            }
        };
        let sha256 = image_policy::inspect_image(&bytes)
            .ok()
            .map(|value| value.sha256);
        match classify_bytes_for_qq(app, &bytes) {
            Ok(result) => classified.push((sha256, result)),
            Err(error) => {
                failed_images += 1;
                state.set_error(format!("QQ 图片 {} 识别失败：{error}", image.image_id));
            }
        }
    }

    let thresholds = thresholds_for_group(vision_thresholds, group.recall_threshold);
    let moderation_mode = crate::qq::mode_after_image_failures(mode, failed_images as usize);
    let results = classified
        .iter()
        .map(|(_, result)| result.clone())
        .collect::<Vec<_>>();
    if persistent_claim {
        let state_name = if failed_images == 0
            && results
                .iter()
                .any(|result| crate::vision::recall_eligible(result, thresholds))
        {
            "RECALL_ATTEMPTED"
        } else {
            "SEEN"
        };
        if let Err(error) = database.update_moderation_message_state(
            &message.group_id,
            &message.message_id,
            state_name,
            &timestamp(),
        ) {
            state.set_error(format!("QQ 消息幂等状态写入失败，已跳过撤回：{error}"));
            return;
        }
    }
    let moderation_event = moderation_engine.handle_message_in_group(
        adapter,
        message.group_id.clone(),
        message.message_id.clone(),
        moderation_mode,
        &results,
        thresholds,
    );
    let action = action_name(moderation_event.action.clone());
    let decision = decision_name(moderation_event.decision);
    if persistent_claim {
        let message_state = match moderation_event.action {
            ModerationAction::Recalled => "RECALLED",
            ModerationAction::RecallFailed => "RECALL_FAILED",
            ModerationAction::RecallSkippedAdapterOffline
            | ModerationAction::RecallSkippedAlreadyProcessed => "RECALL_ATTEMPTED",
            ModerationAction::None | ModerationAction::Observed => "SEEN",
        };
        if let Err(error) = database.update_moderation_message_state(
            &message.group_id,
            &message.message_id,
            message_state,
            &timestamp(),
        ) {
            state.set_error(format!("QQ 消息幂等结果写入失败：{error}"));
        }
    }
    let reference_set_version = database.reference_set_version().ok();
    if classified.is_empty() {
        let _ = database.append_moderation_log(&ModerationLogRecord {
            group_id: Some(message.group_id.clone()),
            message_id: Some(message.message_id.clone()),
            user_id: None,
            image_sha256: None,
            nailong_score: None,
            naiwa_frog_score: None,
            reference_set_version,
            classification_label: None,
            decision: decision.to_owned(),
            action_result: Some(action.to_owned()),
            created_at: timestamp(),
        });
    } else {
        for (sha256, result) in &classified {
            if let Err(error) = database.append_moderation_log(&ModerationLogRecord {
                group_id: Some(message.group_id.clone()),
                message_id: Some(message.message_id.clone()),
                user_id: None,
                image_sha256: sha256.clone(),
                nailong_score: Some(f64::from(result.nailong_score)),
                naiwa_frog_score: Some(f64::from(result.naiwa_frog_score)),
                reference_set_version,
                classification_label: Some(label_name(result.label).to_owned()),
                decision: decision.to_owned(),
                action_result: Some(action.to_owned()),
                created_at: timestamp(),
            }) {
                state.set_error(format!("QQ 审核日志写入失败：{error}"));
            }
        }
    }

    let (label, nailong_score, naiwa_frog_score) = best_summary(&results);
    state.push_event(QqEventView {
        group_id: message.group_id,
        message_id: message.message_id,
        label,
        nailong_score,
        naiwa_frog_score,
        decision: decision.to_owned(),
        action: action.to_owned(),
        classified_images: results.len() as u32,
        failed_images,
        created_at: timestamp(),
    });
}

fn best_summary(results: &[ClassificationResult]) -> (Option<String>, f32, f32) {
    let nailong_score = results
        .iter()
        .map(|result| result.nailong_score)
        .fold(0.0_f32, f32::max);
    let naiwa_frog_score = results
        .iter()
        .map(|result| result.naiwa_frog_score)
        .fold(0.0_f32, f32::max);
    let label = results
        .iter()
        .max_by(|left, right| {
            left.nailong_score
                .max(left.naiwa_frog_score)
                .total_cmp(&right.nailong_score.max(right.naiwa_frog_score))
        })
        .map(|result| label_name(result.label).to_owned());
    (label, nailong_score, naiwa_frog_score)
}

fn group_view(record: &QQGroupRecord, auto_recall_ready: bool) -> QqGroupView {
    QqGroupView {
        group_id: record.group_id.clone(),
        group_name: record.group_name.clone(),
        mode: if record.mode == "AUTO_RECALL" && !auto_recall_ready {
            "OBSERVE".to_owned()
        } else {
            effective_mode_name(&record.mode)
        },
        recall_threshold: record.recall_threshold,
    }
}

fn effective_mode_name(value: &str) -> String {
    parse_group_mode(value)
        .map(effective_group_mode)
        .map(mode_name)
        .unwrap_or("OFF")
        .to_owned()
}

fn normalize_group_view(group: &QqGroupView, auto_recall_ready: bool) -> QqGroupView {
    QqGroupView {
        group_id: group.group_id.clone(),
        group_name: group.group_name.clone(),
        mode: if group.mode == "AUTO_RECALL" && !auto_recall_ready {
            "OBSERVE".to_owned()
        } else {
            effective_mode_name(&group.mode)
        },
        recall_threshold: group.recall_threshold,
    }
}

fn log_event_view(record: &crate::storage::ModerationLogRecord) -> QqEventView {
    QqEventView {
        group_id: record.group_id.clone().unwrap_or_else(|| "-".to_owned()),
        message_id: record.message_id.clone().unwrap_or_else(|| "-".to_owned()),
        // Legacy rows may not have a stored label. Do not reconstruct one
        // from scores: a score pair is not the same thing as the decision.
        label: record.classification_label.clone(),
        nailong_score: record.nailong_score.unwrap_or(0.0) as f32,
        naiwa_frog_score: record.naiwa_frog_score.unwrap_or(0.0) as f32,
        decision: record.decision.clone(),
        action: record
            .action_result
            .clone()
            .unwrap_or_else(|| "NONE".to_owned()),
        classified_images: u32::from(record.image_sha256.is_some()),
        failed_images: u32::from(record.image_sha256.is_none()),
        created_at: record.created_at.clone(),
    }
}

fn parse_group_mode(value: &str) -> Result<GroupMode, String> {
    match value {
        "OFF" => Ok(GroupMode::Off),
        "OBSERVE" => Ok(GroupMode::Observe),
        "AUTO_RECALL" => Ok(GroupMode::AutoRecall),
        _ => Err("QQ 群模式必须是 OFF、OBSERVE 或 AUTO_RECALL".to_owned()),
    }
}

fn mode_name(mode: GroupMode) -> &'static str {
    match mode {
        GroupMode::Off => "OFF",
        GroupMode::Observe => "OBSERVE",
        GroupMode::AutoRecall => "AUTO_RECALL",
    }
}

fn thresholds_for_group(base: VisionThresholds, recall_threshold: f64) -> VisionThresholds {
    VisionThresholds {
        recall_threshold: recall_threshold as f32,
        ..base
    }
}

fn label_name(label: ClassificationLabel) -> &'static str {
    match label {
        ClassificationLabel::Nailong => "NAILONG",
        ClassificationLabel::NaiwaFrog => "NAIWA_FROG",
        ClassificationLabel::Other => "OTHER",
        ClassificationLabel::Unknown => "UNKNOWN",
    }
}

fn decision_name(decision: ModerationDecision) -> &'static str {
    match decision {
        ModerationDecision::Pass => "PASS",
        ModerationDecision::ObserveRecall => "WOULD_RECALL",
        ModerationDecision::Recall => "RECALL",
        ModerationDecision::Skip => "SKIP",
    }
}

fn action_name(action: ModerationAction) -> &'static str {
    match action {
        ModerationAction::None => "NONE",
        ModerationAction::Observed => "OBSERVED",
        ModerationAction::Recalled => "RECALLED",
        ModerationAction::RecallSkippedAlreadyProcessed => "SKIPPED_ALREADY_PROCESSED",
        ModerationAction::RecallSkippedAdapterOffline => "SKIPPED_ADAPTER_OFFLINE",
        ModerationAction::RecallFailed => "RECALL_FAILED",
    }
}

fn qq_error(error: QQError) -> String {
    error.to_string()
}

fn timestamp() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix:{}", duration.as_secs()),
        Err(_) => "unix:0".to_owned(),
    }
}

#[cfg(feature = "opencv-backend")]
fn classify_bytes_for_qq(app: &AppHandle, bytes: &[u8]) -> Result<ClassificationResult, String> {
    crate::classify_image_bytes(app, bytes, None)
}

#[cfg(not(feature = "opencv-backend"))]
fn classify_bytes_for_qq(_app: &AppHandle, _bytes: &[u8]) -> Result<ClassificationResult, String> {
    Err("OpenCV SIFT/AKAZE backend is not compiled".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        auto_recall_available, best_summary, effective_group_mode, mode_name, normalize_group_view,
        parse_group_mode, validate_requested_mode, QqGroupView, QqServiceState,
    };
    use crate::{
        moderation::GroupMode,
        release,
        storage::ModerationLogRecord,
        vision::{ClassificationLabel, ClassificationResult, ConfidenceLevel},
    };

    #[test]
    fn status_starts_disconnected_without_a_token_or_groups() {
        let status = QqServiceState::default().snapshot();
        assert!(!status.connected);
        assert!(!status.token_configured);
        assert!(!status.auto_recall_available);
        assert!(status.groups.is_empty());
        assert!(status.recent_events.is_empty());
    }

    #[test]
    fn group_modes_round_trip_and_reject_unknown_values() {
        for (name, mode) in [
            ("OFF", GroupMode::Off),
            ("OBSERVE", GroupMode::Observe),
            ("AUTO_RECALL", GroupMode::AutoRecall),
        ] {
            assert_eq!(parse_group_mode(name).unwrap(), mode);
            assert_eq!(mode_name(mode), name);
        }
        assert!(parse_group_mode("RECALL_EVERYTHING").is_err());
    }

    #[test]
    fn auto_recall_requires_the_explicit_release_feature() {
        let certificate_available = release::embedded_certificate()
            .is_some_and(|certificate| release::certificate_is_well_formed(&certificate));
        assert_eq!(
            auto_recall_available(),
            cfg!(feature = "auto-recall-release") && certificate_available
        );
        assert!(validate_requested_mode(GroupMode::Off).is_ok());
        assert!(validate_requested_mode(GroupMode::Observe).is_ok());
        if auto_recall_available() {
            assert!(validate_requested_mode(GroupMode::AutoRecall).is_ok());
            assert_eq!(
                effective_group_mode(GroupMode::AutoRecall),
                GroupMode::AutoRecall
            );
        } else {
            assert!(validate_requested_mode(GroupMode::AutoRecall).is_err());
            assert_eq!(
                effective_group_mode(GroupMode::AutoRecall),
                GroupMode::Observe
            );
        }
    }

    #[test]
    fn stale_auto_recall_is_presented_as_observe_without_the_release_feature() {
        let group = QqGroupView {
            group_id: "10001".to_owned(),
            group_name: "test".to_owned(),
            mode: "AUTO_RECALL".to_owned(),
            recall_threshold: 0.98,
        };
        let normalized = normalize_group_view(&group, auto_recall_available());
        let expected = if auto_recall_available() {
            "AUTO_RECALL"
        } else {
            "OBSERVE"
        };
        assert_eq!(normalized.mode, expected);
    }

    #[test]
    fn event_summary_uses_the_strongest_result_and_keeps_both_scores() {
        let nailong = ClassificationResult {
            label: ClassificationLabel::Nailong,
            nailong_score: 0.91,
            naiwa_frog_score: 0.03,
            best_nailong_reference: Some("NL-1".to_owned()),
            best_naiwa_reference: None,
            best_match: None,
            inlier_count: 20,
            inlier_ratio: 0.8,
            coverage: 0.4,
            reprojection_error: 1.0,
            confidence_level: ConfidenceLevel::High,
            geometry_valid: true,
            sampled_frame_count: 1,
            qualifying_naiwa_frame_count: 0,
        };
        let frog = ClassificationResult {
            label: ClassificationLabel::NaiwaFrog,
            nailong_score: 0.02,
            naiwa_frog_score: 0.96,
            best_nailong_reference: None,
            best_naiwa_reference: Some("NF-1".to_owned()),
            best_match: None,
            inlier_count: 21,
            inlier_ratio: 0.82,
            coverage: 0.42,
            reprojection_error: 1.0,
            confidence_level: ConfidenceLevel::VeryHigh,
            geometry_valid: true,
            sampled_frame_count: 1,
            qualifying_naiwa_frame_count: 1,
        };
        let (label, nailong_score, naiwa_score) = best_summary(&[nailong, frog]);
        assert_eq!(label.as_deref(), Some("NAIWA_FROG"));
        assert_eq!(nailong_score, 0.91);
        assert_eq!(naiwa_score, 0.96);
    }

    #[test]
    fn persisted_legacy_event_does_not_infer_a_label_from_scores() {
        let event = super::log_event_view(&ModerationLogRecord {
            group_id: Some("group-1".to_owned()),
            message_id: Some("message-1".to_owned()),
            user_id: None,
            image_sha256: Some("a".repeat(64)),
            nailong_score: Some(0.91),
            naiwa_frog_score: Some(0.03),
            reference_set_version: Some(1),
            classification_label: None,
            decision: "PASS".to_owned(),
            action_result: Some("NONE".to_owned()),
            created_at: "now".to_owned(),
        });
        assert_eq!(event.label, None);
        assert_eq!(event.nailong_score, 0.91);
    }
}
