//! The CLI's commands as library functions over a [`Ctx`], so tests drive
//! them with a temp home and a [`crate::Recording`] launchd.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::episode::StateError;
use crate::git::GitError;
use crate::gitrsrs::GitRsrsError;
use crate::launchd::{Launchd, LaunchdError};
use crate::paths::{Paths, PathsError, RepoId};
use crate::registry::{Registry, RegistryError, RepoEntry};
use crate::service::ServiceError;
use crate::trust::present_hooks;

mod add;
mod allow;
mod app_service;
mod heartbeat;
mod install;
mod lifecycle;
mod preview;
mod remove;
mod run_hook;
mod status;
mod sync;

pub use add::{AddArgs, add};
pub use allow::allow;
pub use app_service::{start, stop};
pub use heartbeat::check_heartbeat;
pub use install::{ServiceChange, install_service};
pub use lifecycle::{pause, refresh, restart, resume};
pub use preview::{Preview, allow_preview};
pub use remove::remove;
pub use run_hook::{RunHookError, run_hook};
pub use status::status;
pub use sync::sync;

/// Everything a command may touch, passed in explicitly.
pub struct Ctx<'a> {
    pub paths: Paths,
    pub launchd: &'a dyn Launchd,
    /// The git-rsrs the LaunchAgents run and `sync` calls.
    pub git_rsrs: PathBuf,
    /// The rsrsbar binary the generated hooks call back into.
    pub rsrsbar_exe: PathBuf,
    pub now: SystemTime,
}

/// What a command prints to stdout and the process exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    pub exit_code: i32,
}

impl Report {
    /// A successful report.
    pub fn ok(text: String) -> Report {
        Report { text, exit_code: 0 }
    }
}

/// Why a command failed; the message is what the user sees.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    /// A refusal that names its fix.
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GitRsrs(#[from] GitRsrsError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Launchd(#[from] LaunchdError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Paths(#[from] PathsError),
    #[error("{}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// Exit code for a command that ran past its deadline (as `timeout(1)`).
const DEADLINE_EXIT: u8 = 124;
const FAILURE_EXIT: u8 = 1;

impl CommandError {
    /// The process exit code for this error: 124 when `sync` ran past its
    /// deadline, 1 otherwise (usage errors exit 2 from the argument parser).
    pub fn exit_code(&self) -> u8 {
        match self {
            CommandError::GitRsrs(GitRsrsError::Deadline(_)) => DEADLINE_EXIT,
            _ => FAILURE_EXIT,
        }
    }
}

pub(crate) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> CommandError + '_ {
    move |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// The service lock. Every command that changes a repository's service
/// files, launchd job or registry entry (`add`, `remove`, `pause`,
/// `resume`, `refresh`, `restart`, `allow`) holds it from its first registry
/// read to its last write, so it never acts on a registry view that another
/// command has changed in between. Released when the returned handle drops.
///
/// It is an exclusive `flock` on the support directory itself (created if
/// needed): unlike a sibling lock file it leaves no file behind, and it
/// never contends with the `registry.toml.lock` / `state.json.lock` that
/// `Registry::update` and `State::update` take inside it.
pub(crate) fn lock_services(ctx: &Ctx) -> Result<File, CommandError> {
    let dir = ctx.paths.support_dir();
    fs::create_dir_all(&dir).map_err(io_error(&dir))?;
    let handle = File::open(&dir).map_err(io_error(&dir))?;
    handle.lock().map_err(io_error(&dir))?;
    Ok(handle)
}

/// The registered entry for `id`.
pub(crate) fn registered(ctx: &Ctx, id: &str) -> Result<RepoEntry, CommandError> {
    lookup(ctx, &RepoId::new(id)?)
}

fn lookup(ctx: &Ctx, id: &RepoId) -> Result<RepoEntry, CommandError> {
    match Registry::load(&ctx.paths.registry())?.get(id) {
        Some(entry) => Ok(entry.clone()),
        None => Err(RegistryError::UnknownId(id.clone()).into()),
    }
}

/// Takes the service lock and only then looks `id` up, so a command that
/// changes a service never acts on an entry read before it held the lock.
/// The lock is held until the returned file drops.
pub(crate) fn locked_entry(ctx: &Ctx, id: &str) -> Result<(File, RepoEntry), CommandError> {
    let id = RepoId::new(id)?;
    let lock = lock_services(ctx)?;
    Ok((lock, lookup(ctx, &id)?))
}

/// "pre-commit, post-integrate" for the executable hooks in `repo`, or
/// "none".
pub(crate) fn hooks_text(repo: &Path) -> String {
    let hooks: Vec<_> = present_hooks(repo)
        .iter()
        .map(|phase| phase.file_name())
        .collect();
    if hooks.is_empty() {
        "none".to_owned()
    } else {
        hooks.join(", ")
    }
}

/// Removes `path`; a file that is already gone is fine.
pub(crate) fn remove_file(path: &Path) -> Result<(), CommandError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(path)(e)),
        _ => Ok(()),
    }
}
