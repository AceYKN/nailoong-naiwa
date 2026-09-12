use std::collections::{HashMap, HashSet};

use crate::{
    moderation::{GroupMode, ModerationDecision},
    vision::{self, ClassificationResult, VisionThresholds},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QQGroup {
    pub group_id: String,
    pub group_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QQImage {
    pub image_id: String,
    pub source_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QQError {
    NotConnected,
    ImageNotFound(String),
    RecallFailed(String),
    Unsupported(String),
}

impl std::fmt::Display for QQError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => formatter.write_str("QQ adapter is not connected"),
            Self::ImageNotFound(image_id) => write!(formatter, "mock image is missing: {image_id}"),
            Self::RecallFailed(reason) => write!(formatter, "recall failed: {reason}"),
            Self::Unsupported(reason) => {
                write!(formatter, "QQ adapter operation is unsupported: {reason}")
            }
        }
    }
}

impl std::error::Error for QQError {}

/// The classifier and decision engine depend on this boundary, never on a QQ
/// client implementation. A real adapter can replace the mock later.
pub trait QQAdapter {
    fn connect(&mut self) -> Result<(), QQError>;
    fn disconnect(&mut self);
    fn is_connected(&self) -> bool;
    fn subscribe_group_messages(&mut self, group_id: &str) -> Result<(), QQError>;
    fn download_image(&self, image: &QQImage) -> Result<Vec<u8>, QQError>;
    fn recall_message(&mut self, message_id: &str) -> Result<(), QQError>;
    fn get_group_list(&self) -> Result<Vec<QQGroup>, QQError>;
}

/// In-memory adapter used by tests and the future Observe UI. It starts
/// disconnected and contains no real network or QQ implementation.
#[derive(Debug, Default)]
pub struct MockQQAdapter {
    connected: bool,
    groups: Vec<QQGroup>,
    subscribed_groups: HashSet<String>,
    images: HashMap<String, Vec<u8>>,
    recalled_messages: Vec<String>,
    recall_error: Option<String>,
}

impl MockQQAdapter {
    pub fn add_group(&mut self, group_id: impl Into<String>, group_name: impl Into<String>) {
        self.groups.push(QQGroup {
            group_id: group_id.into(),
            group_name: group_name.into(),
        });
    }

    pub fn add_image(&mut self, source_key: impl Into<String>, bytes: Vec<u8>) {
        self.images.insert(source_key.into(), bytes);
    }

    pub fn set_recall_error(&mut self, reason: impl Into<String>) {
        self.recall_error = Some(reason.into());
    }

    pub fn recalled_messages(&self) -> &[String] {
        &self.recalled_messages
    }

    pub fn subscribed_groups(&self) -> &HashSet<String> {
        &self.subscribed_groups
    }
}

impl QQAdapter for MockQQAdapter {
    fn connect(&mut self) -> Result<(), QQError> {
        self.connected = true;
        Ok(())
    }

    fn disconnect(&mut self) {
        self.connected = false;
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    fn subscribe_group_messages(&mut self, group_id: &str) -> Result<(), QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        self.subscribed_groups.insert(group_id.to_owned());
        Ok(())
    }

    fn download_image(&self, image: &QQImage) -> Result<Vec<u8>, QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        self.images
            .get(&image.source_key)
            .cloned()
            .ok_or_else(|| QQError::ImageNotFound(image.image_id.clone()))
    }

    fn recall_message(&mut self, message_id: &str) -> Result<(), QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        if let Some(reason) = &self.recall_error {
            return Err(QQError::RecallFailed(reason.clone()));
        }
        self.recalled_messages.push(message_id.to_owned());
        Ok(())
    }

    fn get_group_list(&self) -> Result<Vec<QQGroup>, QQError> {
        if !self.connected {
            return Err(QQError::NotConnected);
        }
        Ok(self.groups.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModerationAction {
    None,
    Observed,
    Recalled,
    RecallSkippedAlreadyProcessed,
    RecallSkippedAdapterOffline,
    RecallFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationEvent {
    pub message_id: String,
    pub decision: ModerationDecision,
    pub action: ModerationAction,
}

/// The vision-facing seam for the QQ message pipeline. The adapter owns image
/// bytes; the classifier only receives one bounded download at a time and
/// returns the complete geometry-verified v2 classification result.
pub trait QQImageClassifier {
    fn classify(&self, image: &QQImage, bytes: &[u8]) -> Result<ClassificationResult, QQError>;
}

impl<F> QQImageClassifier for F
where
    F: Fn(&QQImage, &[u8]) -> Result<ClassificationResult, QQError>,
{
    fn classify(&self, image: &QQImage, bytes: &[u8]) -> Result<ClassificationResult, QQError> {
        self(image, bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePipelineOutcome {
    pub event: ModerationEvent,
    pub classified_images: usize,
    pub failed_image_ids: Vec<String>,
}

/// Message-level decision engine. A QQ message is the unit of recall even if
/// it contains several images. Failed recall attempts are recorded as
/// processed so the default retry count remains zero.
#[derive(Debug, Default)]
pub struct MessageModerationEngine {
    processed_message_ids: HashSet<String>,
    event_log: Vec<ModerationEvent>,
}

impl MessageModerationEngine {
    pub fn processed_message_ids(&self) -> &HashSet<String> {
        &self.processed_message_ids
    }

    /// Returns the bounded in-memory moderation audit trail. A host can copy
    /// these events into `moderation_log`; keeping the decision engine free of
    /// a database handle preserves the adapter boundary and testability.
    pub fn event_log(&self) -> &[ModerationEvent] {
        &self.event_log
    }

    pub fn handle_message<A: QQAdapter>(
        &mut self,
        adapter: &mut A,
        message_id: impl Into<String>,
        mode: GroupMode,
        classifications: &[ClassificationResult],
        thresholds: VisionThresholds,
    ) -> ModerationEvent {
        let event = self.decide_message(adapter, message_id, mode, classifications, thresholds);
        const MAX_EVENT_LOG: usize = 512;
        if self.event_log.len() >= MAX_EVENT_LOG {
            self.event_log.remove(0);
        }
        self.event_log.push(event.clone());
        event
    }

    fn decide_message<A: QQAdapter>(
        &mut self,
        adapter: &mut A,
        message_id: impl Into<String>,
        mode: GroupMode,
        classifications: &[ClassificationResult],
        thresholds: VisionThresholds,
    ) -> ModerationEvent {
        let message_id = message_id.into();
        let recall_candidate = classifications
            .iter()
            .any(|classification| vision::recall_eligible(classification, thresholds));
        let decision = match (mode, recall_candidate) {
            (GroupMode::Off, _) => ModerationDecision::Skip,
            (GroupMode::Observe, true) => ModerationDecision::ObserveRecall,
            (GroupMode::AutoRecall, true) => ModerationDecision::Recall,
            (GroupMode::Observe | GroupMode::AutoRecall, false) => ModerationDecision::Pass,
        };
        if decision != ModerationDecision::Recall {
            return ModerationEvent {
                message_id,
                decision,
                action: if decision == ModerationDecision::ObserveRecall {
                    ModerationAction::Observed
                } else {
                    ModerationAction::None
                },
            };
        }

        if !self.processed_message_ids.insert(message_id.clone()) {
            return ModerationEvent {
                message_id,
                decision,
                action: ModerationAction::RecallSkippedAlreadyProcessed,
            };
        }
        if !adapter.is_connected() {
            return ModerationEvent {
                message_id,
                decision,
                action: ModerationAction::RecallSkippedAdapterOffline,
            };
        }
        let action = match adapter.recall_message(&message_id) {
            Ok(()) => ModerationAction::Recalled,
            Err(_) => ModerationAction::RecallFailed,
        };
        ModerationEvent {
            message_id,
            decision,
            action,
        }
    }
}

/// Run one message through the adapter boundary and the message-level
/// decision engine. Failed downloads/classifications are recorded as failed
/// image IDs and omitted from scoring, so malformed or unavailable images
/// fail closed instead of causing a crash or inventing a high score.
pub fn process_message<A: QQAdapter, C: QQImageClassifier>(
    adapter: &mut A,
    engine: &mut MessageModerationEngine,
    message_id: impl Into<String>,
    mode: GroupMode,
    images: &[QQImage],
    classifier: &C,
    thresholds: VisionThresholds,
) -> MessagePipelineOutcome {
    let mut classifications = Vec::new();
    let mut failed_image_ids = Vec::new();
    for image in images {
        let bytes = match adapter.download_image(image) {
            Ok(bytes) => bytes,
            Err(_) => {
                failed_image_ids.push(image.image_id.clone());
                continue;
            }
        };
        match classifier.classify(image, &bytes) {
            Ok(classification) => classifications.push(classification),
            Err(_) => failed_image_ids.push(image.image_id.clone()),
        }
    }
    let classified_images = classifications.len();
    let event = engine.handle_message(adapter, message_id, mode, &classifications, thresholds);
    MessagePipelineOutcome {
        event,
        classified_images,
        failed_image_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        process_message, MessageModerationEngine, MessagePipelineOutcome, MockQQAdapter,
        ModerationAction, QQAdapter, QQError, QQImage,
    };
    use crate::{
        moderation::{GroupMode, ModerationDecision},
        vision::{ClassificationLabel, ConfidenceLevel, VisionThresholds},
    };

    fn frog_result(score: f32) -> crate::vision::ClassificationResult {
        crate::vision::ClassificationResult {
            label: ClassificationLabel::NaiwaFrog,
            nailong_score: 0.01,
            naiwa_frog_score: score,
            best_nailong_reference: None,
            best_naiwa_reference: Some("NF-SMOKE".to_owned()),
            best_match: None,
            inlier_count: 20,
            inlier_ratio: 0.80,
            coverage: 0.35,
            reprojection_error: 2.0,
            confidence_level: ConfidenceLevel::VeryHigh,
            geometry_valid: true,
            sampled_frame_count: 1,
            qualifying_naiwa_frame_count: 1,
        }
    }

    fn frog_classifier(
        _: &QQImage,
        _: &[u8],
    ) -> Result<crate::vision::ClassificationResult, QQError> {
        Ok(frog_result(0.99))
    }

    #[test]
    fn mock_adapter_is_safe_by_default_and_requires_connection() {
        let adapter = MockQQAdapter::default();
        assert!(!adapter.is_connected());
        assert_eq!(adapter.get_group_list(), Err(QQError::NotConnected));
        assert_eq!(
            adapter.download_image(&QQImage {
                image_id: "image-1".to_owned(),
                source_key: "source-1".to_owned(),
            }),
            Err(QQError::NotConnected)
        );
    }

    #[test]
    fn observe_only_records_a_candidate_without_adapter_side_effect() {
        let mut adapter = MockQQAdapter::default();
        let mut engine = MessageModerationEngine::default();
        let event = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::Observe,
            &[frog_result(0.99)],
            VisionThresholds::default(),
        );
        assert_eq!(event.decision, ModerationDecision::ObserveRecall);
        assert_eq!(event.action, ModerationAction::Observed);
        assert!(adapter.recalled_messages().is_empty());
        assert!(engine.processed_message_ids().is_empty());
        assert_eq!(engine.event_log(), &[event]);
    }

    #[test]
    fn auto_recall_is_at_most_once_for_a_message_with_many_images() {
        let mut adapter = MockQQAdapter::default();
        adapter.connect().expect("mock connects");
        let mut engine = MessageModerationEngine::default();
        let images = [frog_result(0.99), frog_result(0.995)];
        let first = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &images,
            VisionThresholds::default(),
        );
        let second = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &images,
            VisionThresholds::default(),
        );
        assert_eq!(first.action, ModerationAction::Recalled);
        assert_eq!(
            second.action,
            ModerationAction::RecallSkippedAlreadyProcessed
        );
        assert_eq!(adapter.recalled_messages(), &["message-1".to_owned()]);
    }

    #[test]
    fn offline_auto_recall_is_not_attempted_and_is_not_retried() {
        let mut adapter = MockQQAdapter::default();
        let mut engine = MessageModerationEngine::default();
        let first = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &[frog_result(0.99)],
            VisionThresholds::default(),
        );
        let second = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &[frog_result(0.99)],
            VisionThresholds::default(),
        );
        assert_eq!(first.action, ModerationAction::RecallSkippedAdapterOffline);
        assert_eq!(
            second.action,
            ModerationAction::RecallSkippedAlreadyProcessed
        );
        assert!(adapter.recalled_messages().is_empty());
    }

    #[test]
    fn recall_failure_is_logged_as_failure_without_retry() {
        let mut adapter = MockQQAdapter::default();
        adapter.connect().expect("mock connects");
        adapter.set_recall_error("no permission");
        let mut engine = MessageModerationEngine::default();
        let first = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &[frog_result(0.99)],
            VisionThresholds::default(),
        );
        let second = engine.handle_message(
            &mut adapter,
            "message-1",
            GroupMode::AutoRecall,
            &[frog_result(0.99)],
            VisionThresholds::default(),
        );
        assert_eq!(first.action, ModerationAction::RecallFailed);
        assert_eq!(
            second.action,
            ModerationAction::RecallSkippedAlreadyProcessed
        );
        assert!(adapter.recalled_messages().is_empty());
    }

    #[test]
    fn invalid_model_scores_fail_closed_without_recall() {
        let mut adapter = MockQQAdapter::default();
        adapter.connect().expect("mock connects");
        let mut engine = MessageModerationEngine::default();
        let event = engine.handle_message(
            &mut adapter,
            "message-invalid",
            GroupMode::AutoRecall,
            &[frog_result(f32::NAN)],
            VisionThresholds::default(),
        );
        assert_eq!(event.decision, ModerationDecision::Pass);
        assert_eq!(event.action, ModerationAction::None);
        assert!(adapter.recalled_messages().is_empty());
    }

    #[test]
    fn group_subscription_and_image_download_stay_inside_adapter_boundary() {
        let mut adapter = MockQQAdapter::default();
        adapter.add_group("group-1", "test group");
        adapter.add_image("source-1", vec![1, 2, 3]);
        adapter.connect().expect("mock connects");
        adapter
            .subscribe_group_messages("group-1")
            .expect("subscription succeeds");
        let groups = adapter.get_group_list().expect("groups available");
        let bytes = adapter
            .download_image(&QQImage {
                image_id: "image-1".to_owned(),
                source_key: "source-1".to_owned(),
            })
            .expect("image available");
        assert_eq!(groups[0].group_id, "group-1");
        assert!(adapter.subscribed_groups().contains("group-1"));
        assert_eq!(bytes, vec![1, 2, 3]);
    }

    #[test]
    fn message_pipeline_downloads_all_images_but_recalls_once() {
        let mut adapter = MockQQAdapter::default();
        adapter.add_image("source-a", vec![1]);
        adapter.add_image("source-b", vec![2]);
        adapter.connect().expect("mock connects");
        let images = [
            QQImage {
                image_id: "image-a".to_owned(),
                source_key: "source-a".to_owned(),
            },
            QQImage {
                image_id: "image-b".to_owned(),
                source_key: "source-b".to_owned(),
            },
        ];
        let mut engine = MessageModerationEngine::default();
        let outcome = process_message(
            &mut adapter,
            &mut engine,
            "message-pipeline-1",
            GroupMode::AutoRecall,
            &images,
            &frog_classifier,
            VisionThresholds::default(),
        );
        assert_eq!(outcome.classified_images, 2);
        assert!(outcome.failed_image_ids.is_empty());
        assert_eq!(outcome.event.action, ModerationAction::Recalled);
        assert_eq!(adapter.recalled_messages(), &["message-pipeline-1"]);
    }

    #[test]
    fn message_pipeline_observe_never_recalls() {
        let mut adapter = MockQQAdapter::default();
        adapter.add_image("source", vec![1]);
        adapter.connect().expect("mock connects");
        let image = [QQImage {
            image_id: "image".to_owned(),
            source_key: "source".to_owned(),
        }];
        let mut engine = MessageModerationEngine::default();
        let outcome = process_message(
            &mut adapter,
            &mut engine,
            "message-observe",
            GroupMode::Observe,
            &image,
            &frog_classifier,
            VisionThresholds::default(),
        );
        assert_eq!(outcome.event.action, ModerationAction::Observed);
        assert!(adapter.recalled_messages().is_empty());
    }

    #[test]
    fn message_pipeline_records_download_and_classifier_failures_and_fails_closed() {
        let mut adapter = MockQQAdapter::default();
        adapter.add_image("source-ok", vec![1]);
        adapter.connect().expect("mock connects");
        let images = [
            QQImage {
                image_id: "missing".to_owned(),
                source_key: "source-missing".to_owned(),
            },
            QQImage {
                image_id: "classifier-fails".to_owned(),
                source_key: "source-ok".to_owned(),
            },
        ];
        let mut engine = MessageModerationEngine::default();
        let outcome = process_message(
            &mut adapter,
            &mut engine,
            "message-failures",
            GroupMode::AutoRecall,
            &images,
            &|image: &QQImage, _: &[u8]| {
                if image.image_id == "classifier-fails" {
                    Err(QQError::Unsupported("bad image".to_owned()))
                } else {
                    Ok(frog_result(0.99))
                }
            },
            VisionThresholds::default(),
        );
        assert_eq!(
            outcome,
            MessagePipelineOutcome {
                event: super::ModerationEvent {
                    message_id: "message-failures".to_owned(),
                    decision: ModerationDecision::Pass,
                    action: ModerationAction::None,
                },
                classified_images: 0,
                failed_image_ids: vec!["missing".to_owned(), "classifier-fails".to_owned()],
            }
        );
        assert!(adapter.recalled_messages().is_empty());
    }
}
