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

/// A file an install replaced, with what it held before (`None`: it did
/// not exist).
pub(crate) struct Rewritten {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

/// A finished install: what changed and every file it rewrote.
pub(crate) struct Install {
    pub(crate) change: ServiceChange,
    rewritten: Vec<Rewritten>,
}

impl Install {
    /// Puts the rewritten files back as they were, so the next install sees
    /// the change again and repeats the launchd action that went with it.
    pub(crate) fn undo(&self) -> Result<(), CommandError> {
        restore(&self.rewritten)
    }
}

pub(crate) fn restore(rewritten: &[Rewritten]) -> Result<(), CommandError> {
    for file in rewritten {
        match &file.previous {
            Some(bytes) => write_atomic(&file.path, bytes).map_err(io_error(&file.path))?,
            None => remove_file(&file.path)?,
        }
    }
    Ok(())
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
    let config_rewritten = write_if_changed(&config_path, config.as_bytes())?;
    // All or nothing: a failed plist step puts the config back, so the
    // next install sees the whole change again.
    let plist_rewritten = match write_if_changed(&plist_path, &plist) {
        Ok(rewritten) => rewritten,
        Err(err) => {
            restore(config_rewritten.as_slice())?;
            return Err(err);
        }
    };
    Ok(Install {
        change: ServiceChange {
            config: config_rewritten.is_some(),
            plist: plist_rewritten.is_some(),
        },
        rewritten: config_rewritten
            .into_iter()
            .chain(plist_rewritten)
            .collect(),
    })
}

/// Writes `bytes` unless the file already holds them; `None` when it did.
pub(crate) fn write_if_changed(
    path: &Path,
    bytes: &[u8],
) -> Result<Option<Rewritten>, CommandError> {
    let previous = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(path)(e)),
    };
    if previous.as_deref() == Some(bytes) {
        return Ok(None);
    }
    write_atomic(path, bytes).map_err(io_error(path))?;
    Ok(Some(Rewritten {
        path: path.to_path_buf(),
        previous,
    }))
}
