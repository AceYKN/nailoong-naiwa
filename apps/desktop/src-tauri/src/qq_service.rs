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
    storage::{ModerationLogRecord, QQGroupRecord},
    vision::{ClassificationLabel, ClassificationResult, VisionThresholds},
};

const DEFAULT_GROUP_RECALL_THRESHOLD: f64 = 0.98;
const MAX_RECENT_EVENTS: usize = 100;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(50);

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
            groups: runtime.groups.clone(),
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
    }
}

#[tauri::command]
pub fn qq_status(app: AppHandle, state: State<'_, QqServiceState>) -> Result<QqStatus, String> {
    let mut status = state.snapshot();
    if !status.connected {
        if let Ok(database) = crate::open_database(&app) {
            if let Ok(groups) = database.list_qq_groups() {
                status.groups = groups.iter().map(group_view).collect();
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
        groups.push(group_view(&record));
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
    runtime.groups = groups;
    runtime.last_error = None;
    runtime.stop_sender = Some(stop_sender);
    runtime.worker = Some(worker);
    Ok(QqStatus {
        connected: runtime.connected,
        action_endpoint: runtime.action_endpoint.clone(),
        event_endpoint: runtime.event_endpoint.clone(),
        token_configured: runtime.token_configured,
        groups: runtime.groups.clone(),
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
        *group = group_view(&record);
    } else {
        runtime.groups.push(group_view(&record));
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
    let mode = match parse_group_mode(&group.mode) {
        Ok(mode) => mode,
        Err(error) => {
            state.set_error(error);
            return;
        }
    };
    if mode == GroupMode::Off {
        return;
    }

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

    let thresholds = thresholds_for_group(group.recall_threshold);
    let results = classified
        .iter()
        .map(|(_, result)| result.clone())
        .collect::<Vec<_>>();
    let moderation_event = moderation_engine.handle_message(
        adapter,
        message.message_id.clone(),
        mode,
        &results,
        thresholds,
    );
    let action = action_name(moderation_event.action);
    let decision = decision_name(moderation_event.decision);
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

fn group_view(record: &QQGroupRecord) -> QqGroupView {
    QqGroupView {
        group_id: record.group_id.clone(),
        group_name: record.group_name.clone(),
        mode: record.mode.clone(),
        recall_threshold: record.recall_threshold,
    }
}

fn log_event_view(record: &crate::storage::ModerationLogRecord) -> QqEventView {
    let label = match (record.nailong_score, record.naiwa_frog_score) {
        (Some(nailong), Some(frog)) if frog > nailong => Some("NAIWA_FROG".to_owned()),
        (Some(_), Some(_)) => Some("NAILONG".to_owned()),
        _ => None,
    };
    QqEventView {
        group_id: record.group_id.clone().unwrap_or_else(|| "-".to_owned()),
        message_id: record.message_id.clone().unwrap_or_else(|| "-".to_owned()),
        label,
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

fn thresholds_for_group(recall_threshold: f64) -> VisionThresholds {
    VisionThresholds {
        recall_threshold: recall_threshold as f32,
        ..VisionThresholds::default()
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
    Err("OpenCV SIFT backend is not compiled".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{best_summary, mode_name, parse_group_mode, QqServiceState};
    use crate::{
        moderation::GroupMode,
        vision::{ClassificationLabel, ClassificationResult, ConfidenceLevel},
    };

    #[test]
    fn status_starts_disconnected_without_a_token_or_groups() {
        let status = QqServiceState::default().snapshot();
        assert!(!status.connected);
        assert!(!status.token_configured);
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
}
