//! `pause`, `resume`, `refresh` and `restart`: the installed service's
//! lifecycle. Each holds the service lock from its registry lookup to its
//! last launchd action or registry write.

use std::time::SystemTime;

use super::install::install;
use super::{CommandError, Ctx, Report, install_service, lock_services, locked_entry, registered};
use crate::paths::{RepoId, watch_label};
use crate::registry::{Registry, RepoEntry};

/// Refuses a paused repository for commands that need its watcher.
pub(crate) fn refuse_paused(entry: &RepoEntry) -> Result<(), CommandError> {
    match entry.paused_at {
        Some(_) => Err(CommandError::Refused(format!(
            "{} is paused; resume it first",
            entry.id
        ))),
        None => Ok(()),
    }
}

fn set_paused_at(ctx: &Ctx, id: &RepoId, paused_at: Option<String>) -> Result<(), CommandError> {
    Registry::update(&ctx.paths.registry(), |r| {
        r.with_entry(id, |e| RepoEntry {
            paused_at: paused_at.clone(),
            ..e.clone()
        })
    })?;
    Ok(())
}

fn rfc3339(time: SystemTime) -> String {
    humantime::format_rfc3339_seconds(time).to_string()
}

/// Stops the watcher (`bootout`; an unloaded job is fine), records
/// `paused_at = now`, then disables the label: the plist stays in
/// `~/Library/LaunchAgents`, and launchd would load it again at the next
/// login. Already paused → a note, exit 0, and the disable is repeated in
/// case it failed the first time.
pub fn pause(ctx: &Ctx, id: &str) -> Result<Report, CommandError> {
    let (_services, entry) = locked_entry(ctx, id)?;
    let label = watch_label(&entry.id);
    if let Some(since) = &entry.paused_at {
        ctx.launchd.disable(&label)?;
        return Ok(Report::ok(format!(
            "{} is already paused since {since}",
            entry.id
        )));
    }
    ctx.launchd.bootout(&label)?;
    set_paused_at(ctx, &entry.id, Some(rfc3339(ctx.now)))?;
    ctx.launchd.disable(&label)?;
    Ok(Report::ok(format!(
        "paused {0}; its watcher stays stopped until rsrsbar resume {0}",
        entry.id
    )))
}

/// Rewrites the service files, enables the label `pause` disabled,
/// bootstraps the watcher and only then clears `paused_at`: a failed
/// bootstrap (e.g. the label was loaded by hand) leaves the entry paused.
/// Not paused → a note, exit 0.
pub fn resume(ctx: &Ctx, id: &str) -> Result<Report, CommandError> {
    let (_services, entry) = locked_entry(ctx, id)?;
    if entry.paused_at.is_none() {
        return Ok(Report::ok(format!("{} is not paused", entry.id)));
    }
    install_service(ctx, &entry)?;
    let label = watch_label(&entry.id);
    ctx.launchd.enable(&label)?;
    if let Err(err) = ctx.launchd.bootstrap(&ctx.paths.launch_agent(&entry.id)) {
        return Err(CommandError::Refused(format!(
            "cannot start {label}: {err}; {} stays paused. If the job is loaded \
             already, run launchctl bootout gui/$(id -u)/{label} and resume again",
            entry.id
        )));
    }
    set_paused_at(ctx, &entry.id, None)?;
    Ok(Report::ok(format!("resumed {}", entry.id)))
}

/// Regenerates config and plist for every unpaused repository (or only
/// `id`) and applies them: not loaded → bootstrap; plist changed → bootout
/// and bootstrap (launchd does not re-read a loaded plist); config changed
/// → kickstart (git-rsrs reads its config on start); else nothing. Every
/// repository is attempted and gets one line; the exit code is 1 if any
/// failed. A failed launchd action restores the previous files, so the next
/// refresh sees the change again and retries.
pub fn refresh(ctx: &Ctx, id: Option<&str>) -> Result<Report, CommandError> {
    let _services = lock_services(ctx)?;
    let entries = match id {
        Some(id) => vec![registered(ctx, id)?],
        None => Registry::load(&ctx.paths.registry())?.repos,
    };
    let results: Vec<_> = entries
        .iter()
        .map(|entry| (entry, refresh_one(ctx, entry)))
        .collect();
    let lines: Vec<_> = results
        .iter()
        .map(|(entry, result)| match result {
            Ok(done) => format!("{}: {done}", entry.id),
            Err(err) => format!("{}: failed: {err}", entry.id),
        })
        .collect();
    let failed = results.iter().any(|(_, result)| result.is_err());
    Ok(Report {
        text: lines.join("\n"),
        exit_code: i32::from(failed),
    })
}

fn refresh_one(ctx: &Ctx, entry: &RepoEntry) -> Result<&'static str, CommandError> {
    if entry.paused_at.is_some() {
        return Ok("paused, skipped");
    }
    let installed = install(ctx, entry)?;
    let applied = apply(ctx, &entry.id, installed.change);
    if applied.is_err() {
        installed.undo()?;
    }
    applied
}

/// The launchd action a service change needs; returns what was done.
fn apply(
    ctx: &Ctx,
    id: &RepoId,
    change: super::ServiceChange,
) -> Result<&'static str, CommandError> {
    let label = watch_label(id);
    let plist = ctx.paths.launch_agent(id);
    let loaded = ctx.launchd.job(&label)?.is_some();
    if loaded && change.plist {
        ctx.launchd.bootout(&label)?;
    }
    if !loaded || change.plist {
        ctx.launchd.bootstrap(&plist)?;
        return Ok(if loaded { "reloaded" } else { "started" });
    }
    if change.config {
        ctx.launchd.kickstart(&label)?;
        return Ok("restarted");
    }
    Ok("unchanged")
}

/// Restarts the watcher (`kickstart -k`), the menu's Restart watcher.
pub fn restart(ctx: &Ctx, id: &str) -> Result<Report, CommandError> {
    let (_services, entry) = locked_entry(ctx, id)?;
    refuse_paused(&entry)?;
    ctx.launchd.kickstart(&watch_label(&entry.id))?;
    Ok(Report::ok(format!("restarted {}", entry.id)))
}
