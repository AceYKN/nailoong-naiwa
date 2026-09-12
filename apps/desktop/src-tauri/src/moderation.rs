use serde::{Deserialize, Serialize};

pub const DEFAULT_CLASSIFY_THRESHOLD: f32 = 0.65;
pub const DEFAULT_RECALL_THRESHOLD: f32 = 0.98;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GroupMode {
    Off,
    Observe,
    AutoRecall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationDecision {
    Pass,
    ObserveRecall,
    Recall,
    Skip,
}

pub fn decide(mode: GroupMode, naiwa_frog_score: f32, recall_threshold: f32) -> ModerationDecision {
    if mode == GroupMode::Off {
        return ModerationDecision::Skip;
    }

    if naiwa_frog_score < recall_threshold {
        return ModerationDecision::Pass;
    }

    match mode {
        GroupMode::Observe => ModerationDecision::ObserveRecall,
        GroupMode::AutoRecall => ModerationDecision::Recall,
        GroupMode::Off => ModerationDecision::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::{decide, GroupMode, ModerationDecision};

    #[test]
    fn off_never_performs_work_even_for_high_score() {
        assert_eq!(
            decide(GroupMode::Off, 0.999, 0.98),
            ModerationDecision::Skip
        );
    }

    #[test]
    fn observe_records_a_recall_candidate_without_recalling() {
        assert_eq!(
            decide(GroupMode::Observe, 0.99, 0.98),
            ModerationDecision::ObserveRecall
        );
    }

    #[test]
    fn auto_recall_below_threshold_passes() {
        assert_eq!(
            decide(GroupMode::AutoRecall, 0.50, 0.98),
            ModerationDecision::Pass
        );
    }

    #[test]
    fn auto_recall_above_threshold_is_a_decision_not_an_adapter_call() {
        assert_eq!(
            decide(GroupMode::AutoRecall, 0.99, 0.98),
            ModerationDecision::Recall
        );
    }
}
