//! `rsrsbar add`: register a checkout and start its watcher.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::{CommandError, Ctx, Report, install_service, lock_services, remove_file};
use crate::git::{RepoProbe, probe};
use crate::gitrsrs::check_version;
use crate::paths::{RepoId, watch_label};
use crate::registry::{Registry, RepoEntry};
use crate::trust::{hooks_digest, present_hooks};

const AUTOSYNC_DIR: &str = ".autosync";
const DEFAULT_REMOTE: &str = "origin";

/// `rsrsbar add <PATH> [--id] [--branch] [--remote] [--lock-name] [--interval]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddArgs {
    pub path: PathBuf,
    pub id: Option<String>,
    /// Defaults to the branch checked out.
    pub branch: Option<String>,
    /// Defaults to `origin`.
    pub remote: Option<String>,
    pub lock_name: Option<String>,
    pub interval: Option<String>,
}

/// Probes and refuses, then writes the service files, bootstraps the job and
/// registers the entry. Any failure after the files are written removes them
/// (and boots the job out again if it was loaded): no half-registered
/// repository survives an error.
pub fn add(ctx: &Ctx, args: AddArgs) -> Result<Report, CommandError> {
    let found = probe(&args.path)?;
    let (branch, remote) = check_checkout(&found, &args)?;
    check_version(&ctx.git_rsrs)?;
    let id = match &args.id {
        Some(id) => RepoId::new(id)?,
        None => RepoId::suggest(&found.toplevel)?,
    };
    let entry = RepoEntry {
        id,
        path: found.toplevel.clone(),
        branch,
        remote,
        lock_name: args.lock_name,
        interval: args.interval,
        quiet_period: None,
        git_timeout: None,
        lock_timeout: None,
        stall_threshold: None,
        paused_at: None,
        allowed_hooks: initial_allowed_hooks(&found.toplevel)?,
    };
    // From the duplicate check to the registry update, `add` writes into
    // paths it does not own yet.
    let _services = lock_services(ctx)?;
    // Refuse duplicates and bad settings before touching any file;
    // `Registry::update` checks again under its own lock.
    Registry::load(&ctx.paths.registry())?.with_added(entry.clone())?;
    refuse_loaded_label(ctx, &entry.id)?;
    start_and_register(ctx, &entry)?;
    Ok(Report::ok(added_text(ctx, &entry)))
}

/// The branch and remote to register, or the refusal naming the fix.
fn check_checkout(found: &RepoProbe, args: &AddArgs) -> Result<(String, String), CommandError> {
    let path = found.toplevel.display();
    if found.is_linked_worktree {
        return Err(refused(format!(
            "{path} is a linked worktree; register the primary checkout"
        )));
    }
    let Some(current) = &found.branch else {
        let target = args.branch.as_deref().unwrap_or("a branch");
        return Err(refused(format!(
            "{path} has a detached HEAD; check out {target} first"
        )));
    };
    let branch = args.branch.clone().unwrap_or_else(|| current.clone());
    if &branch != current {
        return Err(refused(format!(
            "{path} is on {current}, not {branch}; check out {branch} first"
        )));
    }
    let remote = args
        .remote
        .clone()
        .unwrap_or_else(|| DEFAULT_REMOTE.to_owned());
    if !found.remotes.contains(&remote) {
        return Err(refused(format!(
            "{path} has no remote {remote}; add it with git remote add {remote} <url>"
        )));
    }
    Ok((branch, remote))
}

fn refused(message: String) -> CommandError {
    CommandError::Refused(message)
}

/// No `.autosync/` means nothing to trust: the empty digest is recorded.
/// Otherwise the hooks stay untrusted until `allow`.
fn initial_allowed_hooks(toplevel: &Path) -> Result<Option<String>, CommandError> {
    let root = toplevel.join(AUTOSYNC_DIR);
    match fs::symlink_metadata(&root) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let digest = hooks_digest(toplevel).map_err(super::io_error(&root))?;
            Ok(Some(digest))
        }
        Err(e) => Err(super::io_error(&root)(e)),
        Ok(_) => Ok(None),
    }
}

/// A job loaded under this label but not registered (e.g. left by a crash)
/// owns the service files; writing over them and then rolling back would
/// break it, so nothing is written.
fn refuse_loaded_label(ctx: &Ctx, id: &RepoId) -> Result<(), CommandError> {
    let label = watch_label(id);
    if ctx.launchd.job(&label)?.is_some() {
        return Err(refused(format!(
            "{label} is already loaded but not registered; \
             run launchctl bootout gui/$(id -u)/{label} first"
        )));
    }
    Ok(())
}

/// Install → bootstrap → register, undoing the earlier steps on failure.
fn start_and_register(ctx: &Ctx, entry: &RepoEntry) -> Result<(), CommandError> {
    let label = watch_label(&entry.id);
    let installed = install_service(ctx, entry)
        .and_then(|_| Ok(ctx.launchd.bootstrap(&ctx.paths.launch_agent(&entry.id))?));
    if let Err(err) = installed {
        remove_service_files(ctx, entry);
        return Err(err);
    }
    let registered = Registry::update(&ctx.paths.registry(), |r| r.with_added(entry.clone()));
    if let Err(err) = registered {
        // Best effort: the original error is what the user needs to see.
        let _ = ctx.launchd.bootout(&label);
        remove_service_files(ctx, entry);
        return Err(err.into());
    }
    Ok(())
}

/// Best-effort rollback of `install_service`: the files, and the repository
/// directory if nothing else (such as an older status file) is in it.
fn remove_service_files(ctx: &Ctx, entry: &RepoEntry) {
    let _ = remove_file(&ctx.paths.git_rsrs_config(&entry.id));
    let _ = remove_file(&ctx.paths.launch_agent(&entry.id));
    let _ = fs::remove_dir(ctx.paths.repo_dir(&entry.id));
}

fn added_text(ctx: &Ctx, entry: &RepoEntry) -> String {
    let hooks: Vec<_> = present_hooks(&entry.path)
        .iter()
        .map(|phase| phase.file_name())
        .collect();
    let hooks = if hooks.is_empty() {
        "none".to_owned()
    } else {
        hooks.join(", ")
    };
    let mut lines = vec![
        format!("added {}", entry.id),
        format!(
            "  {} · {} → {}",
            entry.path.display(),
            entry.branch,
            entry.remote
        ),
        format!("  launchd label: {}", watch_label(&entry.id)),
        format!("  hooks: {hooks}"),
    ];
    if entry.allowed_hooks.is_none() {
        lines.push(format!(
            "  hooks are not allowed yet; run rsrsbar allow {}",
            entry.id
        ));
    }
    if under_cargo_target(&ctx.rsrsbar_exe) {
        lines.push(format!(
            "warning: the hooks call {}, inside a Cargo target/ directory; \
             rerun rsrsbar refresh from the installed binary",
            ctx.rsrsbar_exe.display()
        ));
    }
    lines.join("\n")
}

fn under_cargo_target(exe: &Path) -> bool {
    exe.components()
        .any(|c| c == Component::Normal("target".as_ref()))
}
