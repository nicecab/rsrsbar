//! Writing a repository's generated service files, remembering what they
//! held before so a failed launchd action can be undone.

use std::fs;
use std::path::{Path, PathBuf};

use super::{CommandError, Ctx, io_error, remove_file};
use crate::atomic::write_atomic;
use crate::registry::RepoEntry;
use crate::service::{git_rsrs_config, launch_agent_plist};

/// Which generated files an install rewrote. launchd keeps the plist it
/// loaded at bootstrap (`kickstart -k` does not re-read it), while git-rsrs
/// reads its config when it starts; callers pick the launchd action from
/// this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceChange {
    pub config: bool,
    pub plist: bool,
}

impl ServiceChange {
    pub fn any(self) -> bool {
        self.config || self.plist
    }
}

/// A finished install: what changed and the previous contents of every
/// rewritten file (`None` = it did not exist).
pub(crate) struct Install {
    pub(crate) change: ServiceChange,
    previous: Vec<(PathBuf, Option<Vec<u8>>)>,
}

impl Install {
    /// Puts the rewritten files back as they were, so the next install sees
    /// the change again and repeats the launchd action that went with it.
    pub(crate) fn undo(&self) -> Result<(), CommandError> {
        for (path, bytes) in &self.previous {
            match bytes {
                Some(bytes) => write_atomic(path, bytes).map_err(io_error(path))?,
                None => remove_file(path)?,
            }
        }
        Ok(())
    }
}

/// Writes `entry`'s git-rsrs config and LaunchAgent plist (creating its
/// repository and log directories) and reports which file changed. All or
/// nothing: if the plist cannot be written, the config is restored before
/// the error is returned.
pub fn install_service(ctx: &Ctx, entry: &RepoEntry) -> Result<ServiceChange, CommandError> {
    Ok(install(ctx, entry)?.change)
}

pub(crate) fn install(ctx: &Ctx, entry: &RepoEntry) -> Result<Install, CommandError> {
    let settings = entry.settings()?;
    let config = git_rsrs_config(entry, &settings, &ctx.paths, &ctx.rsrsbar_exe)?;
    let plist = launch_agent_plist(entry, &ctx.paths, &ctx.git_rsrs)?;
    let repo_dir = ctx.paths.repo_dir(&entry.id);
    let log = ctx.paths.stdout_log(&entry.id);
    let dirs = [Some(repo_dir.as_path()), log.parent()];
    for dir in dirs.into_iter().flatten() {
        fs::create_dir_all(dir).map_err(io_error(dir))?;
    }
    let config_path = ctx.paths.git_rsrs_config(&entry.id);
    let plist_path = ctx.paths.launch_agent(&entry.id);
    let config_previous = write_if_changed(&config_path, config.as_bytes())?;
    let config_written = Install {
        change: ServiceChange {
            config: config_previous.is_some(),
            plist: false,
        },
        previous: config_previous
            .map(|bytes| vec![(config_path, bytes)])
            .unwrap_or_default(),
    };
    // All or nothing: a failed plist step puts the config back, so the
    // next install sees the whole change again.
    let plist_previous = match write_if_changed(&plist_path, &plist) {
        Ok(previous) => previous,
        Err(err) => {
            config_written.undo()?;
            return Err(err);
        }
    };
    Ok(Install {
        change: ServiceChange {
            plist: plist_previous.is_some(),
            ..config_written.change
        },
        previous: config_written
            .previous
            .into_iter()
            .chain(plist_previous.map(|bytes| (plist_path, bytes)))
            .collect(),
    })
}

/// Writes `bytes` unless the file already holds them. Returns `None` when
/// unchanged, else `Some(previous contents)` (`Some(None)`: no file before).
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<Option<Option<Vec<u8>>>, CommandError> {
    let current = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(path)(e)),
    };
    if current.as_deref() == Some(bytes) {
        return Ok(None);
    }
    write_atomic(path, bytes).map_err(io_error(path))?;
    Ok(Some(current))
}
