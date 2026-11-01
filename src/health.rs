//! The watchdog's health table: one repository observation in, one row out.

use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::episode::RepoMemory;
use crate::registry::Settings;
use crate::status::{AttemptHealth, Outcome, RecentAttempt, StatusFile, StatusRead};

const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 3600;
const FAILURE_RUN_MIN: u32 = 2;
const INTERMITTENT_MIN: u32 = 3;
/// Consecutive successes that end a red episode: `classify` cuts the failure
/// window and `advance` records the recovery point on the same poll.
pub(crate) const RELEASE_SUCCESSES: u32 = 2;

/// The launchd job state of a repository's watcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    NotRunning,
    Running { up_for: Duration },
}

/// Everything one classification reads.
#[derive(Clone, Copy, Debug)]
pub struct Observation<'a> {
    pub now: SystemTime,
    pub awake_since: SystemTime,
    pub settings: &'a Settings,
    pub paused_at: Option<SystemTime>,
    pub hooks_trusted: bool,
    pub job: JobState,
    pub status: &'a StatusRead,
    pub current_branch: Option<&'a str>,
    pub memory: &'a RepoMemory,
}

/// The spec's table rows in evaluation order (1, 2, 3, 4, 5, starting, 6,
/// 7, 7a, 7b, 8, 9, 10, 11, 12, 13, 14, 15), plus `Unobservable`, which
/// `classify` never returns: it stands for a repository that could not be
/// observed at all (see [`Health::unobservable`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Row {
    PausedLong,
    Paused,
    HooksChanged,
    ManualIntervention,
    WatcherNotRunning,
    Starting,
    SyncFailed,
    FailureRun,
    Intermittent,
    StaysRed,
    SingleFailure,
    OfflineLong,
    Offline,
    OtherBranchLong,
    OtherBranch,
    Stalled,
    Transient,
    Healthy,
    Unobservable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Green,
    Grey,
    Yellow,
    Red,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Notify {
    Never,
    Yes,
    Once,
    Immediately,
}

/// A classified repository: its row, the row's color and notification
/// policy, and the label / failing phase shown to Tom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health {
    pub row: Row,
    pub color: Color,
    pub notify: Notify,
    pub label: Option<String>,
    pub phase: Option<String>,
}

impl Health {
    /// A repository that could not be observed (checkout gone, settings or
    /// launchd unreadable): red, notified at once, "cannot observe
    /// repository".
    pub fn unobservable() -> Health {
        Health::new(Row::Unobservable, Some("cannot observe repository"))
    }

    fn new(row: Row, label: Option<&str>) -> Health {
        Health::with_phase(row, label.map(str::to_owned), None)
    }

    fn with_phase(row: Row, label: Option<String>, phase: Option<String>) -> Health {
        let (color, notify) = policy(row);
        Health {
            row,
            color,
            notify,
            label,
            phase,
        }
    }
}

impl Row {
    /// Rows 6, 7, 7a and 7b: the red that row 7b holds on to.
    pub(crate) fn is_failure_red(self) -> bool {
        matches!(
            self,
            Row::SyncFailed | Row::FailureRun | Row::Intermittent | Row::StaysRed
        )
    }

    /// Rows 6, 7, 7a and 8, whose label and phase describe a fresh failure
    /// (7b only repeats the remembered ones).
    pub(crate) fn has_failure_label(self) -> bool {
        matches!(
            self,
            Row::SyncFailed | Row::FailureRun | Row::Intermittent | Row::SingleFailure
        )
    }

    /// The row an episode is keyed by: rows 7, 7a and 7b count as one row.
    pub(crate) fn episode_group(self) -> Row {
        match self {
            Row::FailureRun | Row::Intermittent | Row::StaysRed => Row::FailureRun,
            other => other,
        }
    }
}

/// The color and notification policy of each row, per the spec's table.
fn policy(row: Row) -> (Color, Notify) {
    match row {
        Row::PausedLong | Row::OtherBranchLong => (Color::Yellow, Notify::Once),
        Row::Paused | Row::Starting | Row::Offline | Row::OtherBranch => {
            (Color::Grey, Notify::Never)
        }
        Row::HooksChanged
        | Row::ManualIntervention
        | Row::WatcherNotRunning
        | Row::SyncFailed
        | Row::Unobservable => (Color::Red, Notify::Immediately),
        Row::FailureRun | Row::Intermittent | Row::OfflineLong | Row::Stalled => {
            (Color::Red, Notify::Yes)
        }
        Row::StaysRed => (Color::Red, Notify::Never),
        Row::SingleFailure | Row::Transient => (Color::Yellow, Notify::Never),
        Row::Healthy => (Color::Green, Notify::Never),
    }
}

/// Classifies one repository: the first matching row in the spec's order
/// wins. Every "more than X" comparison is strict.
pub fn classify(obs: &Observation) -> Health {
    if let Some(paused_at) = obs.paused_at {
        return paused(elapsed(paused_at, obs.now), obs.settings.paused_limit);
    }
    if !obs.hooks_trusted {
        return Health::new(Row::HooksChanged, Some("hook changed — needs allow"));
    }
    if matches!(obs.status, StatusRead::Present(s) if s.last_outcome == Outcome::ManualIntervention)
    {
        return Health::new(Row::ManualIntervention, Some("manual intervention needed"));
    }
    if watcher_not_running(obs) {
        return Health::new(Row::WatcherNotRunning, Some("watcher not running"));
    }
    let StatusRead::Present(status) = obs.status else {
        return Health::new(Row::Starting, Some("waiting for first sync"));
    };
    if matches!(
        status.last_outcome,
        Outcome::AuthenticationFailed | Outcome::Failed
    ) {
        return Health::new(Row::SyncFailed, Some("sync failed"));
    }
    failure_rows(obs, status).unwrap_or_else(|| time_rows(obs, status))
}

/// Consecutive unhealthy `recent` entries, counted from the newest, among
/// entries after the recovery point (`seq > recovery`).
pub fn failure_run(status: &StatusFile, recovery: Option<u64>) -> u32 {
    let run = window(status, recovery).take_while(|a| !a.healthy).count();
    u32::try_from(run).unwrap_or(u32::MAX)
}

/// Unhealthy `recent` entries after the recovery point (`seq > recovery`).
pub fn unhealthy_in_window(status: &StatusFile, recovery: Option<u64>) -> u32 {
    let count = window(status, recovery).filter(|a| !a.healthy).count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// `recent` (newest first) cut at the recovery point.
fn window(status: &StatusFile, recovery: Option<u64>) -> impl Iterator<Item = &RecentAttempt> {
    status
        .recent
        .iter()
        .take_while(move |a| recovery.is_none_or(|point| a.seq > point))
}

/// Rows 7, 7a, 7b and 8, which share one window of `recent`.
fn failure_rows(obs: &Observation, status: &StatusFile) -> Option<Health> {
    let recovery = effective_recovery(status, obs.memory);
    let run = failure_run(status, recovery);
    if run >= FAILURE_RUN_MIN {
        let (label, phase) = failure_label(status, obs.memory);
        return Some(Health::with_phase(Row::FailureRun, Some(label), phase));
    }
    let unhealthy = unhealthy_in_window(status, recovery);
    if unhealthy >= INTERMITTENT_MIN {
        let label = format!("failing intermittently: {unhealthy} of last 10");
        let (_, phase) = failure_label(status, obs.memory);
        return Some(Health::with_phase(Row::Intermittent, Some(label), phase));
    }
    if obs.memory.was_failure_red() && status.consecutive_successes < RELEASE_SUCCESSES {
        return Some(Health::with_phase(
            Row::StaysRed,
            obs.memory.label.clone(),
            obs.memory.phase.clone(),
        ));
    }
    if run == 1 {
        let (label, phase) = failure_label(status, obs.memory);
        return Some(Health::with_phase(Row::SingleFailure, Some(label), phase));
    }
    None
}

/// The recovery point in force for this poll: leaving failure red with
/// enough successes moves it to the current attempt at once, so old failures
/// stop counting this very poll; otherwise the remembered point.
fn effective_recovery(status: &StatusFile, memory: &RepoMemory) -> Option<u64> {
    if memory.was_failure_red() && status.consecutive_successes >= RELEASE_SUCCESSES {
        Some(status.attempt_seq)
    } else {
        memory.recovery_seq(status)
    }
}

/// Label and phase for rows 7, 7a and 8: from the latest attempt when it was
/// unhealthy, otherwise remembered from the last unhealthy observation.
fn failure_label(status: &StatusFile, memory: &RepoMemory) -> (String, Option<String>) {
    if status.latest_health() != AttemptHealth::Unhealthy {
        let label = memory
            .label
            .clone()
            .unwrap_or_else(|| "sync failing".to_owned());
        return (label, memory.phase.clone());
    }
    let phase = status.hook_failures.first().map(|f| f.phase.clone());
    let label = match (status.last_outcome, &phase) {
        (Outcome::TimedOut, _) => "sync timed out".to_owned(),
        (_, Some(phase)) => format!("check failed: {phase}"),
        (_, None) => "check failed".to_owned(),
    };
    (label, phase)
}

/// Rows 9 to 15. The time-based rows (9, 11, 13) are skipped in wake grace.
fn time_rows(obs: &Observation, status: &StatusFile) -> Health {
    let late = |limit: Duration| !in_grace(obs) && since_success(status, obs.now) > limit;
    let (offline_limit, branch_limit) =
        (obs.settings.offline_limit, obs.settings.other_branch_limit);
    match status.last_outcome {
        Outcome::Offline if late(offline_limit) => {
            let label = format!("offline for over {}", limit_text(offline_limit));
            return Health::with_phase(Row::OfflineLong, Some(label), None);
        }
        Outcome::Offline => return Health::new(Row::Offline, None),
        Outcome::BranchSkipped if late(branch_limit) => {
            let branch = obs.current_branch.unwrap_or("another branch");
            let label = format!("on {branch}, no sync for over {}", limit_text(branch_limit));
            return Health::with_phase(Row::OtherBranchLong, Some(label), None);
        }
        Outcome::BranchSkipped => return Health::new(Row::OtherBranch, None),
        _ => {}
    }
    if late(obs.settings.stall_threshold) {
        let label = match status.last_outcome {
            Outcome::LockTimeout => "sync stalled (lock held)",
            _ => "sync stalled",
        };
        return Health::new(Row::Stalled, Some(label));
    }
    match status.last_outcome {
        Outcome::LockTimeout | Outcome::Deferred | Outcome::ValidationRejected => {
            Health::new(Row::Transient, Some(status.last_outcome.as_str()))
        }
        _ => Health::new(Row::Healthy, None),
    }
}

/// `now − last_success_at`, measured from `counters_since` when git-rsrs has
/// never succeeded. An unparsable `counters_since` means no success is known
/// at all, so the time is unbounded (the time rows fire rather than hide a
/// stuck repository).
fn since_success(status: &StatusFile, now: SystemTime) -> Duration {
    let since = status
        .last_success_at
        .or_else(|| humantime::parse_rfc3339(&status.counters_since).ok());
    since.map_or(Duration::MAX, |since| elapsed(since, now))
}

/// Rows 1 and 2.
fn paused(paused_for: Duration, limit: Duration) -> Health {
    if paused_for > limit {
        let label = format!("paused for over {}", limit_text(limit));
        Health::with_phase(Row::PausedLong, Some(label), None)
    } else {
        Health::new(Row::Paused, None)
    }
}

/// A limit for a label in the largest unit that divides it exactly (h, min,
/// else s): the defaults read "24 h" and "2 h", an override "10 min".
fn limit_text(limit: Duration) -> String {
    match limit.as_secs() {
        s if s > 0 && s % SECS_PER_HOUR == 0 => format!("{} h", s / SECS_PER_HOUR),
        s if s > 0 && s % SECS_PER_MINUTE == 0 => format!("{} min", s / SECS_PER_MINUTE),
        s => format!("{s} s"),
    }
}

/// Row 5: the job is not running, or the status file is missing / unreadable
/// after the job has been up for the threshold (skipped in wake grace).
fn watcher_not_running(obs: &Observation) -> bool {
    match (obs.job, obs.status) {
        (JobState::NotRunning, _) => true,
        (JobState::Running { .. }, StatusRead::Present(_)) => false,
        (JobState::Running { up_for }, _) => {
            up_for > obs.settings.stall_threshold && !in_grace(obs)
        }
    }
}

/// The Mac has been awake for less than one interval.
fn in_grace(obs: &Observation) -> bool {
    elapsed(obs.awake_since, obs.now) < obs.settings.interval
}

/// `now − since`, zero when `since` is in the future (clock skew).
fn elapsed(since: SystemTime, now: SystemTime) -> Duration {
    now.duration_since(since).unwrap_or(Duration::ZERO)
}
