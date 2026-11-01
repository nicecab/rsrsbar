//! One poll of every registered repository: observe outside any lock,
//! then classify and advance every episode inside one `State::update`, and
//! finally write the heartbeat. Also the single-instance lock.

use std::fs::{self, File, TryLockError};
use std::io;
use std::time::SystemTime;

use super::Authorization;
use crate::commands::Ctx;
use crate::episode::{Notice, RepoMemory, State, advance};
use crate::health::{Health, classify};
use crate::heartbeat::Heartbeat;
use crate::observe::{RepoSnapshot, observe};
use crate::paths::{Paths, RepoId};
use crate::registry::{Registry, RepoEntry};
use crate::status::StatusRead;
use crate::trust::{Phase, present_hooks};

/// One repository as the menu shows it. `snapshot` is `None` and `error`
/// says why when it could not be observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoView {
    pub entry: RepoEntry,
    pub health: Health,
    pub snapshot: Option<RepoSnapshot>,
    pub error: Option<String>,
    pub hooks: Vec<Phase>,
}

/// Everything one poll found: the repositories in registry order, the
/// notifications to post and the problems the poll itself ran into.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollOutcome {
    pub repos: Vec<RepoView>,
    pub notices: Vec<(RepoId, Notice)>,
    pub warnings: Vec<String>,
}

/// A repository observed outside the state lock (the filesystem and
/// launchd are read here, never while the state lock is held).
struct Observed {
    entry: RepoEntry,
    snapshot: Result<RepoSnapshot, String>,
    hooks: Vec<Phase>,
}

/// Polls every registered repository at `ctx.now` and writes the
/// heartbeat. An unreadable registry means "unknown which repositories
/// exist", not "none": one warning, no repositories, and the state is left
/// untouched (no pruning, no notices).
pub fn poll(ctx: &Ctx, awake_since: SystemTime, notifications: Authorization) -> PollOutcome {
    let outcome = match Registry::load(&ctx.paths.registry()) {
        Ok(registry) => remember(ctx, awake_since, &observe_all(ctx, &registry)),
        Err(err) => PollOutcome {
            warnings: vec![format!("cannot read the registry: {err}")],
            ..PollOutcome::default()
        },
    };
    let heartbeat = Heartbeat {
        at: ctx.now,
        pid: std::process::id(),
        notifications,
    };
    let path = ctx.paths.heartbeat();
    let heartbeat_warning = heartbeat
        .save(&path)
        .err()
        .map(|err| format!("cannot write {}: {err}", path.display()));
    PollOutcome {
        warnings: outcome
            .warnings
            .into_iter()
            .chain(heartbeat_warning)
            .collect(),
        ..outcome
    }
}

fn observe_all(ctx: &Ctx, registry: &Registry) -> Vec<Observed> {
    registry
        .repos
        .iter()
        .map(|entry| Observed {
            entry: entry.clone(),
            snapshot: observe(entry, &ctx.paths, ctx.launchd).map_err(|err| err.to_string()),
            hooks: present_hooks(&entry.path),
        })
        .collect()
}

/// Classifies and advances inside one locked state update, so a concurrent
/// writer (`rsrsbar allow`) is never overwritten with memory read before
/// it; a corrupt state is set aside under that same lock
/// ([`State::update_or_reset`]). If the state cannot be saved at all, the
/// views are still returned but notices are held back (they could not be
/// deduplicated and would repeat every poll).
fn remember(ctx: &Ctx, awake_since: SystemTime, observed: &[Observed]) -> PollOutcome {
    let mut result = None;
    let saved = State::update_or_reset(&ctx.paths.state(), |state| {
        let (next, views, notices) = evaluate(ctx, awake_since, observed, state);
        result = Some((views, notices));
        next
    });
    match saved {
        Ok((_, warning)) => {
            // `update_or_reset` ran the closure before saving.
            let (repos, notices) = result.unwrap_or_default();
            PollOutcome {
                repos,
                notices,
                warnings: warning.into_iter().collect(),
            }
        }
        Err(err) => {
            let (_, repos, _) = evaluate(ctx, awake_since, observed, &State::default());
            PollOutcome {
                repos,
                notices: Vec::new(),
                warnings: vec![format!(
                    "cannot save the state, notifications held back: {err}"
                )],
            }
        }
    }
}

/// The new state (registered repositories only) with every view and notice.
fn evaluate(
    ctx: &Ctx,
    awake_since: SystemTime,
    observed: &[Observed],
    state: &State,
) -> (State, Vec<RepoView>, Vec<(RepoId, Notice)>) {
    let results: Vec<_> = observed
        .iter()
        .map(|observed| {
            let memory = state
                .repos
                .get(&observed.entry.id)
                .cloned()
                .unwrap_or_default();
            view(ctx, awake_since, observed, &memory)
        })
        .collect();
    let repos = results
        .iter()
        .map(|(view, memory, _)| (view.entry.id.clone(), memory.clone()))
        .collect();
    let next = State {
        version: state.version,
        repos,
    };
    let notices = results
        .iter()
        .filter_map(|(view, _, notice)| Some((view.entry.id.clone(), notice.clone()?)))
        .collect();
    let views = results.into_iter().map(|(view, _, _)| view).collect();
    (next, views, notices)
}

/// One repository's view, advanced memory and notice.
fn view(
    ctx: &Ctx,
    awake_since: SystemTime,
    observed: &Observed,
    memory: &RepoMemory,
) -> (RepoView, RepoMemory, Option<Notice>) {
    let snapshot = &observed.snapshot;
    let (health, status) = match snapshot {
        Ok(snapshot) => {
            let health = classify(&snapshot.observation(ctx.now, awake_since, memory));
            let status = match &snapshot.status {
                StatusRead::Present(status) => Some(status),
                _ => None,
            };
            (health, status)
        }
        Err(_) => (Health::unobservable(), None),
    };
    let (next, notice) = advance(memory, &health, status);
    let view = RepoView {
        entry: observed.entry.clone(),
        health,
        snapshot: snapshot.as_ref().ok().cloned(),
        error: snapshot.as_ref().err().cloned(),
        hooks: observed.hooks.clone(),
    };
    (view, next, notice)
}

/// Held by the one running app; released when dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

/// Takes the non-blocking exclusive `flock` on `app.lock`; `Ok(None)` when
/// another instance holds it.
pub fn acquire_instance(paths: &Paths) -> io::Result<Option<InstanceLock>> {
    let path = paths.app_lock();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = File::create(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(err)) => Err(err),
    }
}
