//! `rsrsbar start` / `rsrsbar stop`: the menu bar app's and the heartbeat
//! job's LaunchAgents, loaded and unloaded in a safe order.

use std::path::Path;

use super::install::{restore, write_if_changed};
use super::{CommandError, Ctx, Report, lock_services};
use crate::bundle::bundle_root;
use crate::paths::{APP_LABEL, HEARTBEAT_LABEL};
use crate::service::{app_plist, heartbeat_plist};

/// Writes both LaunchAgents and loads the app, then the heartbeat job.
/// A job that is already loaded is reloaded (bootout + bootstrap) when its
/// plist changed, since launchd keeps the plist it loaded, and restarted
/// otherwise. Refused unless run from the app bundle: the agents point at
/// the bundle's executable. Holds the service lock throughout, so a
/// concurrent `stop` cannot slip between the two loads.
pub fn start(ctx: &Ctx) -> Result<Report, CommandError> {
    let bundle = bundle_root(&ctx.rsrsbar_exe).ok_or_else(|| {
        CommandError::Refused(
            "run rsrsbar start from rsrsbar.app/Contents/MacOS/rsrsbar".to_owned(),
        )
    })?;
    let _services = lock_services(ctx)?;
    let app = app_plist(&bundle, &ctx.paths)?;
    let heartbeat = heartbeat_plist(&bundle, &ctx.paths)?;
    load(ctx, APP_LABEL, &ctx.paths.app_agent(), &app)?;
    load(
        ctx,
        HEARTBEAT_LABEL,
        &ctx.paths.heartbeat_agent(),
        &heartbeat,
    )?;
    Ok(Report::ok(format!(
        "rsrsbar started from {}",
        bundle.display()
    )))
}

/// Unloads the heartbeat job first — so it never sees the deliberately
/// stopped app as stuck and restarts it — then the app, under the service
/// lock. Watchers are not touched.
pub fn stop(ctx: &Ctx) -> Result<Report, CommandError> {
    let _services = lock_services(ctx)?;
    ctx.launchd.bootout(HEARTBEAT_LABEL)?;
    ctx.launchd.bootout(APP_LABEL)?;
    Ok(Report::ok(
        "rsrsbar stopped; repositories keep syncing".to_owned(),
    ))
}

/// Writes `bytes` to `plist` and leaves `label` loaded from exactly that
/// file. If launchd refuses, the previous plist is put back, so the next
/// `start` sees the change again and retries the reload instead of
/// kickstarting a job still loaded from the old file.
fn load(ctx: &Ctx, label: &str, plist: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    let rewritten = write_if_changed(plist, bytes)?;
    let result = apply(ctx, label, plist, rewritten.is_some());
    if let (Err(_), Some(rewritten)) = (&result, rewritten) {
        restore(&[rewritten])?;
    }
    result
}

/// launchd keeps the plist it loaded: a changed one needs a reload.
fn apply(ctx: &Ctx, label: &str, plist: &Path, changed: bool) -> Result<(), CommandError> {
    match (ctx.launchd.job(label)?.is_some(), changed) {
        (true, false) => ctx.launchd.kickstart(label)?,
        (true, true) => {
            ctx.launchd.bootout(label)?;
            ctx.launchd.bootstrap(plist)?;
        }
        (false, _) => ctx.launchd.bootstrap(plist)?,
    }
    Ok(())
}
