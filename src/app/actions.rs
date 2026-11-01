//! What a menu item does. Everything runs here except "Allow hooks…",
//! whose preview / confirm dialog belongs to the shell.

use std::path::Path;
use std::process::Command;

use crate::child::{DEADLINE, output_within};
use crate::commands::{self, CommandError, Ctx};
use crate::episode::Notice;
use crate::paths::RepoId;
use crate::registry::{AppSettings, RepoEntry};

const OPEN: &str = "/usr/bin/open";
const CONSOLE: &str = "Console";

/// A per-repository menu action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    SyncNow(RepoId),
    OpenLog(RepoId),
    OpenTerminal(RepoId),
    OpenEditor(RepoId),
    Restart(RepoId),
    Pause(RepoId),
    Resume(RepoId),
    AllowHooks(RepoId),
}

/// Performs `action`. A sync that fails or runs past its deadline is not an
/// error but a notice carrying the first line of what went wrong, so the
/// shell posts it like any alert; every other failure is an error.
/// `AllowHooks` is refused: the shell runs that dialog itself.
pub fn perform(
    ctx: &Ctx,
    app: &AppSettings,
    action: &Action,
) -> Result<Option<Notice>, CommandError> {
    match action {
        Action::SyncNow(id) => Ok(sync_now(ctx, id)),
        Action::OpenLog(id) => open(CONSOLE, &ctx.paths.stderr_log(id)),
        Action::OpenTerminal(id) => open(app.terminal_app(), &entry(ctx, id)?.path),
        Action::OpenEditor(id) => {
            let editor = app.editor.as_deref().ok_or_else(|| {
                CommandError::Refused("no editor set in the registry's [app] table".to_owned())
            })?;
            open(editor, &entry(ctx, id)?.path)
        }
        Action::Restart(id) => commands::restart(ctx, id.as_str()).map(|_| None),
        Action::Pause(id) => commands::pause(ctx, id.as_str()).map(|_| None),
        Action::Resume(id) => commands::resume(ctx, id.as_str()).map(|_| None),
        Action::AllowHooks(id) => Err(CommandError::Refused(format!(
            "allowing hooks for {id} needs the confirmation dialog"
        ))),
    }
}

/// `None` when the sync exited 0, else an alert with the first line of
/// git-rsrs's output (or of the error, e.g. the deadline).
fn sync_now(ctx: &Ctx, id: &RepoId) -> Option<Notice> {
    let failure = match commands::sync(ctx, id.as_str()) {
        Ok(report) if report.exit_code == 0 => return None,
        Ok(report) => format!("exit {}: {}", report.exit_code, first_line(&report.text)),
        Err(err) => first_line(&err.to_string()).to_owned(),
    };
    Some(Notice::Alert {
        label: format!("sync now failed — {failure}"),
    })
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

fn entry(ctx: &Ctx, id: &RepoId) -> Result<RepoEntry, CommandError> {
    commands::registered(ctx, id.as_str())
}

/// `open -a <app> <path>`, within the child deadline.
fn open(app: &str, path: &Path) -> Result<Option<Notice>, CommandError> {
    let mut command = Command::new(OPEN);
    command.arg("-a").arg(app).arg(path);
    let output = output_within(&mut command, DEADLINE).map_err(|source| CommandError::Io {
        path: Path::new(OPEN).to_path_buf(),
        source,
    })?;
    if output.status.success() {
        return Ok(None);
    }
    Err(CommandError::Refused(format!(
        "open -a {app} {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
