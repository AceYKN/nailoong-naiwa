use serde::{Deserialize, Serialize};

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
