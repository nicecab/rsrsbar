//! `rsrsbar remove`: stop a watcher and forget the repository.

use super::{CommandError, Ctx, Report, locked_entry, remove_file};
use crate::episode::State;
use crate::paths::{RepoId, watch_label};
use crate::registry::Registry;

/// Boots the job out (an unloaded job is fine), enables its label (a paused
/// repository's label is disabled, which would refuse a later `add` of the
/// same id), deletes the plist and the
/// git-rsrs config, unregisters the entry and drops its memory from
/// `state.json`. The status file, the allowed snapshot and the logs stay as
/// history; the report says where. A state file that cannot be updated
/// (e.g. corrupt) is reported as a warning: the repository is already gone.
pub fn remove(ctx: &Ctx, id: &str) -> Result<Report, CommandError> {
    // From the lookup to the state update: a removal must not act on an
    // entry that a concurrent remove + add has since replaced.
    let (_services, entry) = locked_entry(ctx, id)?;
    let id = entry.id;
    let label = watch_label(&id);
    ctx.launchd.bootout(&label)?;
    ctx.launchd.enable(&label)?;
    remove_file(&ctx.paths.launch_agent(&id))?;
    remove_file(&ctx.paths.git_rsrs_config(&id))?;
    Registry::update(&ctx.paths.registry(), |r| r.without(&id))?;
    let mut lines = vec![
        format!("removed {id}"),
        format!(
            "  kept: {} (status, allowed hooks)",
            ctx.paths.repo_dir(&id).display()
        ),
        format!(
            "  kept: {}, {}",
            ctx.paths.stdout_log(&id).display(),
            ctx.paths.stderr_log(&id).display()
        ),
    ];
    if let Err(err) = State::update(&ctx.paths.state(), |s| without_repo(s, &id)) {
        lines.push(format!("warning: cannot update the state file: {err}"));
    }
    Ok(Report::ok(lines.join("\n")))
}

fn without_repo(state: &State, id: &RepoId) -> State {
    State {
        version: state.version,
        repos: state
            .repos
            .iter()
            .filter(|(key, _)| *key != id)
            .map(|(key, memory)| (key.clone(), memory.clone()))
            .collect(),
    }
}
