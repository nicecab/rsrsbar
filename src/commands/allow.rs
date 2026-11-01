//! `rsrsbar allow`: trust the previewed `.autosync/` by snapshotting it.

use std::io;
use std::path::Path;

use super::{CommandError, Ctx, Report, hooks_text, io_error, locked_entry};
use crate::episode::State;
use crate::paths::RepoId;
use crate::registry::{Registry, RepoEntry};
use crate::status::{StatusRead, read_status};
use crate::trust::snapshot_hooks_matching;

/// A tree that cannot be snapshotted safely (InvalidInput from
/// `snapshot_hooks`) is a refusal naming the fix; other errors are I/O.
pub(crate) fn snapshot_refusal(err: io::Error, id: &RepoId, dest: &Path) -> CommandError {
    match err.kind() {
        io::ErrorKind::InvalidInput => CommandError::Refused(format!(
            "{err}; nothing was allowed — fix .autosync/ and run rsrsbar allow {id} again"
        )),
        _ => io_error(dest)(err),
    }
}

/// Stages a snapshot of `.autosync/` and, only if it still has `expected`
/// — the digest of what the preview showed — swaps it in as the allowed
/// snapshot, records the digest and moves the recovery point to the current
/// attempt (`RepoMemory::after_allow`). On a mismatch nothing changes: the
/// previous snapshot and the recorded digest stay as they were. A state
/// file that cannot be updated is a warning: the hooks are allowed either
/// way.
pub fn allow(ctx: &Ctx, id: &str, expected: &str) -> Result<Report, CommandError> {
    let (_services, entry) = locked_entry(ctx, id)?;
    let dest = ctx.paths.allowed_hooks_dir(&entry.id);
    let matched = snapshot_hooks_matching(&entry.path, &dest, expected)
        .map_err(|err| snapshot_refusal(err, &entry.id, &dest))?;
    if !matched {
        return Err(CommandError::Refused(format!(
            "hooks changed while waiting for confirmation; run rsrsbar allow {} again",
            entry.id
        )));
    }
    let digest = expected.to_owned();
    Registry::update(&ctx.paths.registry(), |r| {
        r.with_entry(&entry.id, |e| RepoEntry {
            allowed_hooks: Some(digest.clone()),
            ..e.clone()
        })
    })?;
    let moved = move_recovery_point(ctx, &entry.id);
    let mut lines = vec![format!(
        "allowed hooks for {}: {}",
        entry.id,
        hooks_text(&entry.path)
    )];
    if let Err(err) = moved {
        lines.push(format!("warning: cannot update the state file: {err}"));
    }
    Ok(Report::ok(lines.join("\n")))
}

fn move_recovery_point(ctx: &Ctx, id: &RepoId) -> Result<State, CommandError> {
    let status = match read_status(&ctx.paths.status_file(id)) {
        StatusRead::Present(status) => Some(status),
        StatusRead::Missing | StatusRead::Unreadable(_) => None,
    };
    Ok(State::update(&ctx.paths.state(), |state| {
        let memory = state.repos.get(id).cloned().unwrap_or_default();
        state.with_repo(id, memory.after_allow(status.as_ref()))
    })?)
}
