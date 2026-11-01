//! `rsrsbar run-hook`: the trust gate every generated git-rsrs hook goes
//! through. It fails closed — any doubt exits 1 and runs nothing (never 3,
//! which git-rsrs would read as an intentional pre-commit rejection) — and
//! it executes the allowed snapshot, never the checkout's file.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use crate::paths::{Paths, PathsError, RepoId};
use crate::registry::{Registry, RegistryError};
use crate::trust::{Phase, hooks_digest, is_executable_file, snapshot_digest};

/// Why `run-hook` refused; every variant exits 1.
#[derive(Debug, thiserror::Error)]
pub enum RunHookError {
    #[error("hook changed — needs allow")]
    HookChanged,
    #[error("unknown hook phase {0:?}")]
    UnknownPhase(String),
    #[error(transparent)]
    Id(#[from] PathsError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("cannot read {}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("cannot run {}: {source}", .path.display())]
    Exec { path: PathBuf, source: io::Error },
}

/// Checks that the checkout's `.autosync/` and the allowed snapshot both
/// match the digest recorded by the last allow, then replaces this process
/// with `<allowed-hooks>/<phase>` (inheriting git-rsrs's cwd and
/// environment, so its process-group timeout still applies). Returns
/// `Ok(())` — exit 0 — only when the phase has no executable hook in the
/// allowed set: an absent file, or one without an executable bit, which by
/// git's convention is not a hook (its mode is covered by the digest, so
/// making it executable later needs a new allow). An empty allowed set with
/// an empty checkout and no snapshot passes the same way: there is nothing
/// to run. When a hook runs, this function does not return.
pub fn run_hook(paths: &Paths, id: &str, phase: &str) -> Result<(), RunHookError> {
    let id = RepoId::new(id)?;
    let phase = Phase::ALL
        .into_iter()
        .find(|p| p.file_name() == phase)
        .ok_or_else(|| RunHookError::UnknownPhase(phase.to_owned()))?;
    let registry = Registry::load(&paths.registry())?;
    let entry = registry
        .get(&id)
        .ok_or_else(|| RegistryError::UnknownId(id.clone()))?;
    let allowed = entry
        .allowed_hooks
        .as_deref()
        .ok_or(RunHookError::HookChanged)?;
    let snapshot = paths.allowed_hooks_dir(&id);
    let io = |path: &PathBuf| {
        let path = path.clone();
        move |source| RunHookError::Io { path, source }
    };
    let checkout = hooks_digest(&entry.path).map_err(io(&entry.path))?;
    let copy = snapshot_digest(&snapshot).map_err(io(&snapshot))?;
    if checkout != allowed || copy != allowed {
        return Err(RunHookError::HookChanged);
    }
    let hook = snapshot.join(phase.file_name());
    if !is_executable_file(&hook) {
        return Ok(());
    }
    let source = Command::new(&hook).exec();
    Err(RunHookError::Exec { path: hook, source })
}
