//! Per-repository memory across polls: the previous health, the alerted
//! episode and the recovery point, persisted in `state.json`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::{read_optional, write_atomic};
use crate::health::{Color, Health, Notify, RELEASE_SUCCESSES, Row};
use crate::paths::RepoId;
use crate::status::StatusFile;

const STATE_VERSION: u32 = 1;

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

    /// A copy with the recovery point at the current attempt: allowing hooks
    /// resets the failure window at once. Without a status file there is no
    /// window to reset, so the memory is returned unchanged.
    pub fn after_allow(&self, status: Option<&StatusFile>) -> RepoMemory {
        match status {
            Some(status) => RepoMemory {
                recovery: Some(recovery_point(status)),
                ..self.clone()
            },
            None => self.clone(),
        }
    }

    /// Whether the previous poll was red by row 6, 7, 7a or 7b.
    pub fn was_failure_red(&self) -> bool {
        self.last_row.is_some_and(Row::is_failure_red)
    }
}

impl EpisodeKey {
    /// The episode a health belongs to; rows 7, 7a and 7b count as one row.
    pub fn of(health: &Health) -> EpisodeKey {
        EpisodeKey {
            group: health.row.episode_group(),
            phase: health.phase.clone(),
        }
    }
}

/// A notification to deliver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Alert { label: String },
    Recovered,
}

/// Folds one poll's health into the memory and decides what to notify.
///
/// - Leaving red with `consecutive_successes` ≥ 2 is a recovery: a new
///   recovery point at the current attempt and the alerted episode cleared.
///   Leaving red otherwise is silent.
/// - Green clears the alerted episode ("episodes end on green").
/// - The remembered failure label and phase belong to the episode too: they
///   are cleared on green or recovery, exactly like the alerted episode.
/// - A notifying row whose episode differs from the alerted one alerts and
///   becomes the alerted episode. When a recovery and an alert coincide
///   (red → a notifying yellow row), the alert is returned: it names what
///   needs attention now, and the recovery is still recorded.
/// - The row and color are always stored; label and phase are replaced only
///   on failure rows (6, 7, 7a, 8), so 7b keeps the remembered ones.
pub fn advance(
    memory: &RepoMemory,
    health: &Health,
    status: Option<&StatusFile>,
) -> (RepoMemory, Option<Notice>) {
    let recovered = recovers(memory, health, status);
    let is_recovery = recovered.is_some();
    let episode_over = is_recovery || health.color == Color::Green;
    let alerted = memory.alerted.clone().filter(|_| !episode_over);
    let key = EpisodeKey::of(health);
    let alert = health.notify != Notify::Never && alerted.as_ref() != Some(&key);
    let (label, phase) = if health.row.has_failure_label() {
        (health.label.clone(), health.phase.clone())
    } else if episode_over {
        (None, None)
    } else {
        (memory.label.clone(), memory.phase.clone())
    };
    let next = RepoMemory {
        last_row: Some(health.row),
        last_color: Some(health.color),
        label,
        phase,
        alerted: if alert { Some(key) } else { alerted },
        recovery: recovered.or_else(|| memory.recovery.clone()),
    };
    let notice = if alert {
        Some(Notice::Alert {
            label: health
                .label
                .clone()
                .unwrap_or_else(|| "needs attention".to_owned()),
        })
    } else {
        is_recovery.then_some(Notice::Recovered)
    };
    (next, notice)
}

/// The new recovery point when this poll leaves red with enough successes.
fn recovers(
    memory: &RepoMemory,
    health: &Health,
    status: Option<&StatusFile>,
) -> Option<RecoveryPoint> {
    let left_red = memory.last_color == Some(Color::Red) && health.color != Color::Red;
    status
        .filter(|s| left_red && s.consecutive_successes >= RELEASE_SUCCESSES)
        .map(recovery_point)
}

fn recovery_point(status: &StatusFile) -> RecoveryPoint {
    RecoveryPoint {
        counters_since: status.counters_since.clone(),
        seq: status.attempt_seq,
    }
}

/// Errors from reading and writing `state.json`.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("cannot serialize the state: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("unsupported state version {0} (expected {STATE_VERSION})")]
    UnsupportedVersion(u32),
}

/// Every repository's memory, persisted so a restart neither re-sends
/// ongoing alerts nor re-fires "once" notifications.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub repos: BTreeMap<RepoId, RepoMemory>,
}

impl Default for State {
    fn default() -> State {
        State {
            version: STATE_VERSION,
            repos: BTreeMap::new(),
        }
    }
}

impl State {
    /// Reads the state; a missing file is an empty state. A corrupt file or
    /// an unknown version is an error (the caller decides how to recover).
    pub fn load(path: &Path) -> Result<State, StateError> {
        let Some(text) = read_optional(path).map_err(|source| io_error(path, source))? else {
            return Ok(State::default());
        };
        let state: State = serde_json::from_str(&text).map_err(|source| StateError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        if state.version != STATE_VERSION {
            return Err(StateError::UnsupportedVersion(state.version));
        }
        Ok(state)
    }

    /// Writes the state atomically.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let bytes = serde_json::to_vec_pretty(self)?;
        write_atomic(path, &bytes).map_err(|source| io_error(path, source))
    }

    /// Locked read-modify-write, shared by the CLI and the app: holds an
    /// exclusive lock on the sibling `<path>.lock` (never replaced, unlike
    /// the state itself) across load, `f` and save, like `Registry::update`.
    /// A missing state starts empty; a corrupt one is an error, never
    /// overwritten. The lock is released when the file is dropped.
    pub fn update(path: &Path, f: impl FnOnce(&State) -> State) -> Result<State, StateError> {
        let lock_path = lock_path(path);
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        let lock = File::create(&lock_path).map_err(|source| io_error(&lock_path, source))?;
        lock.lock().map_err(|source| io_error(&lock_path, source))?;
        let updated = f(&State::load(path)?);
        updated.save(path)?;
        drop(lock);
        Ok(updated)
    }

    /// A copy with `id`'s memory replaced.
    pub fn with_repo(&self, id: &RepoId, memory: RepoMemory) -> State {
        let repos = self
            .repos
            .iter()
            .filter(|(key, _)| *key != id)
            .map(|(key, value)| (key.clone(), value.clone()))
            .chain([(id.clone(), memory)])
            .collect();
        State {
            version: self.version,
            repos,
        }
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".lock");
    PathBuf::from(name)
}

fn io_error(path: &Path, source: io::Error) -> StateError {
    StateError::Io {
        path: path.to_path_buf(),
        source,
    }
}
