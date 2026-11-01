//! The CLI's commands as library functions over a [`Ctx`], so tests drive
//! them with a temp home and a [`crate::Recording`] launchd.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::atomic::write_atomic;
use crate::episode::StateError;
use crate::git::GitError;
use crate::gitrsrs::GitRsrsError;
use crate::launchd::{Launchd, LaunchdError};
use crate::paths::{Paths, PathsError};
use crate::registry::{RegistryError, RepoEntry};
use crate::service::{ServiceError, git_rsrs_config, launch_agent_plist};

mod add;
mod remove;

pub use add::{AddArgs, add};
pub use remove::remove;

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

pub(crate) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> CommandError + '_ {
    move |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Writes `entry`'s git-rsrs config and LaunchAgent plist (creating its
/// repository and log directories) and returns whether either file changed.
pub fn install_service(ctx: &Ctx, entry: &RepoEntry) -> Result<bool, CommandError> {
    let settings = entry.settings()?;
    let config = git_rsrs_config(entry, &settings, &ctx.paths, &ctx.rsrsbar_exe)?;
    let plist = launch_agent_plist(entry, &ctx.paths, &ctx.git_rsrs)?;
    let repo_dir = ctx.paths.repo_dir(&entry.id);
    let log = ctx.paths.stdout_log(&entry.id);
    let dirs = [Some(repo_dir.as_path()), log.parent()];
    for dir in dirs.into_iter().flatten() {
        fs::create_dir_all(dir).map_err(io_error(dir))?;
    }
    let config_changed =
        write_if_changed(&ctx.paths.git_rsrs_config(&entry.id), config.as_bytes())?;
    let plist_changed = write_if_changed(&ctx.paths.launch_agent(&entry.id), &plist)?;
    Ok(config_changed || plist_changed)
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, CommandError> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    write_atomic(path, bytes).map_err(io_error(path))?;
    Ok(true)
}

/// The service lock. Every command that changes a repository's service
/// files, launchd job or registry entry (`add`, `remove`, and later
/// `pause`, `resume`, `refresh`, `allow`) holds it from its first registry
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

/// Removes `path`; a file that is already gone is fine.
pub(crate) fn remove_file(path: &Path) -> Result<(), CommandError> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(path)(e)),
        _ => Ok(()),
    }
}
