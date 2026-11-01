//! The shell's "Allow hooks…" dialog: show the preview in TextEdit, ask
//! with an `NSAlert`, and allow exactly what was shown.

use std::path::Path;
use std::process::Command;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
use objc2_foundation::{NSString, ns_string};

use super::shell::Config;
use super::shell_workers::post_notice;
use crate::atomic::write_atomic;
use crate::child::{DEADLINE, output_within};
use crate::commands::{self, CommandError};
use crate::episode::Notice;
use crate::launchd::Launchctl;
use crate::paths::RepoId;

const OPEN: &str = "/usr/bin/open";

/// Writes the preview to `<support>/repos/<id>/pending-allow.diff`, opens it
/// in TextEdit, asks "Allow hooks for <id>?" (Allow / Cancel) and on Allow
/// calls `commands::allow` with the previewed digest, so only what was
/// shown is allowed. A failure is posted as a notification.
pub(super) fn allow_hooks(config: &Config, id: &RepoId, mtm: MainThreadMarker) {
    let launchd = Launchctl::current();
    let ctx = config.ctx(&launchd);
    let result = commands::allow_preview(&ctx, id.as_str()).and_then(|preview| {
        let diff = config.paths.repo_dir(id).join("pending-allow.diff");
        write_atomic(&diff, preview.text.as_bytes()).map_err(commands_io(&diff))?;
        open_in_text_edit(&diff)?;
        if confirmed(id, mtm) {
            commands::allow(&ctx, id.as_str(), &preview.digest).map(drop)
        } else {
            Ok(())
        }
    });
    if let Err(err) = result {
        let notice = Notice::Alert {
            label: err.to_string(),
        };
        post_notice(id, &notice);
    }
}

/// The app is activated first so the alert is frontmost.
fn confirmed(id: &RepoId, mtm: MainThreadMarker) -> bool {
    NSApplication::sharedApplication(mtm).activate();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&format!("Allow hooks for {id}?")));
    alert.setInformativeText(ns_string!(
        "The changes since the last allow are open in TextEdit."
    ));
    alert.addButtonWithTitle(ns_string!("Allow"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    alert.runModal() == NSAlertFirstButtonReturn
}

fn open_in_text_edit(path: &Path) -> Result<(), CommandError> {
    let mut command = Command::new(OPEN);
    command.args(["-a", "TextEdit"]).arg(path);
    let output = output_within(&mut command, DEADLINE).map_err(commands_io(Path::new(OPEN)))?;
    if output.status.success() {
        return Ok(());
    }
    Err(CommandError::Refused(format!(
        "cannot open {} in TextEdit: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn commands_io(path: &Path) -> impl FnOnce(std::io::Error) -> CommandError + '_ {
    move |source| CommandError::Io {
        path: path.to_path_buf(),
        source,
    }
}
