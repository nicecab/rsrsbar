//! `rsrsbar sync`: one git-rsrs sync now, under rsrsbar's own deadline.

use super::lifecycle::{refuse_paused, registered};
use super::{CommandError, Ctx, Report};
use crate::gitrsrs::{sync_deadline, sync_once};

/// Runs `git-rsrs sync` once for `id` and passes its output and exit code
/// through; past the deadline the error exits 124. A paused repository is
/// refused.
pub fn sync(ctx: &Ctx, id: &str) -> Result<Report, CommandError> {
    let entry = registered(ctx, id)?;
    refuse_paused(&entry)?;
    let deadline = sync_deadline(&entry.settings()?);
    let config = ctx.paths.git_rsrs_config(&entry.id);
    let run = sync_once(&ctx.git_rsrs, &entry.path, &config, deadline)?;
    Ok(Report {
        text: run.output.trim_end().to_owned(),
        exit_code: run.exit_code,
    })
}
