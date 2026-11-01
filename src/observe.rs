//! One registry entry turned into the inputs of the health table, so the
//! CLI's `status` and the app classify identically.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use crate::episode::RepoMemory;
use crate::git::current_branch;
use crate::health::{JobState, Observation};
use crate::launchd::{Launchd, LaunchdError};
use crate::paths::{Paths, watch_label};
use crate::registry::{RegistryError, RepoEntry, Settings};
use crate::status::{Outcome, StatusRead, read_status};
use crate::system::process_age;
use crate::trust::hooks_digest;

/// Errors from observing a repository.
#[derive(Debug, thiserror::Error)]
pub enum ObserveError {
    #[error(transparent)]
    Settings(#[from] RegistryError),
    #[error("{id}: paused_at {value:?} is not an RFC 3339 time: {source}")]
    PausedAt {
        id: String,
        value: String,
        source: humantime::TimestampError,
    },
    #[error(transparent)]
    Launchd(#[from] LaunchdError),
    #[error("cannot read the checkout {}: {source}", path.display())]
    Checkout { path: PathBuf, source: io::Error },
}

/// What one look at a repository found; `observation` lends it to
/// [`crate::classify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSnapshot {
    pub settings: Settings,
    pub paused_at: Option<SystemTime>,
    pub hooks_trusted: bool,
    pub job: JobState,
    pub status: StatusRead,
    pub current_branch: Option<String>,
}

impl RepoSnapshot {
    /// The classification input for this snapshot at `now`.
    pub fn observation<'a>(
        &'a self,
        now: SystemTime,
        awake_since: SystemTime,
        memory: &'a RepoMemory,
    ) -> Observation<'a> {
        Observation {
            now,
            awake_since,
            settings: &self.settings,
            paused_at: self.paused_at,
            hooks_trusted: self.hooks_trusted,
            job: self.job,
            status: &self.status,
            current_branch: self.current_branch.as_deref(),
            memory,
        }
    }
}

/// Looks at one registered repository: that its checkout is still a
/// readable directory (a deleted checkout would otherwise look trusted and
/// merely "not syncing"), its settings and pause time, whether
/// `.autosync/` still matches the allowed digest (unreadable → untrusted),
/// its launchd job (`Running` whenever a PID exists, uptime `ZERO` when the
/// age cannot be read), its status file, and — only when the latest outcome
/// is `branch_skipped` — the branch checked out.
pub fn observe(
    entry: &RepoEntry,
    paths: &Paths,
    launchd: &dyn Launchd,
) -> Result<RepoSnapshot, ObserveError> {
    fs::read_dir(&entry.path).map_err(|source| ObserveError::Checkout {
        path: entry.path.clone(),
        source,
    })?;
    let settings = entry.settings()?;
    let paused_at = paused_at(entry)?;
    let hooks_trusted = hooks_digest(&entry.path)
        .is_ok_and(|digest| entry.allowed_hooks.as_deref() == Some(digest.as_str()));
    let job = match launchd
        .job(&watch_label(&entry.id))?
        .and_then(|job| job.pid)
    {
        Some(pid) => JobState::Running {
            up_for: process_age(pid).ok().flatten().unwrap_or(Duration::ZERO),
        },
        None => JobState::NotRunning,
    };
    let status = read_status(&paths.status_file(&entry.id));
    let skipped =
        matches!(&status, StatusRead::Present(s) if s.last_outcome == Outcome::BranchSkipped);
    Ok(RepoSnapshot {
        settings,
        paused_at,
        hooks_trusted,
        job,
        status,
        current_branch: skipped.then(|| current_branch(&entry.path)).flatten(),
    })
}

fn paused_at(entry: &RepoEntry) -> Result<Option<SystemTime>, ObserveError> {
    let Some(value) = entry.paused_at.as_deref() else {
        return Ok(None);
    };
    humantime::parse_rfc3339(value)
        .map(Some)
        .map_err(|source| ObserveError::PausedAt {
            id: entry.id.to_string(),
            value: value.to_owned(),
            source,
        })
}
