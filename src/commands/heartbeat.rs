//! `rsrsbar check-heartbeat`: the independent job that restarts a stuck app
//! and alerts through `osascript` when that does not help.

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use super::{CommandError, Ctx, Report};
use crate::child::{DEADLINE, output_within};
use crate::heartbeat::{Action, AlertKind, Heartbeat, WatchState, decide};
use crate::paths::APP_LABEL;

/// Looked up on `PATH` (`/usr/bin` under launchd).
const OSASCRIPT: &str = "osascript";
/// The alert text arrives as `argv`, never inside the AppleScript source.
const ALERT_SCRIPT: [&str; 3] = [
    "on run argv",
    "display notification (item 1 of argv) with title \"rsrsbar\"",
    "end run",
];

/// Loads the heartbeat and this job's state, decides, performs the actions
/// and always saves the new state, then returns the first action error.
///
/// `awake_since` is `None` when the wake time could not be read; there is
/// then no wake grace, because judging a possibly expected stale heartbeat
/// costs at most one needless restart, while not judging at all would let
/// this last line of defence go quiet.
///
/// A restart counts as made once attempted, even if launchctl refused it:
/// the next run then alerts instead of retrying a restart that keeps
/// failing in silence. An alert counts as sent only if `osascript`
/// succeeded, so a failed one is retried on the next run.
pub fn check_heartbeat(ctx: &Ctx, awake_since: Option<SystemTime>) -> Result<Report, CommandError> {
    let paths = &ctx.paths;
    let heartbeat = Heartbeat::load(&paths.heartbeat());
    let state = WatchState::load(&paths.heartbeat_state());
    // If launchctl cannot even say whether the app is loaded, assume it is:
    // the kickstart then reports launchctl's error and the next run alerts,
    // rather than this job failing before it decides anything.
    let app_loaded = ctx.launchd.job(APP_LABEL).map_or(true, |job| job.is_some());
    let awake = awake_since.unwrap_or(UNIX_EPOCH);
    let (actions, decided) = decide(ctx.now, awake, heartbeat.as_ref(), app_loaded, &state);
    let results: Vec<_> = actions
        .iter()
        .map(|action| (*action, perform(ctx, *action)))
        .collect();
    let saved = results
        .iter()
        .fold(decided, |next, (action, result)| match (action, result) {
            (Action::Alert(kind), Err(_)) => unrecorded(&next, &state, *kind),
            _ => next,
        });
    let path = paths.heartbeat_state();
    let save = if saved == state {
        Ok(())
    } else {
        saved.save(&path).map_err(super::io_error(&path))
    };
    let lines = results
        .into_iter()
        .map(|(_, result)| result)
        .collect::<Result<Vec<_>, _>>()?;
    save?;
    let unknown_wake = awake_since
        .is_none()
        .then(|| "wake time unknown; judged without wake grace".to_owned());
    let lines: Vec<_> = unknown_wake
        .into_iter()
        .chain(lines.into_iter().flatten())
        .collect();
    Ok(Report::ok(lines.join("\n")))
}

/// `next` with its record of `kind` put back to what `previous` had.
fn unrecorded(next: &WatchState, previous: &WatchState, kind: AlertKind) -> WatchState {
    let others = next.alerted.iter().filter(|(k, _)| **k != kind);
    let before = previous.alerted.get_key_value(&kind);
    WatchState {
        alerted: others.chain(before).map(|(k, at)| (*k, *at)).collect(),
        ..next.clone()
    }
}

/// Performs one action; the line it reports, if any.
fn perform(ctx: &Ctx, action: Action) -> Result<Option<String>, CommandError> {
    match action {
        Action::Nothing => Ok(None),
        Action::Kickstart => {
            ctx.launchd.kickstart(APP_LABEL)?;
            Ok(Some(format!("heartbeat stale; restarted {APP_LABEL}")))
        }
        Action::Bootstrap => {
            ctx.launchd.bootstrap(&ctx.paths.app_agent())?;
            Ok(Some(format!("heartbeat stale; loaded {APP_LABEL}")))
        }
        Action::Alert(kind) => {
            alert(kind)?;
            Ok(Some(format!("alerted: {}", kind.text())))
        }
    }
}

/// `osascript -e <source>… -- <text>`, within the child deadline.
fn alert(kind: AlertKind) -> Result<(), CommandError> {
    let mut command = Command::new(OSASCRIPT);
    for line in ALERT_SCRIPT {
        command.arg("-e").arg(line);
    }
    command.arg("--").arg(kind.text());
    let output = output_within(&mut command, DEADLINE).map_err(|source| CommandError::Io {
        path: PathBuf::from(OSASCRIPT),
        source,
    })?;
    if output.status.success() {
        return Ok(());
    }
    Err(CommandError::Refused(format!(
        "{OSASCRIPT} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
