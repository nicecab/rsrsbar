//! Per-repository memory across polls: the previous health, the alerted
//! episode and the recovery point.

use serde::{Deserialize, Serialize};

use crate::health::{Color, Row};
use crate::status::StatusFile;

/// The `attempt_seq` after which failures count, tied to the counters it
/// belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPoint {
    pub counters_since: String,
    pub seq: u64,
}

/// What one notification episode is about: the row group and failing phase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeKey {
    pub group: Row,
    pub phase: Option<String>,
}

/// What rsrsbar remembers about a repository between polls.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMemory {
    pub last_row: Option<Row>,
    pub last_color: Option<Color>,
    pub label: Option<String>,
    pub phase: Option<String>,
    pub alerted: Option<EpisodeKey>,
    pub recovery: Option<RecoveryPoint>,
}

impl RepoMemory {
    /// The recovery point's `attempt_seq`, only while it belongs to the
    /// status file's current counters (a changed `counters_since` means
    /// git-rsrs restarted them, so the point no longer applies).
    pub fn recovery_seq(&self, status: &StatusFile) -> Option<u64> {
        self.recovery
            .as_ref()
            .filter(|point| point.counters_since == status.counters_since)
            .map(|point| point.seq)
    }

    /// Whether the previous poll was red by row 6, 7, 7a or 7b.
    pub fn was_failure_red(&self) -> bool {
        matches!(
            self.last_row,
            Some(Row::SyncFailed | Row::FailureRun | Row::Intermittent | Row::StaysRed)
        )
    }
}
